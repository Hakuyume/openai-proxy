mod aggregated_discovery_service;

use clap::Parser;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

#[derive(Parser)]
struct Args {
    #[clap(long, default_value_t = 50051)]
    port: u16,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    let reflection = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(tonic_envoy::FILE_DESCRIPTOR_SET)
        .build_v1()?;

    let (ads_tx, ads_service) = aggregated_discovery_service::service();

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            interval.tick().await;
            let _ = ads_tx.send(Arc::new(aggregated_discovery_service::Config::new(
                [("1.2.3.4".parse().unwrap(), 80, "a")],
                "model",
                "local_route",
            )?));

            interval.tick().await;
            let _ = ads_tx.send(Arc::new(aggregated_discovery_service::Config::new(
                [
                    ("1.2.3.4".parse().unwrap(), 80, "a"),
                    ("1.2.3.4".parse().unwrap(), 80, "b"),
                    ("5.6.7.8".parse().unwrap(), 80, "b"),
                ],
                "model",
                "local_route",
            )?));
        }
        #[allow(unreachable_code)]
        Ok::<_, prost::EncodeError>(())
    });

    tonic::transport::Server::builder()
        .layer(tower_http::trace::TraceLayer::new_for_grpc())
        .add_service(reflection)
        .add_service(ads_service)
        .serve((Ipv4Addr::UNSPECIFIED, args.port).into())
        .await?;

    Ok(())
}
