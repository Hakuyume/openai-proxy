pub mod metrics;
pub mod schemas;

use hyper_rustls::ConfigBuilderExt;
use std::sync::Arc;

pub fn tls_config() -> Result<rustls::ClientConfig, rustls::Error> {
    Ok(rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_webpki_roots()
    .with_no_client_auth())
}
