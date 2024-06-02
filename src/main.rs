use axum::body::Body;
use axum::response::IntoResponse;
use axum::{extract, routing, Json, Router};
use bytes::Bytes;
use futures::TryFutureExt;
use heck::ToShoutySnakeCase;
use http::uri::{self, InvalidUriParts, PathAndQuery};
use http::{request, Request, Response, StatusCode, Uri};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper_util::rt::TokioExecutor;
use serde::{Deserialize, Serialize};
use std::convert;
use std::env;
use std::fmt::Display;
use std::future;
use std::mem;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::RwLock;
use tokio::time;
use tower_http::trace::TraceLayer;
use tracing::Instrument;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let config = serde_json::from_str::<Config>(&env::var(format!(
        "{}_CONFIG",
        env!("CARGO_BIN_NAME").to_shouty_snake_case()
    ))?)?;

    let state = Arc::new(State {
        client: hyper_util::client::legacy::Client::builder(TokioExecutor::new())
            .build(hyper_util::client::legacy::connect::HttpConnector::new()),
        endpoints: config.endpoints,
    });

    tokio::spawn(watch(state.clone()));

    let app = Router::new()
        .route("/v1/health", routing::get(|| future::ready(())))
        .route("/v1/models", routing::get(v1_models))
        .route("/v1/chat/completions", routing::post(tunnel))
        .route("/v1/completions", routing::post(tunnel))
        .route("/v1/embeddings", routing::post(tunnel))
        .with_state(state)
        .layer(TraceLayer::new_for_http());

    let listener = tokio::net::TcpListener::bind((Ipv4Addr::UNSPECIFIED, config.port)).await?;
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

#[derive(Deserialize)]
struct Config {
    port: u16,
    endpoints: Vec<Endpoint>,
}

type Client = hyper_util::client::legacy::Client<
    hyper_util::client::legacy::connect::HttpConnector,
    Full<Bytes>,
>;
struct State {
    client: Client,
    endpoints: Vec<Endpoint>,
}

#[derive(Deserialize)]
struct Endpoint {
    #[serde(with = "http_serde::uri")]
    uri: Uri,
    #[serde(with = "humantime_serde")]
    interval: Duration,
    #[serde(skip)]
    models: RwLock<Option<Vec<Model>>>,
}

impl Endpoint {
    fn uri(&self, path_and_query: Option<PathAndQuery>) -> Result<Uri, InvalidUriParts> {
        let uri::Parts {
            scheme, authority, ..
        } = self.uri.clone().into_parts();
        let mut parts = uri::Parts::default();
        parts.scheme = scheme;
        parts.authority = authority;
        parts.path_and_query = path_and_query;
        Uri::from_parts(parts)
    }
}

async fn watch(state: Arc<State>) {
    async fn v1_models(client: &Client, endpoint: &Endpoint) -> anyhow::Result<Vec<Model>> {
        let request =
            Request::get(endpoint.uri(Some("/v1/models".parse()?))?).body(Full::default())?;
        let response = client.request(request).await?;
        let (parts, body) = response.into_parts();
        anyhow::ensure!(parts.status.is_success(), "status = {:?}", parts.status);
        let body = body.collect().await?.to_bytes();
        let List { data } = serde_json::from_slice(&body)?;
        Ok(data)
    }

    futures::future::join_all(state.endpoints.iter().map(|endpoint| {
        async {
            let mut interval = time::interval(endpoint.interval);
            loop {
                interval.tick().await;
                let models = v1_models(&state.client, endpoint)
                    .inspect_ok(|models| tracing::info!(?models))
                    .inspect_err(|e| tracing::error!(error = e.to_string()))
                    .await;
                *endpoint.models.write().await = models.ok();
            }
        }
        .instrument(tracing::info_span!(
            "watch",
            "endpoint.uri" = ?endpoint.uri,
        ))
    }))
    .await;
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
    let data = futures::future::join_all(
        state
            .endpoints
            .iter()
            .map(|endpoint| async { endpoint.models.read().await.clone() }),
    )
    .await
    .into_iter()
    .flatten()
    .flatten()
    .collect();
    Json(List { data })
}

async fn tunnel(
    extract::State(state): extract::State<Arc<State>>,
    mut parts: request::Parts,
    body: Bytes,
) -> Result<Response<Incoming>, Response<Body>> {
    let model = {
        #[derive(Deserialize)]
        struct Request {
            model: String,
        }

        serde_json::from_slice::<Request>(&body)
            .map_err(|e| error(StatusCode::BAD_REQUEST, e))?
            .model
    };
    tracing::info!(model);

    let endpoint = futures::future::join_all(state.endpoints.iter().map(|endpoint| {
        let model = &model;
        async move {
            endpoint
                .models
                .read()
                .await
                .iter()
                .flatten()
                .any(|m| &m.id == model)
                .then_some(endpoint)
        }
    }))
    .await
    .into_iter()
    .find_map(convert::identity)
    .ok_or_else(|| error(StatusCode::BAD_REQUEST, "no such model"))?;

    parts.uri = endpoint
        .uri(mem::take(&mut parts.uri).into_parts().path_and_query)
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
