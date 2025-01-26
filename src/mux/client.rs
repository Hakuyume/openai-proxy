use futures::FutureExt;
use http::{Request, Response, Uri};
use hyper_rustls::ConfigBuilderExt;
use std::convert::Infallible;
use std::iter;
use std::net::IpAddr;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

#[derive(Clone)]
pub(super) struct Pool<B> {
    tls_config: rustls::ClientConfig,
    cache: Arc<Mutex<lru::LruCache<Options, Service<B>>>>,
}

#[derive(Clone, Eq, Hash, PartialEq)]
pub(super) struct Options {
    pub(super) ip: IpAddr,
    pub(super) http2_only: bool,
}

type Service<B> = tower::util::BoxCloneService<
    Request<B>,
    Response<hyper::body::Incoming>,
    hyper_util::client::legacy::Error,
>;

impl<B> Pool<B>
where
    B: http_body::Body + Send + Unpin + 'static,
    B::Data: Send,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    const CACHE_CAP: NonZeroUsize = NonZeroUsize::new(u16::MAX as _).unwrap();

    pub(super) fn new() -> Result<Self, rustls::Error> {
        let tls_config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()?
        .with_webpki_roots()
        .with_no_client_auth();

        Ok(Self {
            tls_config,
            cache: Arc::new(Mutex::new(lru::LruCache::new(Self::CACHE_CAP))),
        })
    }

    pub(super) fn service(&self, options: &Options) -> Service<B> {
        let mut cache = self.cache.lock().unwrap_or_else(|mut e| {
            **e.get_mut() = lru::LruCache::new(Self::CACHE_CAP);
            self.cache.clear_poison();
            e.into_inner()
        });
        cache
            .get_or_insert(options.clone(), || self.build_service(options))
            .clone()
    }

    fn build_service(&self, options: &Options) -> Service<B> {
        let mut connector =
            hyper_util::client::legacy::connect::HttpConnector::new_with_resolver({
                let f = futures::future::ok::<_, Infallible>(iter::once((options.ip, 0).into()));
                tower::service_fn(move |_| f.clone())
            });
        connector.enforce_http(false);

        let connector = tower::service_fn({
            let options = options.clone();
            move |uri: Uri| {
                let mut labels = Vec::new();
                labels.push((&"options.ip", &options.ip.to_string()).into());
                labels.push((&"options.http2_only", &options.http2_only.to_string()).into());
                if let Some(scheme) = uri.scheme_str() {
                    labels.push((&"uri.scheme", &scheme.to_owned()).into());
                }
                if let Some(host) = uri.host() {
                    labels.push((&"uri.host", &host.to_owned()).into());
                }
                if let Some(port) = uri.port_u16() {
                    labels.push((&"uri.port", &port.to_string()).into());
                }
                connector.clone().oneshot(uri).map(|output| match output {
                    Ok(io) => {
                        metrics::counter!("client_connect", labels.clone()).increment(1);
                        Ok(hyper_inspect_io::Io::new(
                            io,
                            super::metrics::HyperIo::new("client_", labels),
                        ))
                    }
                    Err(e) => {
                        labels.extend(super::metrics::error_label(&e));
                        metrics::counter!("client_connect_error", labels).increment(1);
                        Err(e)
                    }
                })
            }
        });

        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(self.tls_config.clone())
            .https_or_http()
            .enable_http1()
            .enable_http2()
            .wrap_connector(connector);
        hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
            .http2_only(options.http2_only)
            .build(connector)
            .boxed_clone()
    }
}
