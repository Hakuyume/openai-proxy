use futures::{FutureExt, StreamExt};

pub fn watch<'a, I, SA, SB, T, E, F, U>(
    streams: I,
    f: F,
) -> impl futures::Stream<Item = Vec<U>> + 'a
where
    I: IntoIterator<Item = SA>,
    SA: futures::Stream + Unpin + 'a,
    SA::Item: IntoIterator<Item = SB>,
    SB: futures::Stream<Item = Result<T, E>> + Unpin + 'a,
    T: 'a,
    E: 'a,
    F: FnMut(&T) -> U + 'a,
{
    struct State<SA, SB, T, E, F> {
        streams: Vec<Stream<SA, SB, T, E>>,
        is_ready: bool,
        f: F,
    }

    impl<SA, SB, T, E, F, U> State<SA, SB, T, E, F>
    where
        SA: futures::Stream + Unpin,
        SA::Item: IntoIterator<Item = SB>,
        SB: futures::Stream<Item = Result<T, E>> + Unpin,
        F: FnMut(&T) -> U,
    {
        async fn next(&mut self) -> Option<Vec<U>> {
            loop {
                if self.streams.is_empty() {
                    break None;
                }
                let (item, index, _) =
                    futures::future::select_all(self.streams.iter_mut().map(Stream::next)).await;
                if let Some(item) = item {
                    self.streams.extend(item);
                } else {
                    self.streams.swap_remove(index);
                }

                if self.streams.iter().all(|stream| match stream {
                    Stream::Left { is_ready, .. } => *is_ready,
                    Stream::Right { item, .. } => item.is_some(),
                }) {
                    self.is_ready = true;
                }
                if self.is_ready {
                    let items = self
                        .streams
                        .iter()
                        .filter_map(|stream| {
                            if let Stream::Right {
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
        streams: streams
            .into_iter()
            .map(|stream| Stream::Left {
                stream,
                is_ready: false,
            })
            .collect(),
        is_ready: false,
        f,
    };
    futures::stream::unfold(state, async |mut state| Some((state.next().await?, state)))
}

enum Stream<SA, SB, T, E> {
    Left {
        stream: SA,
        is_ready: bool,
    },
    Right {
        stream: SB,
        item: Option<Result<T, E>>,
    },
}

impl<SA, SB, T, E> Stream<SA, SB, T, E>
where
    SA: futures::Stream + Unpin,
    SA::Item: IntoIterator<Item = SB>,
    SB: futures::Stream<Item = Result<T, E>> + Unpin,
{
    fn next(&mut self) -> impl Future<Output = Option<Vec<Stream<SA, SB, T, E>>>> + Unpin {
        match self {
            Self::Left { stream, is_ready } => stream
                .next()
                .map(move |item| {
                    let item = item?
                        .into_iter()
                        .map(|stream| Self::Right { stream, item: None })
                        .collect();
                    *is_ready = true;
                    Some(item)
                })
                .left_future(),
            Stream::Right { stream, item } => stream
                .next()
                .map(move |item_next| {
                    *item = Some(item_next?);
                    Some(Vec::new())
                })
                .right_future(),
        }
    }
}
