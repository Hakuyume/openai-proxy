use futures::{FutureExt, StreamExt};

pub fn discover_and_probe<'a, I, D, P, T, E, F, U>(
    discover_streams: I,
    f: F,
) -> impl futures::Stream<Item = Vec<U>> + 'a
where
    I: IntoIterator<Item = D>,
    D: futures::Stream + Unpin + 'a,
    D::Item: IntoIterator<Item = (P, futures::future::AbortRegistration)>,
    P: futures::Stream<Item = Result<T, E>> + Unpin + 'a,
    T: 'a,
    E: 'a,
    F: FnMut(&T) -> U + 'a,
{
    struct State<D, P, T, E, F> {
        streams: Vec<Stream<D, P, T, E>>,
        is_ready: bool,
        f: F,
    }

    impl<D, P, T, E, F, U> State<D, P, T, E, F>
    where
        D: futures::Stream + Unpin,
        D::Item: IntoIterator<Item = (P, futures::future::AbortRegistration)>,
        P: futures::Stream<Item = Result<T, E>> + Unpin,
        F: FnMut(&T) -> U,
    {
        async fn next(&mut self) -> Option<Vec<U>> {
            loop {
                if self.streams.is_empty() {
                    break None;
                }
                let (streams, index, _) =
                    futures::future::select_all(self.streams.iter_mut().map(Stream::next)).await;
                if let Some(streams) = streams {
                    self.streams.extend(streams);
                } else {
                    self.streams.swap_remove(index);
                }

                if self.streams.iter().all(|stream| match stream {
                    Stream::Discover { is_ready, .. } => *is_ready,
                    Stream::Probe { item, .. } => item.is_some(),
                }) {
                    self.is_ready = true;
                }
                if self.is_ready {
                    let items = self
                        .streams
                        .iter()
                        .filter_map(|stream| {
                            if let Stream::Probe {
                                item: Some(Ok(item)),
                                ..
                            } = stream
                            {
                                Some((self.f)(item))
                            } else {
                                None
                            }
                        })
                        .collect();
                    break Some(items);
                }
            }
        }
    }

    let state = State {
        streams: discover_streams
            .into_iter()
            .map(|discover_stream| Stream::Discover {
                stream: discover_stream,
                is_ready: false,
            })
            .collect(),
        is_ready: false,
        f,
    };
    futures::stream::unfold(state, async |mut state| Some((state.next().await?, state)))
}

enum Stream<D, P, T, E> {
    Discover {
        stream: D,
        is_ready: bool,
    },
    Probe {
        stream: futures::future::Abortable<P>,
        item: Option<Result<T, E>>,
    },
}

impl<D, P, T, E> Stream<D, P, T, E>
where
    D: futures::Stream + Unpin,
    D::Item: IntoIterator<Item = (P, futures::future::AbortRegistration)>,
    P: futures::Stream<Item = Result<T, E>> + Unpin,
{
    fn next(&mut self) -> impl Future<Output = Option<Vec<Self>>> + Unpin + '_ {
        match self {
            Self::Discover { stream, is_ready } => stream
                .next()
                .map(move |probe_streams| {
                    let streams = probe_streams?
                        .into_iter()
                        .map(|(probe_stream, abort_registration)| Self::Probe {
                            stream: futures::future::Abortable::new(
                                probe_stream,
                                abort_registration,
                            ),
                            item: None,
                        })
                        .collect();
                    *is_ready = true;
                    Some(streams)
                })
                .left_future(),
            Stream::Probe { stream, item } => stream
                .next()
                .map(move |item_next| {
                    *item = Some(item_next?);
                    Some(Vec::new())
                })
                .right_future(),
        }
    }
}
