use axum::{Json, Router, extract, routing};
use bytes::Bytes;
use clap::Parser;
use futures::{FutureExt, TryFutureExt};
use http::header::HOST;
use http_body_util::BodyExt;
use nom::Finish;
use std::net::Ipv4Addr;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::net::TcpListener;

#[derive(Parser)]
struct Args {
    #[clap(long, default_value_t = 80)]
    port: u16,
    #[clap(long)]
    upstream: http::Uri,
    #[clap(long, value_parser = humantime::parse_duration)]
    interval: Duration,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    let state = Arc::new(State {
        client: misc::hyper::client(misc::hyper::tls_config()?, None, false),
        upstream: args.upstream,
        models: RwLock::new(None),
    });

    futures::future::try_join(
        async {
            let app = Router::new()
                .route("/v1/models", routing::get(list_models))
                .fallback(fallback)
                .with_state(state.clone())
                .layer(tower_http::trace::TraceLayer::new_for_http());

            let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, args.port)).await?;
            axum::serve(listener, app)
                .with_graceful_shutdown(tokio::signal::ctrl_c().map(|_| ()))
                .await
        },
        watch(state.clone(), args.interval).map(Ok),
    )
    .await?;

    Ok(())
}

struct State {
    client: misc::hyper::Client<axum::body::Body>,
    upstream: http::Uri,
    models: RwLock<Option<schemas::List<schemas::Model>>>,
}

async fn list_models(
    extract::State(state): extract::State<Arc<State>>,
) -> Result<Json<schemas::List<schemas::Model>>, http::StatusCode> {
    match state.models.read().unwrap().clone() {
        Some(v) => Ok(Json(v)),
        None => Err(http::StatusCode::SERVICE_UNAVAILABLE),
    }
}

async fn fallback(
    extract::State(state): extract::State<Arc<State>>,
    mut request: http::Request<axum::body::Body>,
) -> Result<http::Response<hyper::body::Incoming>, (http::StatusCode, String)> {
    *request.uri_mut() = format!(
        "{}{}",
        state.upstream.to_string().trim_end_matches('/'),
        request.uri(),
    )
    .parse::<http::Uri>()
    .map_err(|e| (http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    request.headers_mut().remove(HOST);
    state
        .client
        .request(request)
        .map_err(|e| (http::StatusCode::BAD_GATEWAY, e.to_string()))
        .await
}

async fn watch(state: Arc<State>, interval: Duration) {
    #[tracing::instrument(err, skip(state))]
    async fn get(state: &State, path: &str) -> anyhow::Result<Bytes> {
        let uri = format!("{}{path}", state.upstream.to_string().trim_end_matches('/')).parse()?;
        let response = state.client.get(uri).await?;
        anyhow::ensure!(response.status().is_success(), "{response:?}");
        let body = response
            .collect()
            .map_ok(http_body_util::Collected::to_bytes)
            .await?;
        Ok(body)
    }

    async fn fetch(state: &State) -> anyhow::Result<schemas::List<schemas::Model>> {
        let (body_0, body_1) =
            futures::future::try_join(get(state, "/v1/models"), get(state, "/metrics")).await?;

        let mut models = serde_json::from_slice::<schemas::List<schemas::Model>>(&body_0)?;

        let (running, pending) = {
            let mut body = str::from_utf8(&body_1)?.replace("\r\n", "\n");
            body.push_str("# EOF\n");
            let (_, exposition) = openmetrics_nom::exposition(body.as_str())
                .finish()
                .map_err(nom::error::Error::<&str>::cloned)?;
            let (_, metricset) = &exposition.metricset;
            metricset
                .metricfamily
                .iter()
                .flat_map(|(_, metricfamily)| &metricfamily.metric)
                .flat_map(|(_, metric)| &metric.sample)
                .fold((None, None), |(mut running, mut pending), (_, sample)| {
                    if let Ok(v) = sample.number.parse::<f64>() {
                        match sample.metricname {
                            "vllm:num_requests_running" => {
                                *running.get_or_insert_default() += v as u64;
                            }
                            "vllm:num_requests_waiting" => {
                                *pending.get_or_insert_default() += v as u64;
                            }
                            _ => (),
                        }
                    }
                    (running, pending)
                })
        };

        for model in &mut models.data {
            model.running = running;
            model.pending = pending;
        }

        Ok(models)
    }

    let mut interval = tokio::time::interval(interval);
    loop {
        interval.tick().await;
        *state.models.write().unwrap() = fetch(&state).map(Result::ok).await;
    }
}
