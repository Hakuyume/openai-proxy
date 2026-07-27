use crate::{Error, client, config};
use futures::StreamExt;
use std::collections::HashMap;
use std::future;
use std::net::IpAddr;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Clone, Debug, serde::Deserialize)]
pub(super) struct Config {
    #[serde(with = "http_serde::uri")]
    uri: http::Uri,
    #[serde(default)]
    http2_prior_knowledge: bool,
    resolve: Option<Resolve>,
    unix_socket: Option<PathBuf>,
    authorization: Option<config::Authorization>,
}

#[derive(Clone, Debug, serde::Deserialize)]
struct Resolve {
    #[serde(with = "humantime_serde")]
    interval: Duration,
}

pub(super) fn watch(
    resolver: hickory_resolver::TokioResolver,
    mut config: Config,
) -> Result<
    impl futures::Stream<Item = Vec<(client::Client, futures::future::AbortRegistration)>> + Send,
    Error,
> {
    let stream = if config.unix_socket.is_none()
        && let Some(resolve) = config.resolve.take()
    {
        watch_resolve(resolver, config, resolve)?.right_stream()
    } else {
        let config = client::standard::Config {
            uri: config.uri.clone(),
            http2_prior_knowledge: config.http2_prior_knowledge,
            resolve: None,
            unix_socket: config.unix_socket,
            authorization: config.authorization,
        };
        let client = client::Client::standard(config)?;
        let (_, abort_registration) = futures::future::AbortHandle::new_pair();
        futures::stream::once(future::ready(vec![(client, abort_registration)])).left_stream()
    };
    Ok(stream)
}

fn watch_resolve(
    resolver: hickory_resolver::TokioResolver,
    config: Config,
    resolve: Resolve,
) -> Result<
    impl futures::Stream<Item = Vec<(client::Client, futures::future::AbortRegistration)>> + Send,
    Error,
> {
    struct State {
        resolver: hickory_resolver::TokioResolver,
        config: Config,
        interval: misc::time::Interval,
        host: String,
        abort_guards: HashMap<IpAddr, misc::future::AbortGuard>,
    }

    impl State {
        async fn next(&mut self) -> Vec<(client::Client, futures::future::AbortRegistration)> {
            self.interval.tick().await;
            let mut item = Vec::new();
            match self.resolver.lookup_ip(&self.host).await {
                Ok(lookup_ip) => {
                    let mut abort_guards_next = HashMap::new();
                    for ip_addr in lookup_ip.iter() {
                        if let Some(abort_guard) = self.abort_guards.remove(&ip_addr) {
                            abort_guards_next.insert(ip_addr, abort_guard);
                        } else {
                            let config = client::standard::Config {
                                uri: self.config.uri.clone(),
                                http2_prior_knowledge: self.config.http2_prior_knowledge,
                                resolve: Some((ip_addr, 0).into()),
                                unix_socket: None,
                                authorization: self.config.authorization.clone(),
                            };
                            match client::Client::standard(config) {
                                Ok(client) => {
                                    let (abort_guard, abort_registration) =
                                        misc::future::AbortGuard::new_pair();
                                    abort_guards_next.insert(ip_addr, abort_guard);
                                    item.push((client, abort_registration));
                                }
                                Err(e) => tracing::error!(error = e.to_string()),
                            }
                        }
                    }
                    self.abort_guards = abort_guards_next;
                }
                Err(e) => {
                    tracing::warn!(error = e.to_string());
                    if e.is_nx_domain() || e.is_no_records_found() {
                        self.abort_guards.clear()
                    }
                }
            }
            item
        }
    }

    let host = config.uri.host().ok_or("missing host")?.to_owned();
    let state = State {
        resolver,
        config,
        host,
        interval: misc::time::interval(resolve.interval),
        abort_guards: HashMap::new(),
    };
    let stream =
        futures::stream::unfold(state, async |mut state| Some((state.next().await, state)));
    Ok(stream)
}
