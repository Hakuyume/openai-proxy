use hyper_rustls::ConfigBuilderExt;
use std::iter;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use tower::ServiceExt;

pub fn tls_config() -> Result<rustls::ClientConfig, rustls::Error> {
    Ok(rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_webpki_roots()
    .with_no_client_auth())
}

pub type Client<B> = hyper_util::client::legacy::Client<Connector, B>;
type Connector =
    hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector<Resolver>>;
type Resolver = tower::util::BoxCloneSyncService<
    hyper_util::client::legacy::connect::dns::Name,
    Box<dyn Iterator<Item = SocketAddr> + Send>,
    std::io::Error,
>;
pub fn client<B>(
    tls_config: rustls::ClientConfig,
    ip: Option<IpAddr>,
    http2_only: bool,
) -> Client<B>
where
    B: http_body::Body + Send,
    B::Data: Send,
{
    let resolver = if let Some(ip) = ip {
        tower::util::BoxCloneSyncService::new(tower::service_fn(move |_| {
            futures::future::ok(Box::new(iter::once((ip, 0u16).into())) as _)
        }))
    } else {
        tower::util::BoxCloneSyncService::new(
            hyper_util::client::legacy::connect::dns::GaiResolver::new()
                .map_response(|addrs| Box::new(addrs) as _),
        )
    };
    let mut connector =
        hyper_util::client::legacy::connect::HttpConnector::new_with_resolver(resolver);
    connector.enforce_http(false);
    let connector = hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config(tls_config)
        .https_or_http()
        .enable_http1()
        .enable_http2()
        .wrap_connector(connector);
    hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
        .http2_only(http2_only)
        .build(connector)
}
