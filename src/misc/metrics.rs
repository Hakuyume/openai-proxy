use futures::{FutureExt, TryFutureExt};
use http::{Request, Response, Uri};
use http_body::Body;
use http_body_util::BodyExt;
use hyper_util::rt::{TokioExecutor, TokioIo};
use metrics::IntoLabels;
use std::convert::Infallible;
use std::io;
use std::mem;
use std::net::SocketAddr;
use std::time::Duration;
use tower::{Service, ServiceExt};

pub fn install()
-> Result<metrics_exporter_prometheus::PrometheusHandle, metrics::SetRecorderError<impl Sized>> {
    let prometheus_recorder =
        metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
    let prometheus_handle = prometheus_recorder.handle();
    metrics_util::layers::Stack::new(prometheus_recorder)
        .push(metrics_util::layers::PrefixLayer::new(env!(
            "CARGO_BIN_NAME"
        )))
        .install()?;
    Ok(prometheus_handle)
}

pub fn wrap_connector<C, F>(
    connector: C,
    f: F,
) -> impl Clone
+ Service<
    Uri,
    Response = hyper_inspect_io::Io<C::Response, HyperIo>,
    Error = C::Error,
    Future = impl Send,
> + Send
+ 'static
where
    C: Clone + Service<Uri> + Send + 'static,
    C::Future: Send,
    C::Error: std::error::Error + 'static,
    F: Clone + FnMut(&mut Vec<metrics::Label>, &Uri) + Send + 'static,
{
    tower::service_fn({
        let mut f = f.clone();
        move |uri: Uri| {
            let mut labels = Vec::new();
            if let Some(scheme) = uri.scheme_str() {
                labels.push((&"uri.scheme", &scheme.to_owned()).into());
            }
            if let Some(host) = uri.host() {
                labels.push((&"uri.host", &host.to_owned()).into());
            }
            if let Some(port) = uri.port_u16() {
                labels.push((&"uri.port", &port.to_string()).into());
            }
            f(&mut labels, &uri);
            connector.clone().oneshot(uri).map(|output| match output {
                Ok(io) => {
                    metrics::counter!("client_connect", labels.clone()).increment(1);
                    Ok(hyper_inspect_io::Io::new(
                        io,
                        HyperIo::new("client_", labels),
                    ))
                }
                Err(e) => {
                    labels.extend(error_label(&e));
                    metrics::counter!("client_connect_error", labels).increment(1);
                    Err(e)
                }
            })
        }
    })
}

pub async fn serve<S, B>(service: S, bind: SocketAddr) -> io::Result<()>
where
    S: Clone
        + Service<Request<hyper::body::Incoming>, Response = Response<B>, Error = Infallible>
        + Send
        + 'static,
    S::Future: Send,
    B: Body + Send + 'static,
    B::Data: Send + 'static,
    B::Error: Into<tower::BoxError>,
{
    let listener = tokio::net::TcpListener::bind(bind).await?;
    loop {
        let mut labels = Vec::new();
        let io = match listener.accept().await {
            Ok((stream, _)) => {
                metrics::counter!("server_accept", labels).increment(1);
                hyper_inspect_io::Io::new(TokioIo::new(stream), HyperIo::new("server_", Vec::new()))
            }
            Err(e) => {
                labels.extend(error_label(&e));
                metrics::counter!("server_accept_error", labels).increment(1);
                // https://github.com/tokio-rs/axum/blob/axum-v0.7.9/axum/src/serve.rs#L465-L498
                if !matches!(
                    e.kind(),
                    io::ErrorKind::ConnectionRefused
                        | io::ErrorKind::ConnectionAborted
                        | io::ErrorKind::ConnectionReset
                ) {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                continue;
            }
        };
        let service = hyper::service::service_fn({
            let service = service.clone();
            move |request: Request<hyper::body::Incoming>| {
                let labels = vec![
                    (&"method", &request.method().to_string()).into(),
                    (&"path", &request.uri().path().to_string()).into(),
                ];
                metrics::counter!("request", labels.clone()).increment(1);
                let mut guard = Guard {
                    name: "response".to_owned(),
                    labels: labels.into_labels(),
                };
                service.clone().call(request).map_ok(move |response| {
                    let (parts, body) = response.into_parts();
                    guard
                        .labels
                        .push((&"status", &parts.status.as_u16().to_string()).into());
                    let body = body.map_err(move |e| {
                        let _ = &guard;
                        e
                    });
                    Response::from_parts(parts, body)
                })
            }
        });
        tokio::spawn(async move {
            hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                .serve_connection_with_upgrades(io, service)
                .await
        });
    }
}

fn error_label(e: &(dyn std::error::Error + 'static)) -> Option<metrics::Label> {
    let mut stack = Vec::new();
    let mut source = Some(e);
    while let Some(e) = source {
        if let Some(e) = e.downcast_ref::<io::Error>() {
            stack.push(e.kind().to_string());
        }
        source = e.source();
    }
    Some((&"error", &stack.pop()?).into())
}

pub struct HyperIo {
    prefix: &'static str,
    labels: Vec<metrics::Label>,
    read_bytes: metrics::Counter,
    write_bytes: metrics::Counter,
    _guard: Guard,
}

impl HyperIo {
    fn new(prefix: &'static str, labels: Vec<metrics::Label>) -> Self {
        Self {
            prefix,
            labels: labels.clone(),
            read_bytes: metrics::counter!(format!("{prefix}read_bytes"), labels.clone()),
            write_bytes: metrics::counter!(format!("{prefix}write_bytes"), labels.clone()),
            _guard: Guard {
                name: format!("{prefix}drop"),
                labels,
            },
        }
    }
}

impl hyper_inspect_io::InspectRead for HyperIo {
    fn inspect_read(&mut self, value: Result<&[u8], &io::Error>) {
        match value {
            Ok(buf) => self.read_bytes.increment(buf.len() as _),
            Err(e) => {
                let mut labels = self.labels.clone();
                labels.extend(error_label(e));
                metrics::counter!(format!("{}read_error", self.prefix), labels).increment(1);
            }
        }
    }
}

impl hyper_inspect_io::InspectWrite for HyperIo {
    fn inspect_write(&mut self, value: Result<&[u8], &io::Error>) {
        match value {
            Ok(buf) => self.write_bytes.increment(buf.len() as _),
            Err(e) => {
                let mut labels = self.labels.clone();
                labels.extend(error_label(e));
                metrics::counter!(format!("{}write_error", self.prefix), labels).increment(1);
            }
        }
    }
}

struct Guard {
    name: String,
    labels: Vec<metrics::Label>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        metrics::counter!(mem::take(&mut self.name), mem::take(&mut self.labels)).increment(1);
    }
}
