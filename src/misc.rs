pub mod metrics;
pub mod schemas;

use axum::response::IntoResponse;
use http::StatusCode;
use hyper_rustls::ConfigBuilderExt;
use std::fmt;
use std::sync::Arc;

pub fn tls_config() -> Result<rustls::ClientConfig, rustls::Error> {
    Ok(rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_webpki_roots()
    .with_no_client_auth())
}

pub fn map_err<M>(code: StatusCode) -> impl FnOnce(M) -> axum::response::Response
where
    M: fmt::Display,
{
    move |message: M| {
        (
            code,
            axum::Json(schemas::Error {
                code,
                message: message.to_string(),
            }),
        )
            .into_response()
    }
}
