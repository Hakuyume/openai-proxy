use crate::{Endpoint, Error, config, endpoint};
use futures::future::Either;
use futures::{FutureExt, StreamExt, TryFutureExt};
use http_body_util::BodyExt;
use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tracing::Instrument;

#[derive(Clone, Debug, serde::Deserialize)]
pub struct Config {
    #[serde(flatten)]
    connection: Connection,
    #[serde(flatten)]
    options: Options,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(tag = "type")]
enum Connection {
    #[serde(rename = "standard")]
    Standard {
        #[serde(with = "http_serde::uri")]
        uri: http::Uri,
        #[serde(default)]
        http2_prior_knowledge: bool,
        #[serde(default)]
        resolve: bool,
        unix_socket: Option<PathBuf>,
        authorization: Option<config::Authorization>,
    },
    #[serde(rename = "tunnel")]
    Tunnel {
        #[serde(flatten)]
        bind: config::Bind,
    },
}

#[derive(Clone, Copy, Debug, serde::Deserialize)]
struct Options {
    #[serde(with = "humantime_serde")]
    interval: Duration,
    #[serde(default, with = "humantime_serde")]
    timeout: Option<Duration>,
    #[serde(default)]
    vllm: bool,
}

pub async fn watch(
    config: Vec<Config>,
    tx: tokio::sync::watch::Sender<Option<(u64, Arc<[Endpoint]>)>>,
) -> Result<(), Error> {
    let mut builder = hickory_resolver::TokioResolver::builder_tokio()?;
    builder.options_mut().ip_strategy = hickory_resolver::config::LookupIpStrategy::Ipv4AndIpv6;
    builder.options_mut().cache_size = 0;
    builder.options_mut().try_tcp_on_error = true;
    let resolver = builder.build()?;

    let mut watch_backends = Vec::new();
    let mut watch_endpoints = HashMap::<_, (_, _, Option<Result<Vec<schemas::Model>, _>>)>::new();
    for config in config {
        match config.connection {
            Connection::Standard {
                uri,
                http2_prior_knowledge,
                resolve: _,
                unix_socket: unix_socket @ Some(_),
                authorization,
            }
            | Connection::Standard {
                uri,
                http2_prior_knowledge,
                resolve: false,
                unix_socket: unix_socket @ None,
                authorization,
            } => {
                let endpoint = endpoint::Endpoint::standard(
                    uri,
                    http2_prior_knowledge,
                    None,
                    unix_socket,
                    authorization,
                )?;
                let watch = watch_endpoint(endpoint.clone(), config.options);
                watch_endpoints.insert(endpoint.id(), (watch.boxed(), endpoint, None));
            }
            Connection::Standard {
                uri,
                http2_prior_knowledge,
                resolve: true,
                unix_socket: None,
                authorization,
            } => {
                let watch = watch_standard_resolve(
                    resolver.clone(),
                    uri,
                    http2_prior_knowledge,
                    authorization,
                    config.options,
                )?;
                watch_backends.push((watch.boxed(), false));
            }
            Connection::Tunnel { bind } => {
                let watch = watch_tunnel(bind, config.options).await?;
                watch_backends.push((watch.boxed(), true));
            }
        }
    }

    let mut version = 0;
    let mut is_ready = false;
    while !(watch_backends.is_empty() && watch_endpoints.is_empty()) {
        if !is_ready
            && watch_backends.iter().all(|(_, is_ready)| *is_ready)
            && watch_endpoints
                .values()
                .all(|(_, _, models)| models.is_some())
        {
            is_ready = true
        }
        if is_ready {
            let endpoints = watch_endpoints
                .values()
                .filter_map(|(_, endpoint, models)| {
                    Some(Endpoint {
                        endpoint: endpoint.clone(),
                        models: models.as_ref()?.as_ref().ok()?.clone(),
                    })
                })
                .collect::<Vec<_>>();
            tx.send(Some((version, Arc::from(endpoints))))?;
            version += 1;
        }

        let future_backends =
            watch_backends
                .iter_mut()
                .enumerate()
                .map(|(backend_index, (stream, _))| {
                    stream
                        .next()
                        .map(move |item| Either::Left((backend_index, item)))
                        .left_future()
                });
        let future_endpoints = watch_endpoints
            .iter_mut()
            .map(|(endpoint_id, (stream, _, _))| {
                stream
                    .next()
                    .map(|item| Either::Right((*endpoint_id, item)))
                    .right_future()
            });
        let (item, _, _) =
            futures::future::select_all(future_backends.chain(future_endpoints)).await;
        match item {
            Either::Left((backend_index, Some(delta))) => {
                if let Some((_, is_ready)) = watch_backends.get_mut(backend_index) {
                    *is_ready = true;
                }
                for delta in delta {
                    match delta {
                        Delta::Insert { endpoint, options } => {
                            let watch = watch_endpoint(endpoint.clone(), options);
                            watch_endpoints.insert(endpoint.id(), (watch.boxed(), endpoint, None));
                        }
                        Delta::Remove { endpoint_id } => {
                            watch_endpoints.remove(&endpoint_id);
                        }
                    }
                }
            }
            Either::Left((_, None)) => unreachable!(),
            Either::Right((endpoint_id, Some(models_next))) => {
                if let Some((_, _, models)) = watch_endpoints.get_mut(&endpoint_id) {
                    *models = Some(models_next);
                }
            }
            Either::Right((endpoint_id, None)) => {
                watch_endpoints.remove(&endpoint_id);
            }
        }
    }

    Ok(())
}

enum Delta {
    Insert {
        endpoint: endpoint::Endpoint,
        options: Options,
    },
    Remove {
        endpoint_id: uuid::Uuid,
    },
}

fn watch_standard_resolve(
    resolver: hickory_resolver::TokioResolver,
    uri: http::Uri,
    http2_prior_knowledge: bool,
    authorization: Option<config::Authorization>,
    options: Options,
) -> Result<impl futures::Stream<Item = Vec<Delta>> + Send, Error> {
    struct State {
        resolver: hickory_resolver::TokioResolver,
        uri: http::Uri,
        http2_prior_knowledge: bool,
        authorization: Option<config::Authorization>,
        options: Options,
        host: String,
        interval: misc::interval::Interval,
        endpoint_ids: HashMap<IpAddr, uuid::Uuid>,
    }

    async fn next(state: &mut State) -> Vec<Delta> {
        state.interval.tick().await;
        let mut delta = Vec::new();
        match state.resolver.lookup_ip(&state.host).await {
            Ok(lookup_ip) => {
                let mut endpoint_ids_next = HashMap::new();
                for ip_addr in lookup_ip.iter() {
                    if let Some(endpoint_id) = state.endpoint_ids.remove(&ip_addr) {
                        endpoint_ids_next.insert(ip_addr, endpoint_id);
                    } else {
                        match endpoint::Endpoint::standard(
                            state.uri.clone(),
                            state.http2_prior_knowledge,
                            Some((ip_addr, 0).into()),
                            None,
                            state.authorization.clone(),
                        ) {
                            Ok(endpoint) => {
                                endpoint_ids_next.insert(ip_addr, endpoint.id());
                                delta.push(Delta::Insert {
                                    endpoint,
                                    options: state.options,
                                });
                            }
                            Err(e) => tracing::error!(error = e.to_string()),
                        }
                    }
                }
                for (_, endpoint_id) in state.endpoint_ids.drain() {
                    delta.push(Delta::Remove { endpoint_id });
                }
                state.endpoint_ids = endpoint_ids_next;
            }
            Err(e) => {
                tracing::warn!(error = e.to_string());
                if e.is_nx_domain() || e.is_no_records_found() {
                    for (_, endpoint_id) in state.endpoint_ids.drain() {
                        delta.push(Delta::Remove { endpoint_id });
                    }
                }
            }
        }
        delta
    }

    let host = uri.host().ok_or("missing host")?.to_owned();
    let state = State {
        resolver,
        uri,
        http2_prior_knowledge,
        authorization,
        options,
        host,
        interval: misc::interval::Interval::new(options.interval),
        endpoint_ids: HashMap::new(),
    };

    Ok(futures::stream::unfold(state, async |mut state| {
        Some((next(&mut state).await, state))
    }))
}

async fn watch_tunnel(
    bind: config::Bind,
    options: Options,
) -> Result<impl futures::Stream<Item = Vec<Delta>> + Send, Error> {
    struct State {
        options: Options,
        futures: futures::stream::FuturesUnordered<
            futures::future::BoxFuture<'static, Either<Accept, uuid::Uuid>>,
        >,
    }
    type Accept = (tokio_net_incoming::Listener, endpoint::Endpoint, Connection);
    type Connection =
        endpoint::Connection<tokio_tungstenite::WebSocketStream<tokio_net_incoming::Stream>>;

    async fn next(state: &mut State) -> Delta {
        match state.futures.next().await {
            Some(Either::Left((listener, endpoint, connection))) => {
                state
                    .futures
                    .push(accept(listener).map(Either::Left).boxed());
                state.futures.push(
                    tokio::spawn(
                        connection
                            .map_err(|e| tracing::warn!(error = e.to_string()))
                            .instrument(tracing::Span::current()),
                    )
                    .map({
                        let endpoint_id = endpoint.id();
                        move |_| Either::Right(endpoint_id)
                    })
                    .boxed(),
                );
                Delta::Insert {
                    endpoint,
                    options: state.options,
                }
            }
            Some(Either::Right(endpoint_id)) => Delta::Remove { endpoint_id },
            // `futures` always contains `accept` future.
            None => unreachable!(),
        }
    }

    async fn accept(listener: tokio_net_incoming::Listener) -> Accept {
        async fn accept(
            listener: &tokio_net_incoming::Listener,
        ) -> Result<(endpoint::Endpoint, Connection), Error> {
            let (stream, _) = listener.accept().await?;
            let stream = tokio_tungstenite::accept_async(stream).await?;
            endpoint::Endpoint::tunnel(stream).await
        }

        loop {
            match accept(&listener).await {
                Ok((endpoint, connection)) => {
                    break (listener, endpoint, connection);
                }
                Err(e) => {
                    tracing::warn!(error = e.to_string());
                }
            }
        }
    }

    let listener = bind.bind().await?;
    let state = State {
        options,
        futures: futures::stream::FuturesUnordered::new(),
    };
    state
        .futures
        .push(accept(listener).map(Either::Left).boxed());
    Ok(futures::stream::unfold(state, async |mut state| {
        Some((vec![next(&mut state).await], state))
    }))
}

fn watch_endpoint(
    endpoint: endpoint::Endpoint,
    options: Options,
) -> impl futures::Stream<Item = Result<Vec<schemas::Model>, Error>> + Send {
    struct State {
        endpoint: endpoint::Endpoint,
        options: Options,
        interval: misc::interval::Interval,
    }

    async fn next(state: &mut State) -> Option<Result<Vec<schemas::Model>, Error>> {
        state.interval.tick().await;
        let f = if state.options.vllm {
            futures::future::try_join(
                list_models(&state.endpoint),
                vllm_scrape_metrics(&state.endpoint),
            )
            .map_ok(|(mut models, metrics)| {
                for model in &mut models {
                    model.metrics = metrics.clone();
                }
                models
            })
            .left_future()
        } else {
            list_models(&state.endpoint).right_future()
        };
        let f = if let Some(timeout) = state.options.timeout {
            tokio::time::timeout(timeout, f)
                .map(|output| output?)
                .left_future()
        } else {
            f.right_future()
        };
        match f.await {
            Ok(models) => Some(Ok(models)),
            Err(e) if endpoint::is_closed(&e) => None,
            Err(e) => Some(Err(e)),
        }
    }

    let state = State {
        endpoint,
        options,
        interval: misc::interval::Interval::new(options.interval),
    };
    futures::stream::unfold(state, async |mut state| {
        Some((next(&mut state).await?, state))
    })
}

#[tracing::instrument(err(level = tracing::Level::WARN), skip_all)]
async fn list_models(endpoint: &endpoint::Endpoint) -> Result<Vec<schemas::Model>, Error> {
    let response = get(endpoint, "/v1/models").await?;
    let body = serde_json::from_slice::<schemas::List<_>>(response.body())?;
    Ok(body.data)
}

#[tracing::instrument(err(level = tracing::Level::WARN), skip_all)]
async fn vllm_scrape_metrics(endpoint: &endpoint::Endpoint) -> Result<schemas::Metrics, Error> {
    let response = get(endpoint, "/metrics").await?;
    misc::vllm::parse_metrics(response.body())
}

async fn get(
    endpoint: &endpoint::Endpoint,
    uri: &str,
) -> Result<http::Response<bytes::Bytes>, Error> {
    #[derive(Debug, thiserror::Error)]
    #[error("{0:?}")]
    struct StatusError(http::Response<bytes::Bytes>);

    let response = endpoint
        .send(http::Request::get(uri).body(http_body_util::Empty::new())?)
        .await?;
    let (parts, body) = response.into_parts();
    let body = body
        .collect()
        .map_ok(http_body_util::Collected::to_bytes)
        .await?;
    let response = http::Response::from_parts(parts, body);
    if response.status().is_success() {
        Ok(response)
    } else {
        Err(StatusError(response).into())
    }
}
