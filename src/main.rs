mod auth;
mod azure;
mod misc;
mod mux;

use axum::{Router, routing};
use bytes::Bytes;
use clap::Parser;
use http_body_util::Full;
use serde::Deserialize;
use std::future;
use std::net::SocketAddr;
use std::str::FromStr;

#[derive(Parser)]
struct Args {
    #[clap(long)]
    bind: SocketAddr,
    #[clap(long)]
    config: Config,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Config {
    Auth(auth::Config),
    Azure(azure::Config),
    Mux(mux::Config),
}

impl FromStr for Config {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(s).map_err(|e| e.to_string())
    }
}

fn service(pool: &crate::misc::pool::Pool<Full<Bytes>>, config: Config) -> anyhow::Result<Router> {
    match config {
        Config::Auth(config) => auth::service(pool, config),
        Config::Azure(config) => azure::service(pool, config),
        Config::Mux(config) => mux::service(pool, config),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let args = Args::parse();
    tracing::info!(config = ?args.config);

    let pool = misc::pool::Pool::new()?;

    let prometheus_recorder =
        metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
    let prometheus_handle = prometheus_recorder.handle();
    metrics_util::layers::Stack::new(prometheus_recorder)
        .push(metrics_util::layers::PrefixLayer::new(env!(
            "CARGO_BIN_NAME"
        )))
        .install()?;

    let service = service(&pool, args.config)?
        .layer(
            tower_http::trace::TraceLayer::new_for_http()
                .make_span_with(tower_http::trace::DefaultMakeSpan::new().include_headers(true)),
        )
        .layer(
            tower_http::sensitive_headers::SetSensitiveHeadersLayer::new([
                http::header::AUTHORIZATION,
            ]),
        )
        .route("/health", routing::get(|| future::ready(())))
        .route(
            "/metrics",
            routing::get(move || future::ready(prometheus_handle.render())),
        );

    misc::metrics::serve(service, args.bind).await?;
    Ok(())
}
