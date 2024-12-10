use hyper_rustls::ConfigBuilderExt;
use std::convert::Infallible;
use std::iter;
use std::net::IpAddr;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

#[derive(Clone)]
pub(super) struct Client<B> {
    tls_config: rustls::ClientConfig,
    cache: Arc<Mutex<lru::LruCache<Options, Service<B>>>>,
}

#[derive(Clone, Eq, Hash, PartialEq)]
pub(super) struct Options {
    pub(super) ip: IpAddr,
    pub(super) http2_only: bool,
}

type Service<B> = tower::util::BoxCloneService<
    http::Request<B>,
    http::Response<hyper::body::Incoming>,
    hyper_util::client::legacy::Error,
>;

impl<B> Client<B>
where
    B: Default + http_body::Body + Send + Unpin + 'static,
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

    pub(super) fn request(
        &self,
        request: http::Request<B>,
        options: &Options,
    ) -> tower::util::Oneshot<Service<B>, http::Request<B>> {
        let mut cache = self.cache.lock().unwrap_or_else(|mut e| {
            **e.get_mut() = lru::LruCache::new(Self::CACHE_CAP);
            self.cache.clear_poison();
            e.into_inner()
        });
        let service = cache
            .get_or_insert(options.clone(), || self.service(options))
            .clone();
        service.oneshot(request)
    }

    fn service(&self, options: &Options) -> Service<B> {
        let mut connector =
            hyper_util::client::legacy::connect::HttpConnector::new_with_resolver({
                let f = futures::future::ok::<_, Infallible>(iter::once((options.ip, 0).into()));
                tower::service_fn(move |_| f.clone())
            });
        connector.enforce_http(false);
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

        let service = tower::ServiceBuilder::new()
            .layer(tower_http::follow_redirect::FollowRedirectLayer::new())
            .service(service);

        service.boxed_clone()
    }
}
