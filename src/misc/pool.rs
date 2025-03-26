use http::{Request, Response};
use hyper_rustls::ConfigBuilderExt;
use std::convert::Infallible;
use std::iter;
use std::net::IpAddr;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use tower::util::BoxCloneSyncService;
use tower::{ServiceBuilder, ServiceExt};

#[derive(Clone)]
pub struct Pool<B> {
    tls_config: rustls::ClientConfig,
    cache: Arc<Mutex<lru::LruCache<Options, Service<B>>>>,
}

#[derive(Clone, Default, Eq, Hash, PartialEq)]
pub struct Options {
    pub ip: Option<IpAddr>,
    pub http2_only: bool,
}

pub type Incoming = tower_http::trace::ResponseBody<
    hyper::body::Incoming,
    tower_http::classify::NeverClassifyEos<tower_http::classify::ServerErrorsFailureClass>,
>;

type Service<B> =
    tower::util::BoxCloneService<Request<B>, Response<Incoming>, hyper_util::client::legacy::Error>;

impl<B> Pool<B>
where
    B: http_body::Body + Send + Unpin + 'static,
    B::Data: Send,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    const CACHE_CAP: NonZeroUsize = NonZeroUsize::new(u16::MAX as _).unwrap();

    pub fn new() -> Result<Self, rustls::Error> {
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

    pub fn service(&self, options: &Options) -> Service<B> {
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
        let connector = if let Some(ip) = options.ip {
            let mut connector =
                hyper_util::client::legacy::connect::HttpConnector::new_with_resolver({
                    let f = futures::future::ok::<_, Infallible>(iter::once((ip, 0).into()));
                    tower::service_fn(move |_| f.clone())
                });
            connector.enforce_http(false);
            BoxCloneSyncService::new(connector)
        } else {
            let mut connector = hyper_util::client::legacy::connect::HttpConnector::new();
            connector.enforce_http(false);
            BoxCloneSyncService::new(connector)
        };

        let Options { ip, http2_only } = *options;
        let connector = crate::misc::metrics::wrap_connector(connector, move |labels, _| {
            if let Some(ip) = ip {
                labels.push((&"options.ip", &ip.to_string()).into());
            }
            labels.push((&"options.http2_only", &http2_only.to_string()).into());
        });

        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(self.tls_config.clone())
            .https_or_http()
            .enable_http1()
            .enable_http2()
            .wrap_connector(connector);

        let service =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .http2_only(options.http2_only)
                .build(connector);

        ServiceBuilder::new()
            .layer(
                tower_http::trace::TraceLayer::new_for_http().make_span_with(
                    tower_http::trace::DefaultMakeSpan::new().include_headers(true),
                ),
            )
            .service(service)
            .boxed_clone()
    }
}
