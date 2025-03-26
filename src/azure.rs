use axum::{Json, Router, extract, routing};
use bytes::Bytes;
use futures::TryFutureExt;
use http::{HeaderValue, Request, Response, StatusCode};
use http_body_util::Full;
use serde::Deserialize;
use std::env;
use tower::ServiceExt;

#[derive(Clone, Debug, Deserialize)]
pub(super) struct Config {
    resource: String,
    api_key: String,
    deployment: String,
    #[serde(default = "default_api_version")]
    api_version: String,
    #[serde(default)]
    models: Vec<crate::misc::schemas::Model>,
}

fn default_api_version() -> String {
    "2024-10-21".to_owned()
}

pub(super) fn service(
    pool: &crate::misc::pool::Pool<Full<Bytes>>,
    config: Config,
) -> anyhow::Result<Router> {
    let mut api_key = env::var(config.api_key)?.parse::<HeaderValue>()?;
    api_key.set_sensitive(true);

    let state = State {
        pool: pool.clone(),
        resource: config.resource,
        deployment: config.deployment,
        api_version: config.api_version,
        api_key,
    };

    let service = Router::new()
        .route("/v1/models", routing::get(v1_models))
        .with_state(config.models)
        .route("/v1/chat/completions", routing::post(tunnel))
        .with_state((state.clone(), "chat/completions"))
        .route("/v1/completions", routing::post(tunnel))
        .with_state((state.clone(), "completions"))
        .route("/v1/embeddings", routing::post(tunnel))
        .with_state((state.clone(), "embeddings"));
    Ok(service)
}

#[derive(Clone)]
struct State {
    pool: crate::misc::pool::Pool<Full<Bytes>>,
    resource: String,
    api_version: String,
    deployment: String,
    api_key: HeaderValue,
}

async fn v1_models(
    extract::State(models): extract::State<Vec<crate::misc::schemas::Model>>,
) -> Json<crate::misc::schemas::List<crate::misc::schemas::Model>> {
    Json(crate::misc::schemas::List { data: models })
}

async fn tunnel(
    extract::State((state, path)): extract::State<(State, &'static str)>,
    mut parts: http::request::Parts,
    body: Bytes,
) -> Result<Response<crate::misc::pool::Incoming>, axum::response::Response> {
    const API_KEY: http::header::HeaderName = http::header::HeaderName::from_static("api-key");

    parts.uri = format!(
        "https://{}.openai.azure.com/openai/deployments/{}/{path}?api-version={}",
        state.resource, state.deployment, state.api_version,
    )
    .parse()
    .map_err(crate::misc::map_err(StatusCode::BAD_REQUEST))?;
    parts.version = http::Version::default();
    for name in [
        http::header::AUTHORIZATION,
        http::header::CONNECTION,
        http::header::HOST,
        API_KEY,
    ] {
        while parts.headers.remove(&name).is_some() {}
    }
    parts.headers.insert(API_KEY, state.api_key);

    state
        .pool
        .service(&crate::misc::pool::Options::default())
        .oneshot(Request::from_parts(parts, Full::new(body)))
        .map_err(crate::misc::map_err(StatusCode::BAD_GATEWAY))
        .await
}
