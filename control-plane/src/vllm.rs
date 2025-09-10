mod generator;
mod resolver;

use crate::aggregated_discovery_service;
use clap::Parser;
use futures::{StreamExt, TryFutureExt};
use std::net::Ipv4Addr;
use std::pin;
use std::time::Duration;

#[derive(Parser)]
pub(super) struct Args {
    #[clap(long, default_value_t = 50051)]
    port: u16,
    #[clap(long)]
    upstream: http::Uri,
    #[clap(long, value_parser = humantime::parse_duration)]
    interval: Duration,
    #[clap(long)]
    route_config_name: String,
    #[clap(long)]
    cluster_name: String,
    #[clap(long, value_parser = humantime::parse_duration)]
    timeout: Option<Duration>,
    #[clap(long, value_parser = humantime::parse_duration)]
    idle_timeout: Option<Duration>,
}

pub(super) async fn main(args: Args) -> anyhow::Result<()> {
    let reflection_service = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(tonic_health::pb::FILE_DESCRIPTOR_SET)
        .register_encoded_file_descriptor_set(tonic_envoy::FILE_DESCRIPTOR_SET)
        .build_v1()?;
    let (health_reporter, health_service) = tonic_health::server::health_reporter();
    let (mut ads_reporter, ads_service) = aggregated_discovery_service::service();

    health_reporter
        .set_serving::<aggregated_discovery_service::Service>()
        .await;

    futures::future::try_join(
        async {
            tonic::transport::Server::builder()
                .layer(tower_http::trace::TraceLayer::new_for_grpc())
                .add_service(reflection_service)
                .add_service(health_service)
                .add_service(ads_service)
                .serve((Ipv4Addr::UNSPECIFIED, args.port).into())
                .await?;
            Ok(())
        },
        async {
            let resolver = resolver::Resolver::new(args.upstream, args.interval)?;
            let mut stream = pin::pin!(resolver.watch());
            while let Some(models) = stream.next().await {
                let generator = generator::Generator {
                    models: &models,
                    route_config_name: &args.route_config_name,
                    cluster_name: &args.cluster_name,
                    timeout: args.timeout,
                    idle_timeout: args.idle_timeout,
                };
                ads_reporter.route_configurations(vec![generator.route_configuration()?])?;
            }
            Ok(())
        },
    )
    .map_ok(|_| ())
    .await
}
