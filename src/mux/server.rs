use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use std::convert::Infallible;
use std::fmt;
use std::future;
use std::iter;
use std::net::{IpAddr, SocketAddr};
use std::task::{Context, Poll};
use url::Url;

pub(super) struct Server {
    uri: Url,
    ip: IpAddr,
    client: Client,
    models: Vec<super::Model>,
}

impl fmt::Debug for Server {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Server")
            .field("uri", &self.uri.as_str())
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
    #[tracing::instrument(err(level = tracing::Level::WARN), fields(uri = uri.as_str()))]
    pub(super) fn new(mut uri: Url, ip: IpAddr) -> anyhow::Result<Self> {
        let mut http2_only = false;
        for (key, value) in uri.query_pairs() {
            match &*key {
                "http2_only" => http2_only = value.parse()?,
                _ => anyhow::bail!("unknown option"),
            }
        }
        uri.set_query(None);
        uri.set_fragment(None);

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
                .http2_only(http2_only)
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

    pub(super) async fn request(
        &self,
        request: http::Request<Bytes>,
    ) -> anyhow::Result<http::Response<hyper::body::Incoming>> {
        let request = {
            let (parts, body) = request.into_parts();
            let mut uri = self.uri.clone();
            if let Ok(mut path_segments) = uri.path_segments_mut() {
                path_segments
                    .pop_if_empty()
                    .extend(parts.uri.path().split('/').skip(1));
            }
            uri.set_query(parts.uri.query());
            let builder = http::Request::builder()
                .method(parts.method)
                .uri(uri.as_str());
            let builder = parts
                .headers
                .into_iter()
                .filter_map(|(name, value)| {
                    let name = name?;
                    [http::header::CONTENT_LENGTH, http::header::CONTENT_TYPE]
                        .contains(&name)
                        .then_some((name, value))
                })
                .fold(builder, |builder, (name, value)| {
                    builder.header(name, value)
                });
            builder.body(Full::new(body))?
        };
        tracing::info!(method = ?request.method(), uri = ?request.uri(), headers = ?request.headers());

        let response = self.client.request(request).await?;

        let response = {
            let (parts, body) = response.into_parts();
            let builder = http::Response::builder().status(parts.status);
            let builder = parts
                .headers
                .into_iter()
                .filter_map(|(name, value)| {
                    let name = name?;
                    [http::header::CONTENT_LENGTH, http::header::CONTENT_TYPE]
                        .contains(&name)
                        .then_some((name, value))
                })
                .fold(builder, |builder, (name, value)| {
                    builder.header(name, value)
                });
            builder.body(body)?
        };

        Ok(response)
    }

    #[tracing::instrument(err(level = tracing::Level::WARN))]
    pub(super) async fn fetch_models(&mut self) -> anyhow::Result<()> {
        let request = http::Request::get("/v1/models").body(Bytes::new())?;
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
