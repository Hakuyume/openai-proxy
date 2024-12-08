use bytes::Bytes;
use http::Uri;
use http_body_util::BodyExt;
use http_body_util::Full;
use std::convert::Infallible;
use std::fmt;
use std::future;
use std::iter;
use std::net::{IpAddr, SocketAddr};
use std::task::{Context, Poll};

pub(super) struct Server {
    uri: Uri,
    ip: IpAddr,
    client: Client,
    models: Vec<super::Model>,
}

impl fmt::Debug for Server {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Server")
            .field("uri", &self.uri)
            .field("ip", &self.ip)
            .field(
                "models",
                &self
                    .models
                    .iter()
                    .map(|model| &model.id)
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl Server {
    pub(super) fn new(uri: Uri, ip: IpAddr) -> anyhow::Result<Self> {
        let mut connector =
            hyper_util::client::legacy::connect::HttpConnector::new_with_resolver(Resolver(ip));
        connector.enforce_http(false);
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_provider_and_webpki_roots(rustls::crypto::aws_lc_rs::default_provider())?
            .https_or_http()
            .enable_http1()
            .enable_http2()
            .wrap_connector(connector);
        let client =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .build(connector);

        Ok(Self {
            uri,
            ip,
            client,
            models: Vec::new(),
        })
    }

    pub fn models(&self) -> &[super::Model] {
        &self.models
    }

    pub(super) async fn request<B>(
        &self,
        request: http::Request<B>,
    ) -> anyhow::Result<http::Response<hyper::body::Incoming>>
    where
        B: Into<Bytes>,
    {
        let (mut parts, body) = request.into_parts();
        parts.uri = {
            let mut this = self.uri.clone().into_parts();
            let other = parts.uri.into_parts();
            this.path_and_query = match (this.path_and_query, other.path_and_query) {
                (Some(this), Some(other)) => {
                    Some(format!("{}{}", this.path().trim_end_matches('/'), other,).parse()?)
                }
                (this, None) => this,
                (None, other) => other,
            };
            Uri::from_parts(this)?
        };
        parts.headers.remove(http::header::HOST);
        tracing::info!(uri = ?parts.uri);

        let request = http::Request::from_parts(parts, Full::new(body.into()));
        Ok(self.client.request(request).await?)
    }

    #[tracing::instrument(err(level = tracing::Level::WARN), fields(uri = ?self.uri, ip = ?self.ip), skip_all)]
    pub(super) async fn fetch_models(&mut self) -> anyhow::Result<()> {
        let request = http::Request::get("http://example/v1/models").body(Bytes::new())?;
        let response = self.request(request).await?;

        let (parts, body) = response.into_parts();
        let body = body.collect().await?.to_bytes();
        if parts.status.is_success() {
            self.models = serde_json::from_slice::<super::List<_>>(&body)?.data;
            Ok(())
        } else {
            Err(anyhow::format_err!(
                "status = {:?}, body = {:?}",
                parts.status,
                body
            ))
        }
    }
}

type Client = hyper_util::client::legacy::Client<
    hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector<Resolver>>,
    Full<Bytes>,
>;

#[derive(Clone, Copy)]
struct Resolver(IpAddr);
impl tower::Service<hyper_util::client::legacy::connect::dns::Name> for Resolver {
    type Response = iter::Once<SocketAddr>;
    type Error = Infallible;
    type Future = future::Ready<Result<Self::Response, Self::Error>>;
    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
    fn call(&mut self, _: hyper_util::client::legacy::connect::dns::Name) -> Self::Future {
        future::ready(Ok(iter::once((self.0, 0).into())))
    }
}
