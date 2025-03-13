use axum::response::IntoResponse;
use axum::{Json, Router, extract, routing};
use bytes::Bytes;
use clap::{ArgAction, Parser};
use futures::TryFutureExt;
use http::{HeaderValue, Request, Response, StatusCode};
use http_body_util::Full;
use serde::Deserialize;
use std::env;
use std::fmt;
use std::future;
use std::net::SocketAddr;
use tower::ServiceExt;
use tower::util::BoxCloneSyncService;

#[derive(Parser)]
pub(super) struct Args {
    #[clap(long)]
    bind: SocketAddr,
    #[clap(long)]
    resource: String,
    #[clap(long, action = ArgAction::Append)]
    deployment: Vec<String>,
    #[clap(long, default_value = "2024-10-21")]
    api_version: String,
}

pub(super) async fn main(args: Args) -> anyhow::Result<()> {
    let prometheus_handle = crate::misc::metrics::install()?;

    let mut connector = hyper_util::client::legacy::connect::HttpConnector::new();
    connector.enforce_http(false);
    let connector = crate::misc::metrics::wrap_connector(connector, |_, _| ());
    let connector = hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config(crate::misc::tls_config()?)
        .https_or_http()
        .enable_http1()
        .enable_http2()
        .wrap_connector(connector);
    let service = hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
        .build(connector);

    let mut api_key = env::var("AZURE_OPENAI_API_KEY")?.parse::<HeaderValue>()?;
    api_key.set_sensitive(true);

    let state = State {
        service: BoxCloneSyncService::new(service),
        resource: args.resource,
        api_version: args.api_version,
        api_key,
    };

    let app = Router::new()
        .route("/v1/models", routing::get(v1_models))
        .with_state(args.deployment)
        .route("/v1/chat/completions", routing::post(tunnel))
        .with_state((state.clone(), "chat/completions"))
        .route("/v1/completions", routing::post(tunnel))
        .with_state((state.clone(), "completions"))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .route("/health", routing::get(|| future::ready(())))
        .route(
            "/metrics",
            routing::get(move || future::ready(prometheus_handle.render())),
        );

    crate::misc::metrics::serve(app, args.bind).await?;
    Ok(())
}

#[derive(Clone)]
struct State {
    service: BoxCloneSyncService<
        Request<Full<Bytes>>,
        Response<hyper::body::Incoming>,
        hyper_util::client::legacy::Error,
    >,
    resource: String,
    api_version: String,
    api_key: HeaderValue,
}

async fn v1_models(
    extract::State(deployment): extract::State<Vec<String>>,
) -> Json<crate::misc::schemas::List<crate::misc::schemas::Model>> {
    let models = deployment
        .into_iter()
        .map(|id| crate::misc::schemas::Model {
            id,
            extra: serde_json::Map::new(),
        })
        .collect();
    Json(crate::misc::schemas::List { data: models })
}

async fn tunnel(
    extract::State((state, path)): extract::State<(State, &'static str)>,
    mut parts: http::request::Parts,
    body: Bytes,
) -> Result<Response<hyper::body::Incoming>, axum::response::Response> {
    fn error<M>(code: StatusCode, message: M) -> axum::response::Response
    where
        M: fmt::Display,
    {
        (
            code,
            Json(crate::misc::schemas::Error {
                code,
                message: message.to_string(),
            }),
        )
            .into_response()
    }

    let model = {
        #[derive(Deserialize)]
        struct Body {
            model: String,
        }

        serde_json::from_slice::<Body>(&body)
            .map_err(|e| error(StatusCode::BAD_REQUEST, e))?
            .model
    };

    parts.uri = format!(
        "https://{}.openai.azure.com/openai/deployments/{model}/{path}?api-version={}",
        state.resource, state.api_version,
    )
    .parse()
    .map_err(|e| error(StatusCode::BAD_REQUEST, e))?;
    parts.version = http::Version::default();
    for name in [
        http::header::AUTHORIZATION,
        http::header::CONNECTION,
        http::header::HOST,
        http::header::UPGRADE,
    ] {
        while parts.headers.remove(&name).is_some() {}
    }
    parts.headers.insert("api-key", state.api_key);
    tracing::info!(?parts);

    state
        .service
        .oneshot(Request::from_parts(parts, Full::new(body)))
        .map_err(|e| error(StatusCode::BAD_GATEWAY, e))
        .await
}
