use crate::{Error, config};
use futures::FutureExt;
use http_body_util::BodyExt;
use std::iter;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

pub type Body = http_body_util::combinators::UnsyncBoxBody<bytes::Bytes, Error>;
pub type Connection<S> = hyper::client::conn::http2::Connection<
    misc::tungstenite::Io<S>,
    Body,
    hyper_util::rt::TokioExecutor,
>;

#[derive(Clone, Debug)]
pub struct Endpoint {
    id: uuid::Uuid,
    inner: Arc<Inner>,
}

#[derive(Debug)]
enum Inner {
    Standard {
        client: reqwest::Client,
        uri: http::Uri,
        http2_prior_knowledge: bool,
        resolve: Option<SocketAddr>,
        unix_socket: Option<PathBuf>,
        authorization: Option<config::Authorization>,
    },
    Tunnel {
        send_request: hyper::client::conn::http2::SendRequest<Body>,
    },
}

pub struct Info {
    pub addr: SocketAddr,
    pub http2_prior_knowledge: bool,
}

impl Endpoint {
    pub fn standard(
        uri: http::Uri,
        http2_prior_knowledge: bool,
        resolve: Option<SocketAddr>,
        unix_socket: Option<PathBuf>,
        authorization: Option<config::Authorization>,
    ) -> Result<Self, Error> {
        let mut builder = reqwest::Client::builder();
        if http2_prior_knowledge {
            builder = builder.http2_prior_knowledge();
        }
        if let Some(resolve) = resolve {
            struct Resolve(SocketAddr);

            impl reqwest::dns::Resolve for Resolve {
                fn resolve(&self, _: reqwest::dns::Name) -> reqwest::dns::Resolving {
                    futures::future::ok(Box::new(iter::once(self.0)) as _).boxed()
                }
            }

            builder = builder.dns_resolver(Resolve(resolve));
        }
        if let Some(unix_socket) = &unix_socket {
            builder = builder.unix_socket(unix_socket.clone());
        }
        let client = builder.build()?;
        Ok(Self {
            id: uuid::Uuid::new_v4(),
            inner: Arc::new(Inner::Standard {
                client,
                uri,
                http2_prior_knowledge,
                resolve,
                unix_socket,
                authorization,
            }),
        })
    }

    pub async fn tunnel<S>(stream: S) -> Result<(Self, Connection<S>), Error>
    where
        misc::tungstenite::Io<S>:
            hyper::rt::Read + hyper::rt::Write + Send + Sync + Unpin + 'static,
    {
        let (send_request, connection) =
            hyper::client::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
                .keep_alive_interval(Duration::from_secs(5))
                .keep_alive_while_idle(true)
                .timer(hyper_util::rt::TokioTimer::new())
                .handshake(misc::tungstenite::Io::new(stream))
                .await?;
        Ok((
            Self {
                id: uuid::Uuid::new_v4(),
                inner: Arc::new(Inner::Tunnel { send_request }),
            },
            connection,
        ))
    }

    pub fn id(&self) -> uuid::Uuid {
        self.id
    }

    pub fn send<B>(
        &self,
        mut request: http::Request<B>,
    ) -> impl Future<Output = Result<http::Response<Body>, Error>> + Send + 'static
    where
        B: http_body::Body<Data = bytes::Bytes> + Send + 'static,
        B::Error: Into<Error>,
    {
        let inner = self.inner.clone();
        request.headers_mut().remove(http::header::AUTHORIZATION);
        request.headers_mut().remove(http::header::COOKIE);
        request.headers_mut().remove(http::header::HOST);
        request
            .headers_mut()
            .remove(http::header::PROXY_AUTHORIZATION);
        request.headers_mut().remove("x-api-key");
        async move {
            match &*inner {
                Inner::Standard {
                    client,
                    uri,
                    authorization,
                    ..
                } => {
                    set_base(request.uri_mut(), uri.clone())?;
                    if let Some(authorization) = authorization {
                        request.headers_mut().insert(
                            http::header::AUTHORIZATION,
                            http::HeaderValue::from_str(&authorization.value().await?)?,
                        );
                    }
                    let request = reqwest::Request::try_from(request.map(|body| {
                        reqwest::Body::wrap_stream(http_body_util::BodyDataStream::new(body))
                    }))?;
                    let response = client.execute(request).await?;
                    Ok(http::Response::from(response)
                        .map(|body| body.map_err(Into::into).boxed_unsync()))
                }
                Inner::Tunnel { send_request } => {
                    let response = send_request
                        .clone()
                        .send_request(request.map(|body| body.map_err(Into::into).boxed_unsync()))
                        .await?;
                    Ok(response.map(|body| body.map_err(Into::into).boxed_unsync()))
                }
            }
        }
    }

    pub fn info(&self) -> Option<Info> {
        match &*self.inner {
            Inner::Standard {
                uri,
                http2_prior_knowledge,
                resolve: Some(resolve),
                unix_socket: None,
                authorization: None,
                ..
            } if uri.scheme() == Some(&http::uri::Scheme::HTTP) => {
                let port = if let Some(port) = uri.port_u16() {
                    port
                } else if resolve.port() > 0 {
                    resolve.port()
                } else {
                    80
                };
                Some(Info {
                    addr: (resolve.ip(), port).into(),
                    http2_prior_knowledge: *http2_prior_knowledge,
                })
            }
            _ => None,
        }
    }
}

pub fn is_closed(e: &Error) -> bool {
    e.downcast_ref().is_some_and(hyper::Error::is_closed)
}

fn set_base(uri: &mut http::Uri, base: http::Uri) -> Result<(), http::Error> {
    let mut parts = base.into_parts();
    if let Some(path_and_query) = &mut parts.path_and_query {
        *path_and_query = format!(
            "{}{}",
            path_and_query.path().trim_end_matches('/'),
            uri.path(),
        )
        .parse()?;
    }
    *uri = http::Uri::from_parts(parts)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_set_base() {
        fn check(uri: &str, base: &str, expected: &str) {
            let mut uri = uri.parse().unwrap();
            super::set_base(&mut uri, base.parse().unwrap()).unwrap();
            assert_eq!(uri, expected);
        }

        check("/baz", "http://foo.bar/", "http://foo.bar/baz");
        check("/qux", "http://foo.bar/baz/", "http://foo.bar/baz/qux");
    }
}
