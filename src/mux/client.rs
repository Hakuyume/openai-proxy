use bytes::Bytes;
use http_body_util::Full;
use hyper_rustls::ConfigBuilderExt;
use std::collections::HashMap;
use std::convert::Infallible;
use std::future;
use std::iter;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;
use std::time::Instant;

#[derive(Clone)]
pub(super) struct Pool {
    timeout: Duration,
    tls_config: rustls::ClientConfig,
    pool: Arc<Mutex<HashMap<PoolKey, (Client, Instant)>>>,
}
type Connector =
    hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector<Resolver>>;
type Client = hyper_util::client::legacy::Client<Connector, Full<Bytes>>;
type PoolKey = (IpAddr, bool);

impl Pool {
    pub(super) fn new(timeout: Duration) -> Result<Self, rustls::Error> {
        let tls_config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()?
        .with_webpki_roots()
        .with_no_client_auth();

        Ok(Self {
            timeout,
            tls_config,
            pool: Arc::default(),
        })
    }

    pub(super) fn get(&self, ip: IpAddr, http2_only: bool) -> Client {
        let mut pool = self.pool.lock().unwrap_or_else(|mut e| {
            **e.get_mut() = HashMap::new();
            self.pool.clear_poison();
            e.into_inner()
        });
        let now = Instant::now();

        let (client, instant) = pool.entry((ip, http2_only)).or_insert_with(|| {
            let mut connector =
                hyper_util::client::legacy::connect::HttpConnector::new_with_resolver(Resolver(ip));
            connector.enforce_http(false);
            let connector = hyper_rustls::HttpsConnectorBuilder::new()
                .with_tls_config(self.tls_config.clone())
                .https_or_http()
                .enable_http1()
                .enable_http2()
                .wrap_connector(connector);
            let client =
                hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                    .http2_only(http2_only)
                    .build(connector);
            (client, now)
        });
        let client = client.clone();
        *instant = now;

        pool.retain(|_, (_, instant)| now.saturating_duration_since(*instant) < self.timeout);

        client
    }
}

#[derive(Clone, Copy)]
pub(super) struct Resolver(IpAddr);
impl tower::Service<hyper_util::client::legacy::connect::dns::Name> for Resolver {
    type Response = iter::Once<SocketAddr>;
    type Error = Infallible;
    type Future = future::Ready<Result<Self::Response, Self::Error>>;
    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
    fn call(&mut self, _: hyper_util::client::legacy::connect::dns::Name) -> Self::Future {
        future::ready(Ok(iter::once((self.0, 0).into())))
    }
}
