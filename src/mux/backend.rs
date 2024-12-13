use bytes::Bytes;
use futures::{FutureExt, TryFutureExt};
use http::Uri;
use http_body_util::{BodyExt, Full};
use serde::Deserialize;
use std::convert::Infallible;
use std::future::Future;
use std::net::IpAddr;
use std::str::{self, FromStr};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use std::{fmt, future};
use tower::ServiceExt;
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
            parts
                .path_and_query
                .take()
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

type Error = Box<dyn std::error::Error + Send + Sync + 'static>;
impl Backend {
    pub(super) fn models(&self) -> &[super::Model] {
        &self.models
    }

    pub(super) fn request(
        &self,
        request: http::Request<Full<Bytes>>,
    ) -> impl Future<Output = Result<http::Response<hyper::body::Incoming>, Error>> + use<'_> {
        self.service().oneshot(request)
    }

    fn v1_models(
        &self,
    ) -> impl Future<Output = Result<super::List<super::Model>, Error>> + Send + use<'_> {
        fn collect_body<Fut, B, E>(f: Fut) -> impl Future<Output = Result<http::Response<Bytes>, E>>
        where
            Fut: Future<Output = Result<http::Response<B>, E>>,
            B: http_body::Body,
            E: From<B::Error>,
        {
            f.and_then(|response| {
                let (parts, body) = response.into_parts();
                body.collect()
                    .map_ok(|body| http::Response::from_parts(parts, body.to_bytes()))
                    .err_into()
            })
        }

        let service = tower::ServiceBuilder::new()
            .layer(tower_http::timeout::TimeoutLayer::new(
                self.config.timeout.unwrap_or(Duration::MAX),
            ))
            .layer(tower_http::follow_redirect::FollowRedirectLayer::new())
            .map_future(collect_body)
            .service(self.service());

        // https://github.com/rust-lang/rust/issues/102211
        future::ready(http::Request::get("/v1/models").body(Full::default()))
            .err_into()
            .and_then(move |request| service.oneshot(request))
            .map(|response| {
                let (parts, body) = response?.into_parts();
                if parts.status.is_success() {
                    Ok(serde_json::from_slice(&body)?)
                } else {
                    Err(format!("status = {:?}, body = {:?}", parts.status, body).into())
                }
            })
            .inspect_err(|e: &Error| tracing::warn!(error = e.to_string()))
            .instrument(tracing::info_span!("v1_models", ?self))
    }

    fn service(
        &self,
    ) -> impl Clone
           + tower::Service<
        http::Request<Full<Bytes>>,
        Response = http::Response<hyper::body::Incoming>,
        Error = Error,
        Future: Send + 'static,
    > + use<'_> {
        self.pool
            .service(&super::client::Options {
                ip: self.ip,
                http2_only: self.config.http2_only,
            })
            .filter({
                move |request: http::Request<Full<Bytes>>| {
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
                    Ok::<_, http::uri::InvalidUriParts>(http::Request::from_parts(parts, body))
                }
            })
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
