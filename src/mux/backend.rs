use bytes::Bytes;
use futures::TryFutureExt;
use http::{Request, Response, Uri};
use http_body_util::Full;
use serde::Deserialize;
use std::convert::Infallible;
use std::fmt;
use std::future::{self, Future};
use std::mem;
use std::net::IpAddr;
use std::str::{self, FromStr};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tower::{BoxError, ServiceBuilder, ServiceExt};
use tracing::Instrument;

#[derive(Clone)]
pub(super) struct Config {
    uri: Uri,
    http2_only: bool,
    interval: Duration,
    timeout: Option<Duration>,
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("uri", &self.uri)
            .field("http2_only", &self.http2_only)
            .field("interval", &self.interval)
            .finish()
    }
}

impl FromStr for Config {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Query {
            #[serde(default)]
            http2_only: bool,
            #[serde(with = "humantime_serde")]
            interval: Duration,
            #[serde(default, with = "humantime_serde")]
            timeout: Option<Duration>,
        }

        let uri = s.parse::<Uri>().map_err(|e| e.to_string())?;
        let mut parts = uri.into_parts();

        let Query {
            http2_only,
            interval,
            timeout,
        } = serde_urlencoded::from_str(
            mem::replace(
                &mut parts.path_and_query,
                Some(http::uri::PathAndQuery::from_static("/")),
            )
            .as_ref()
            .and_then(http::uri::PathAndQuery::query)
            .unwrap_or_default(),
        )
        .map_err(|e| e.to_string())?;

        Ok(Self {
            uri: Uri::from_parts(parts).map_err(|e| e.to_string())?,
            http2_only,
            interval,
            timeout,
        })
    }
}

pub(super) struct Backend {
    pool: super::client::Pool<Full<Bytes>>,
    config: Config,
    ip: IpAddr,
    models: Vec<super::Model>,
}

impl fmt::Debug for Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Backend")
            .field("uri", &self.config.uri)
            .field("ip", &self.ip)
            .finish()
    }
}

impl Backend {
    pub(super) fn models(&self) -> &[super::Model] {
        &self.models
    }

    pub(super) fn send(
        &self,
        request: Request<Full<Bytes>>,
    ) -> impl Future<Output = Result<Response<hyper::body::Incoming>, BoxError>> + use<'_> {
        self.service().oneshot(request)
    }

    fn v1_models(
        &self,
    ) -> impl Future<Output = Result<super::List<super::Model>, BoxError>> + Send + use<'_> {
        let service = ServiceBuilder::new()
            .map_err(|e| match e {
                http_extra::from_json::response::Error::Service(e) => e,
                http_extra::from_json::response::Error::Json(e) => e.into(),
            })
            .layer(http_extra::from_json::response::Layer::default())
            .map_err(|e| match e {
                http_extra::check_status::Error::Service(e) => e,
                http_extra::check_status::Error::Status(e) => e.into(),
            })
            .layer(http_extra::check_status::Layer::default())
            .layer(tower_http::timeout::TimeoutLayer::new(
                self.config.timeout.unwrap_or(Duration::MAX),
            ))
            .map_err(
                |e: http_extra::collect_body::response::Error<_, hyper::Error>| match e {
                    http_extra::collect_body::response::Error::Service(e) => e,
                    http_extra::collect_body::response::Error::Body(e) => e.into(),
                },
            )
            .layer(http_extra::collect_body::response::Layer::default())
            .layer(tower_http::follow_redirect::FollowRedirectLayer::new())
            .service(self.service());

        // https://github.com/rust-lang/rust/issues/102211
        future::ready(Request::get("/v1/models").body(Full::<Bytes>::default()))
            .err_into()
            .and_then(|request| service.oneshot(request))
            .map_ok(Response::into_body)
            .inspect_err(|e| tracing::warn!(error = e.to_string()))
            .instrument(tracing::info_span!("v1_models", ?self))
    }

    fn service(
        &self,
    ) -> impl Clone
           + tower::Service<
        Request<Full<Bytes>>,
        Response = Response<hyper::body::Incoming>,
        Error = BoxError,
        Future: Send + 'static,
    > + use<'_> {
        let service = self.pool.service(&super::client::Options {
            ip: self.ip,
            http2_only: self.config.http2_only,
        });
        ServiceBuilder::new()
            .filter(move |request: Request<Full<Bytes>>| {
                let (mut parts, body) = request.into_parts();
                parts.uri = resolve_uri(self.config.uri.clone(), parts.uri)?;
                parts.version = http::Version::default();
                for name in [
                    http::header::AUTHORIZATION,
                    http::header::CONNECTION,
                    http::header::HOST,
                    http::header::UPGRADE,
                ] {
                    while parts.headers.remove(&name).is_some() {}
                }
                tracing::info!(?parts);
                Ok::<_, http::uri::InvalidUriParts>(Request::from_parts(parts, body))
            })
            .service(service)
    }
}

fn resolve_uri(base: Uri, reference: Uri) -> Result<Uri, http::uri::InvalidUriParts> {
    let mut parts = base.into_parts();
    parts.path_and_query = reference.into_parts().path_and_query;
    Uri::from_parts(parts)
}

pub(super) type Backends = Arc<RwLock<Arc<[Backend]>>>;
pub(super) fn watch(
    resolver: hickory_resolver::TokioAsyncResolver,
    pool: super::client::Pool<Full<Bytes>>,
    config: Config,
) -> (impl Future<Output = Infallible> + Send + 'static, Backends) {
    let backends = Backends::default();
    let f = {
        let span = tracing::info_span!("watch", uri = ?config.uri);
        let backends = backends.clone();
        let mut interval = tokio::time::interval(config.interval);
        async move {
            loop {
                interval.tick().await;
                let lookup_ip = {
                    if let Some(host) = config.uri.host() {
                        match resolver.lookup_ip(host).await {
                            Ok(lookup_ip) => Some(lookup_ip),
                            Err(e) => {
                                tracing::warn!(error = e.to_string());
                                None
                            }
                        }
                    } else {
                        tracing::warn!("missing host");
                        None
                    }
                    .into_iter()
                    .flatten()
                };

                let next = {
                    let mut backends = lookup_ip
                        .map(|ip| Backend {
                            pool: pool.clone(),
                            config: config.clone(),
                            ip,
                            models: Vec::new(),
                        })
                        .collect::<Box<[_]>>();
                    futures::future::join_all(backends.iter_mut().map(|backend| async move {
                        if let Ok(models) = backend.v1_models().await {
                            backend.models = models.data;
                        }
                    }))
                    .await;
                    backends.into()
                };

                match backends.write() {
                    Ok(mut backends) => *backends = next,
                    Err(mut e) => {
                        **e.get_mut() = next;
                        backends.clear_poison();
                    }
                }
            }
        }
        .instrument(span)
    };

    (f, backends)
}
