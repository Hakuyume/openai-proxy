use bytes::BufMut;
use std::cmp;
use std::io;
use std::pin::Pin;
use std::task::{self, Context, Poll};
use tokio_tungstenite::tungstenite::Message;

#[pin_project::pin_project]
pub struct Io<T> {
    #[pin]
    inner: T,
    read: bytes::Bytes,
}

impl<T> Io<T> {
    pub fn new(inner: T) -> Self {
        Self {
            inner,
            read: bytes::Bytes::new(),
        }
    }
}

impl<T, E> hyper::rt::Read for Io<T>
where
    T: futures::Stream<Item = Result<Message, E>>,
    E: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        mut buf: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<Result<(), io::Error>> {
        let mut this = self.project();
        while this.read.is_empty() {
            let message = task::ready!(this.inner.as_mut().poll_next(cx))
                .transpose()
                .map_err(map_err)?;
            match message {
                Some(Message::Binary(data)) => *this.read = data,
                Some(Message::Close(_)) | None => break,
                _ => (),
            }
        }
        buf.put_slice(
            &this
                .read
                .split_to(cmp::min(this.read.len(), buf.remaining()))[..],
        );
        Poll::Ready(Ok(()))
    }
}

impl<T> hyper::rt::Write for Io<T>
where
    T: futures::Sink<Message>,
    T::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        self.poll_write_vectored(cx, &[io::IoSlice::new(buf)])
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        let this = self.project();
        this.inner.poll_flush(cx).map_err(map_err)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        let this = self.project();
        this.inner.poll_close(cx).map_err(map_err)
    }

    fn is_write_vectored(&self) -> bool {
        true
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<Result<usize, io::Error>> {
        let mut this = self.project();
        task::ready!(this.inner.as_mut().poll_ready(cx)).map_err(map_err)?;
        let mut data = bytes::BytesMut::new();
        for buf in bufs {
            data.put_slice(buf);
        }
        let data = data.freeze();
        this.inner
            .as_mut()
            .start_send(Message::Binary(data.clone()))
            .map_err(map_err)?;
        Poll::Ready(Ok(data.len()))
    }
}

fn map_err<E>(e: E) -> io::Error
where
    E: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    io::Error::new(io::ErrorKind::BrokenPipe, e)
}
