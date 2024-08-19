use axum::body::Body;
use axum::response::IntoResponse;
use axum::{extract, routing, Json, Router};
use bytes::Bytes;
use clap::Parser;
use futures::TryFutureExt;
use headers::authorization::Bearer;
use headers::{Authorization, HeaderMapExt};
use http::header::HOST;
use http::{request, Request, Response, StatusCode, Uri};
use http_body_util::Full;
use hyper::body::Incoming;
use hyper_util::rt::TokioExecutor;
use serde::{Deserialize, Serialize};
use std::fmt::Display;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::{env, future};
use tokio::signal::unix::{signal, SignalKind};
use tower_http::trace::TraceLayer;

#[derive(Debug, Parser)]
struct Opts {
    #[clap(long)]
    config: Config,
}

#[derive(Clone, Debug, Deserialize)]
struct Config {
    bind: SocketAddr,
    upstreams: Vec<Upstream>,
}

#[derive(Clone, Debug, Deserialize)]
struct Upstream {
    #[serde(with = "http_serde::uri")]
    uri: Uri,
    api_key: Option<ApiKey>,
    models: Vec<Model>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ApiKey {
    Env(String),
}

impl FromStr for Config {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(s).map_err(|e| e.to_string())
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let opts = Opts::parse();
    tracing::info!(?opts);

    let state = Arc::new(State {
        client: hyper_util::client::legacy::Client::builder(TokioExecutor::new())
            .build(hyper_util::client::legacy::connect::HttpConnector::new()),
        endpoints: opts
            .config
            .upstreams
            .into_iter()
            .map(|upstream| {
                Ok((
                    Endpoint {
                        uri: upstream.uri,
                        authorization: match upstream.api_key {
                            Some(ApiKey::Env(key)) => Some(Authorization::bearer(&env::var(key)?)?),
                            None => None,
                        },
                    },
                    upstream.models,
                ))
            })
            .collect::<anyhow::Result<_>>()?,
    });

    let app = Router::new()
        .route("/health", routing::get(|| future::ready(())))
        .route("/v1/models", routing::get(v1_models))
        .route("/v1/chat/completions", routing::post(tunnel))
        .route("/v1/completions", routing::post(tunnel))
        .route("/v1/embeddings", routing::post(tunnel))
        .with_state(state)
        .layer(TraceLayer::new_for_http());

    let listener = tokio::net::TcpListener::bind(opts.config.bind).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown({
            let mut sigterm = signal(SignalKind::terminate())?;
            async move {
                sigterm.recv().await;
            }
        })
        .await?;

    Ok(())
}

type Client = hyper_util::client::legacy::Client<
    hyper_util::client::legacy::connect::HttpConnector,
    Full<Bytes>,
>;

struct State {
    client: Client,
    endpoints: Vec<(Endpoint, Vec<Model>)>,
}

struct Endpoint {
    uri: Uri,
    authorization: Option<Authorization<Bearer>>,
}

impl Endpoint {
    fn map_parts(&self, mut parts: request::Parts) -> Result<request::Parts, http::Error> {
        parts.uri = {
            let mut parts_a = self.uri.clone().into_parts();
            let parts_b = parts.uri.into_parts();
            parts_a.path_and_query = match (parts_a.path_and_query, parts_b.path_and_query) {
                (Some(path_and_query_a), Some(path_and_query_b)) => {
                    Some(format!("{}{}", path_and_query_a.path(), path_and_query_b).parse()?)
                }
                (path_and_query_a, None) => path_and_query_a,
                (None, path_and_query_b) => path_and_query_b,
            };
            Uri::from_parts(parts_a)?
        };
        parts.headers.remove(HOST);
        if let Some(authorization) = self.authorization.clone() {
            parts.headers.typed_insert(authorization);
        }
        Ok(parts)
    }
}

fn error<M>(code: StatusCode, message: M) -> Response<Body>
where
    M: Display,
{
    #[derive(Serialize)]
    #[serde(tag = "object", rename = "error")]
    struct Error {
        #[serde(with = "http_serde::status_code")]
        code: StatusCode,
        message: String,
    }

    tracing::error!(code = ?code, message = message.to_string());
    (
        code,
        Json(Error {
            code,
            message: message.to_string(),
        }),
    )
        .into_response()
}

async fn v1_models(extract::State(state): extract::State<Arc<State>>) -> Json<List<Model>> {
    let models = state
        .endpoints
        .iter()
        .flat_map(|(_, models)| models)
        .cloned()
        .collect();
    Json(List { data: models })
}

async fn tunnel(
    extract::State(state): extract::State<Arc<State>>,
    parts: request::Parts,
    body: Bytes,
) -> Result<Response<Incoming>, Response<Body>> {
    let model = {
        #[derive(Deserialize)]
        struct Payload {
            model: String,
        }

        serde_json::from_slice::<Payload>(&body)
            .map_err(|e| error(StatusCode::BAD_REQUEST, e))?
            .model
    };
    tracing::info!(model);

    let model = &model;
    let (endpoint, _) = state
        .endpoints
        .iter()
        .find(|(_, models)| models.iter().any(|m| &m.id == model))
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "unknown model"))?;

    let parts = endpoint
        .map_parts(parts)
        .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    tracing::info!(uri = ?parts.uri);

    let request = Request::from_parts(parts, Full::new(body));
    let response = state
        .client
        .request(request)
        .map_err(|e| error(StatusCode::BAD_GATEWAY, e))
        .await?;

    Ok(response)
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "object", rename = "list")]
struct List<T> {
    data: Vec<T>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "object", rename = "model")]
struct Model {
    id: String,
    #[serde(flatten)]
    _extra: serde_json::Map<String, serde_json::Value>,
}
