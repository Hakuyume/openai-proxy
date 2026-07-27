use crate::Error;
use futures::FutureExt;
use http_body_util::BodyExt;
use std::iter;
use std::net::SocketAddr;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct Client {
    inner: Arc<Inner>,
}

#[derive(Debug)]
enum Inner {
    Standard {
        client: reqwest::Client,
        config: standard::Config,
    },
    Tunnel {
        send_request: hyper::client::conn::http2::SendRequest<Body>,
        config: tunnel::Config,
    },
}

#[derive(Copy, Clone, Debug)]
pub enum Config<'a> {
    Standard(&'a standard::Config),
    Tunnel(#[allow(dead_code)] &'a tunnel::Config),
}

pub type Body = http_body_util::combinators::UnsyncBoxBody<bytes::Bytes, Error>;

pub mod standard {
    use crate::config;
    use std::net::SocketAddr;
    use std::path::PathBuf;

    #[derive(Clone, Debug)]
    pub struct Config {
        pub uri: http::Uri,
        pub http2_prior_knowledge: bool,
        pub resolve: Option<SocketAddr>,
        pub unix_socket: Option<PathBuf>,
        pub authorization: Option<config::Authorization>,
    }
}

pub mod tunnel {
    use std::time::Duration;

    pub type Connection<S> = hyper::client::conn::http2::Connection<
        misc::tungstenite::Io<S>,
        super::Body,
        hyper_util::rt::TokioExecutor,
    >;

    #[derive(Clone, Debug)]
    pub struct Config {
        pub keep_alive_interval: Duration,
    }
}

impl Client {
    pub fn standard(config: standard::Config) -> Result<Self, Error> {
        let mut builder = reqwest::Client::builder();
        if config.http2_prior_knowledge {
            builder = builder.http2_prior_knowledge();
        }
        if let Some(resolve) = config.resolve {
            struct Resolve(SocketAddr);

            impl reqwest::dns::Resolve for Resolve {
                fn resolve(&self, _: reqwest::dns::Name) -> reqwest::dns::Resolving {
                    futures::future::ok(Box::new(iter::once(self.0)) as _).boxed()
                }
            }

            builder = builder.dns_resolver(Resolve(resolve));
        }
        if let Some(unix_socket) = &config.unix_socket {
            builder = builder.unix_socket(unix_socket.clone());
        }
        let client = builder.build()?;
        Ok(Self {
            inner: Arc::new(Inner::Standard { client, config }),
        })
    }

    pub async fn tunnel<S>(
        stream: S,
        config: tunnel::Config,
    ) -> Result<(Self, tunnel::Connection<S>), Error>
    where
        misc::tungstenite::Io<S>:
            hyper::rt::Read + hyper::rt::Write + Send + Sync + Unpin + 'static,
    {
        let (send_request, connection) =
            hyper::client::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
                .keep_alive_interval(config.keep_alive_interval)
                .keep_alive_while_idle(true)
                .timer(hyper_util::rt::TokioTimer::new())
                .handshake(misc::tungstenite::Io::new(stream))
                .await?;
        Ok((
            Self {
                inner: Arc::new(Inner::Tunnel {
                    send_request,
                    config,
                }),
            },
            connection,
        ))
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
        *request.version_mut() = http::Version::default();
        request.headers_mut().remove(http::header::AUTHORIZATION);
        request.headers_mut().remove(http::header::COOKIE);
        request.headers_mut().remove(http::header::HOST);
        request
            .headers_mut()
            .remove(http::header::PROXY_AUTHORIZATION);
        request.headers_mut().remove("x-api-key");
        async move {
            match &*inner {
                Inner::Standard { client, config } => {
                    set_base(request.uri_mut(), config.uri.clone())?;
                    if let Some(authorization) = &config.authorization {
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
                Inner::Tunnel { send_request, .. } => {
                    let response = send_request
                        .clone()
                        .send_request(request.map(|body| body.map_err(Into::into).boxed_unsync()))
                        .await?;
                    Ok(response.map(|body| body.map_err(Into::into).boxed_unsync()))
                }
            }
        }
    }

    pub fn config(&self) -> Config<'_> {
        match &*self.inner {
            Inner::Standard { config, .. } => Config::Standard(config),
            Inner::Tunnel { config, .. } => Config::Tunnel(config),
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
