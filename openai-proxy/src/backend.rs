mod connection;
mod protocol;

use crate::{Error, client, endpoint};
use futures::{FutureExt, StreamExt};
use std::sync::Arc;

#[derive(Clone, Debug, serde::Deserialize)]
pub(super) struct Config {
    connection: connection::Config,
    protocol: protocol::Config,
}

pub(super) async fn watch(
    config: Vec<Config>,
) -> Result<impl futures::Stream<Item = Arc<[endpoint::Endpoint]>>, Error> {
    struct State {
        streams: Vec<Stream>,
        is_ready: bool,
    }

    impl State {
        async fn next(&mut self) -> Option<Arc<[endpoint::Endpoint]>> {
            loop {
                let (item, index, _) =
                    futures::future::select_all(self.streams.iter_mut().map(Stream::next)).await;
                if let Some(item) = item {
                    self.streams.extend(item);
                } else {
                    self.streams.swap_remove(index);
                }

                if self.streams.is_empty() {
                    break None;
                }
                if self.streams.iter().all(|stream| match stream {
                    Stream::Connection { is_ready, .. } => *is_ready,
                    Stream::Protocol { providers, .. } => providers.is_some(),
                }) {
                    self.is_ready = true;
                }
                if self.is_ready {
                    break Some(
                        self.streams
                            .iter()
                            .filter_map(|stream| {
                                if let Stream::Protocol {
                                    id,
                                    client,
                                    providers: Some(Ok(providers)),
                                    ..
                                } = stream
                                {
                                    Some(endpoint::Endpoint {
                                        id: *id,
                                        client: client.clone(),
                                        providers: providers.clone(),
                                    })
                                } else {
                                    None
                                }
                            })
                            .collect::<Vec<_>>()
                            .into(),
                    );
                }
            }
        }
    }

    let mut builder = hickory_resolver::TokioResolver::builder_tokio()?;
    builder.options_mut().ip_strategy = hickory_resolver::config::LookupIpStrategy::Ipv4AndIpv6;
    builder.options_mut().cache_size = 0;
    builder.options_mut().try_tcp_on_error = true;
    let resolver = builder.build()?;

    let mut streams = Vec::new();
    for config in config {
        let stream = connection::watch(resolver.clone(), config.connection).await?;
        streams.push(Stream::Connection {
            stream: stream.boxed(),
            protocol: config.protocol,
            is_ready: false,
        });
    }
    let state = State {
        streams,
        is_ready: false,
    };
    let stream =
        futures::stream::unfold(state, async |mut state| Some((state.next().await?, state)));
    Ok(stream)
}

enum Stream {
    Connection {
        stream: futures::stream::BoxStream<
            'static,
            Vec<(client::Client, futures::future::AbortRegistration)>,
        >,
        protocol: protocol::Config,
        is_ready: bool,
    },
    Protocol {
        stream: futures::stream::BoxStream<'static, Result<Vec<schemas::Provider>, Error>>,
        id: uuid::Uuid,
        client: client::Client,
        providers: Option<Result<Vec<schemas::Provider>, Error>>,
    },
}

impl Stream {
    fn next(&mut self) -> impl Future<Output = Option<Vec<Stream>>> + Send + Unpin + '_ {
        match self {
            Stream::Connection {
                stream,
                protocol,
                is_ready,
            } => stream
                .next()
                .map(move |item| {
                    let item = item?
                        .into_iter()
                        .map(|(client, abort_registration)| {
                            let stream = protocol::watch(client.clone(), protocol.clone());
                            Stream::Protocol {
                                stream: futures::future::Abortable::new(stream, abort_registration)
                                    .boxed(),
                                id: uuid::Uuid::new_v4(),
                                client,
                                providers: None,
                            }
                        })
                        .collect();
                    *is_ready = true;
                    Some(item)
                })
                .left_future(),
            Stream::Protocol {
                stream, providers, ..
            } => stream
                .next()
                .map(move |item| {
                    *providers = Some(item?);
                    Some(Vec::new())
                })
                .right_future(),
        }
    }
}
