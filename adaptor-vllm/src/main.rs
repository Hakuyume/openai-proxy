use axum::response::IntoResponse;
use axum::{Json, Router, extract, routing};
use bytes::Bytes;
use clap::Parser;
use futures::{FutureExt, TryFutureExt};
use http::header::HOST;
use http_body_util::BodyExt;
use nom::Finish;
use std::net::Ipv4Addr;
use tokio::net::TcpListener;

#[derive(Parser)]
struct Args {
    #[clap(long, default_value_t = 80)]
    port: u16,
    #[clap(long)]
    upstream: http::Uri,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    let client = misc::hyper::client(misc::hyper::tls_config()?, None, false);
    let app = Router::new()
        .route("/v1/models", routing::get(list_models))
        .fallback(fallback)
        .with_state((client, args.upstream))
        .layer(tower_http::trace::TraceLayer::new_for_http());

    let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, args.port)).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(tokio::signal::ctrl_c().map(|_| ()))
        .await?;

    Ok(())
}

async fn list_models(
    extract::State((client, upstream)): extract::State<(
        misc::hyper::Client<axum::body::Body>,
        http::Uri,
    )>,
) -> Result<Json<schemas::List<schemas::Model>>, axum::response::Response> {
    async fn get(
        client: &misc::hyper::Client<axum::body::Body>,
        upstream: &http::Uri,
        path: &str,
    ) -> Result<Bytes, axum::response::Response> {
        let uri = format!("{}{path}", upstream.to_string().trim_end_matches('/'))
            .parse::<http::Uri>()
            .map_err(|e| {
                (http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
            })?;
        let response = client
            .get(uri)
            .map_err(|e| (http::StatusCode::BAD_GATEWAY, e.to_string()).into_response())
            .await?;
        if response.status().is_success() {
            response
                .collect()
                .map_ok(http_body_util::Collected::to_bytes)
                .map_err(|e| (http::StatusCode::BAD_GATEWAY, e.to_string()).into_response())
                .await
        } else {
            Err(response.into_response())
        }
    }

    let (body_0, body_1) = futures::future::try_join(
        get(&client, &upstream, "/v1/models"),
        get(&client, &upstream, "/metrics"),
    )
    .await?;

    let mut models = serde_json::from_slice::<schemas::List<schemas::Model>>(&body_0)
        .map_err(|e| (http::StatusCode::BAD_GATEWAY, e.to_string()).into_response())?;

    let (running, pending) = {
        let mut body = str::from_utf8(&body_1)
            .map_err(|e| (http::StatusCode::BAD_GATEWAY, e.to_string()).into_response())?
            .replace("\r\n", "\n");
        body.push_str("# EOF\n");
        let (_, exposition) = openmetrics_nom::exposition::<_, nom::error::Error<_>>(body.as_str())
            .finish()
            .map_err(|e| (http::StatusCode::BAD_GATEWAY, e.to_string()).into_response())?;
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

    Ok(Json(models))
}

async fn fallback(
    extract::State((client, upstream)): extract::State<(
        misc::hyper::Client<axum::body::Body>,
        http::Uri,
    )>,
    mut request: http::Request<axum::body::Body>,
) -> Result<http::Response<hyper::body::Incoming>, (http::StatusCode, String)> {
    *request.uri_mut() = format!(
        "{}{}",
        upstream.to_string().trim_end_matches('/'),
        request.uri(),
    )
    .parse::<http::Uri>()
    .map_err(|e| (http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    request.headers_mut().remove(HOST);
    client
        .request(request)
        .map_err(|e| (http::StatusCode::BAD_GATEWAY, e.to_string()))
        .await
}
