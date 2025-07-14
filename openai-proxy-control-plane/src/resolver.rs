use futures::{FutureExt, Stream};
use http_body_util::BodyExt;
use hyper_rustls::ConfigBuilderExt;
use serde::Deserialize;
use std::convert::Infallible;
use std::iter;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;
use tracing_futures::Instrument;

#[derive(Clone, Debug)]
pub(crate) struct Upstream {
    pub(crate) uri: http::Uri,
    pub(crate) http2_only: bool,
    pub(crate) interval: Duration,
    pub(crate) timeout: Option<Duration>,
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

pub(crate) struct Resolver {
    tls_config: rustls::ClientConfig,
    resolver: hickory_resolver::TokioResolver,
}

impl Resolver {
    pub(crate) fn new() -> anyhow::Result<Self> {
        let tls_config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()?
        .with_webpki_roots()
        .with_no_client_auth();

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

    pub(crate) fn watch<'a>(
        &'a self,
        upstream: &'a Upstream,
    ) -> impl Stream<Item = Vec<(IpAddr, Vec<crate::schemas::Model>)>> + Send + 'a {
        futures::stream::unfold(
            tokio::time::interval(upstream.interval),
            move |mut interval| async move {
                interval.tick().await;
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
                        .map(move |output| {
                            let models = match output {
                                Ok(crate::schemas::List { data }) => data,
                                Err(e) => {
                                    tracing::warn!(error = e.to_string());
                                    Vec::new()
                                }
                            };
                            (ip, models)
                        })
                        .instrument(tracing::info_span!("list_models", ?ip))
                }))
                .await;
                Some((endpoints, interval))
            },
        )
        .instrument(tracing::info_span!("watch", ?upstream))
    }

    async fn list_models(
        &self,
        upstream: &Upstream,
        ip: IpAddr,
    ) -> anyhow::Result<crate::schemas::List<crate::schemas::Model>> {
        let mut connector =
            hyper_util::client::legacy::connect::HttpConnector::new_with_resolver({
                let f = futures::future::ok::<_, Infallible>(iter::once((ip, 0).into()));
                tower::service_fn(move |_| f.clone())
            });
        connector.enforce_http(false);
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(self.tls_config.clone())
            .https_or_http()
            .enable_http1()
            .enable_http2()
            .wrap_connector(connector);
        let client =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .http2_only(upstream.http2_only)
                .build::<_, String>(connector);

        let response = tokio::time::timeout(upstream.timeout.unwrap_or(Duration::MAX), async {
            let response = client
                .get(format!("{}v1/models", upstream.uri).parse()?)
                .await?;
            let (parts, body) = response.into_parts();
            let body = body.collect().await?.to_bytes();
            anyhow::Ok(http::Response::from_parts(parts, body))
        })
        .await??;

        if response.status().is_success() {
            Ok(serde_json::from_slice(response.body())?)
        } else {
            Err(anyhow::format_err!("response = {response:?}"))
        }
    }
}
