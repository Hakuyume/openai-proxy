use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use serde::Deserialize;
use std::convert::Infallible;
use std::fmt;
use std::future::Future;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tracing::Instrument;
use url::Url;

#[derive(Clone)]
pub(super) struct Config {
    pub(super) uri: Url,
    pub(super) http2_only: bool,
    pub(super) interval: Duration,
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("uri", &self.uri.as_str())
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
        }

        let mut uri = s.parse::<Url>().map_err(|e| e.to_string())?;
        let Query {
            http2_only,
            interval,
        } = serde_urlencoded::from_str(uri.query().unwrap_or_default())
            .map_err(|e| e.to_string())?;
        uri.set_query(None);
        uri.set_fragment(None);

        Ok(Self {
            uri,
            http2_only,
            interval,
        })
    }
}

pub(super) struct Backend {
    client: super::client::Client<Full<Bytes>>,
    config: Config,
    ip: IpAddr,
    models: Vec<super::Model>,
}

impl fmt::Debug for Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Backend")
            .field("uri", &self.config.uri.as_str())
            .field("ip", &self.ip)
            .finish()
    }
}

pub(super) type Backends = Arc<RwLock<Arc<[Backend]>>>;
pub(super) fn watch(
    resolver: hickory_resolver::TokioAsyncResolver,
    client: super::client::Client<Full<Bytes>>,
    config: Config,
) -> (impl Future<Output = Infallible>, Backends) {
    let backends = Backends::default();
    let f = {
        let span = tracing::info_span!("watch", uri = config.uri.as_str());
        let backends = backends.clone();
        let mut interval = tokio::time::interval(config.interval);
        async move {
            loop {
                interval.tick().await;
                let lookup_ip = {
                    if let Some(host) = config.uri.host_str() {
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
                            client: client.clone(),
                            config: config.clone(),
                            ip,
                            models: Vec::new(),
                        })
                        .collect::<Box<[_]>>();
                    futures::future::join_all(backends.iter_mut().map(Backend::fetch_models)).await;
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

impl Backend {
    pub(super) fn models(&self) -> &[super::Model] {
        &self.models
    }

    pub(super) async fn request(
        &self,
        request: http::Request<Full<Bytes>>,
    ) -> anyhow::Result<http::Response<hyper::body::Incoming>> {
        const HEADER_DENYLIST: &[http::HeaderName] = &[
            http::header::AUTHORIZATION,
            http::header::CONNECTION,
            http::header::HOST,
            http::header::UPGRADE,
        ];

        let request = {
            let (parts, body) = request.into_parts();
            let mut uri = self.config.uri.clone();
            if let Ok(mut path_segments) = uri.path_segments_mut() {
                path_segments
                    .pop_if_empty()
                    .extend(parts.uri.path().split('/').skip(1));
            }
            uri.set_query(parts.uri.query());
            let builder = http::Request::builder()
                .method(parts.method)
                .uri(uri.as_str());
            let builder = parts
                .headers
                .into_iter()
                .filter_map(|(name, value)| {
                    let name = name?;
                    (!HEADER_DENYLIST.contains(&name)).then_some((name, value))
                })
                .fold(builder, |builder, (name, value)| {
                    builder.header(name, value)
                });
            builder.body(body)?
        };
        tracing::info!(method = ?request.method(), uri = ?request.uri(), headers = ?request.headers());

        let response = self
            .client
            .request(
                request,
                &super::client::Options {
                    ip: self.ip,
                    http2_only: self.config.http2_only,
                },
            )
            .await?;

        let response = {
            let (parts, body) = response.into_parts();
            let builder = http::Response::builder().status(parts.status);
            let builder = parts
                .headers
                .into_iter()
                .filter_map(|(name, value)| {
                    let name = name?;
                    (!HEADER_DENYLIST.contains(&name)).then_some((name, value))
                })
                .fold(builder, |builder, (name, value)| {
                    builder.header(name, value)
                });
            builder.body(body)?
        };

        Ok(response)
    }

    #[tracing::instrument(err(level = tracing::Level::WARN))]
    async fn fetch_models(&mut self) -> anyhow::Result<()> {
        let request = http::Request::get("/v1/models").body(Full::default())?;
        let response = self.request(request).await?;

        let (parts, body) = response.into_parts();
        let body = body.collect().await?.to_bytes();
        if parts.status.is_success() {
            self.models = serde_json::from_slice::<super::List<_>>(&body)?.data;
            Ok(())
        } else {
            Err(anyhow::format_err!(
                "status = {:?}, body = {:?}",
                parts.status,
                body
            ))
        }
    }
}
