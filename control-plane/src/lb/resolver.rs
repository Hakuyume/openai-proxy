use futures::{FutureExt, Stream, TryFutureExt};
use rand::{Rng, SeedableRng};
use serde::Deserialize;
use std::net::IpAddr;
use std::time::{Duration, Instant};
use tracing_futures::Instrument;

#[derive(Clone, Debug)]
pub(super) struct Upstream {
    pub(super) uri: http::Uri,
    pub(super) http2_only: bool,
    pub(super) interval: Duration,
    pub(super) timeout: Option<Duration>,
}

impl std::str::FromStr for Upstream {
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

        let uri = s.parse::<http::Uri>().map_err(|e| e.to_string())?;

        let Query {
            http2_only,
            interval,
            timeout,
        } = serde_urlencoded::from_str(uri.query().unwrap_or_default())
            .map_err(|e| e.to_string())?;

        let mut parts = uri.into_parts();
        parts.path_and_query = Some("/".parse().unwrap());
        Ok(Self {
            uri: http::Uri::from_parts(parts).unwrap(),
            http2_only,
            interval,
            timeout,
        })
    }
}

pub(super) struct Resolver {
    tls_config: rustls::ClientConfig,
    resolver: hickory_resolver::TokioResolver,
}

pub(super) struct Endpoint {
    pub(super) ip: IpAddr,
    pub(super) models: Vec<schemas::Model>,
}

impl Resolver {
    pub(super) fn new() -> anyhow::Result<Self> {
        let tls_config = misc::hyper::tls_config()?;
        let resolver = {
            let (config, mut opts) = hickory_resolver::system_conf::read_system_conf()?;
            opts.ip_strategy = hickory_resolver::config::LookupIpStrategy::Ipv4AndIpv6;
            opts.cache_size = 0;
            opts.try_tcp_on_error = true;
            hickory_resolver::TokioResolver::builder_with_config(
                config,
                hickory_resolver::name_server::TokioConnectionProvider::default(),
            )
            .with_options(opts)
            .build()
        };

        Ok(Self {
            tls_config,
            resolver,
        })
    }

    pub(super) fn watch<'a>(
        &'a self,
        upstream: &'a Upstream,
    ) -> impl Stream<Item = Vec<Endpoint>> + Send + 'a {
        futures::stream::unfold(
            (rand::rngs::StdRng::from_os_rng(), Instant::now()),
            move |(mut rng, mut instant)| async move {
                tokio::time::sleep_until(instant.into()).await;
                let now = Instant::now();
                while instant <= now {
                    instant +=
                        rng.random_range(upstream.interval * 4 / 5..=upstream.interval * 6 / 5);
                }
                let lookup_ip = {
                    if let Some(host) = upstream.uri.host() {
                        match self.resolver.lookup_ip(host).await {
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
                let endpoints = futures::future::join_all(lookup_ip.map(|ip| {
                    self.list_models(upstream, ip)
                        .map(|output| {
                            output
                                .map(|schemas::List { data }| data)
                                .unwrap_or_default()
                        })
                        .map(move |models| Endpoint { ip, models })
                }))
                .await;
                Some((endpoints, (rng, instant)))
            },
        )
        .instrument(tracing::info_span!("watch", ?upstream))
    }

    #[tracing::instrument(err, skip(self))]
    async fn list_models(
        &self,
        upstream: &Upstream,
        ip: IpAddr,
    ) -> anyhow::Result<schemas::List<schemas::Model>> {
        let body = tokio::time::timeout(
            upstream.timeout.unwrap_or(Duration::MAX),
            misc::hyper::get(
                &misc::hyper::client::<String>(
                    self.tls_config.clone(),
                    Some(ip),
                    upstream.http2_only,
                ),
                &upstream.uri,
                "/v1/models",
            )
            .map_err(anyhow::Error::from_boxed),
        )
        .await??;
        Ok(serde_json::from_slice(&body)?)
    }
}
