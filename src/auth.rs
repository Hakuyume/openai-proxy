use axum::Router;
use bytes::Bytes;
use http_body_util::Full;
use serde::Deserialize;
use std::env;

#[derive(Clone, Debug, Deserialize)]
pub(super) struct Config {
    inner: Box<crate::Config>,
    api_key: String,
}

pub(super) fn service(
    pool: &crate::misc::pool::Pool<Full<Bytes>>,
    config: Config,
) -> anyhow::Result<Router> {
    let api_key = env::var(config.api_key)?;
    let service = crate::service(pool, *config.inner)?
        .layer(tower_http::validate_request::ValidateRequestHeaderLayer::bearer(&api_key));
    Ok(service)
}
