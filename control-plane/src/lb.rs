mod generator;
mod resolver;

use crate::aggregated_discovery_service;
use clap::Parser;
use futures::{StreamExt, TryFutureExt};
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::time::Duration;

#[derive(Parser)]
pub(super) struct Args {
    #[clap(long, default_value_t = 50051)]
    port: u16,
    #[clap(long)]
    upstream: Vec<resolver::Upstream>,
    #[clap(long)]
    route_config_name: String,
    #[clap(long)]
    metadata_namespace: String,
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
            let resolver = resolver::Resolver::new()?;
            let mut stream = futures::stream::select_all(args.upstream.iter().enumerate().map(
                |(i, upstream)| {
                    resolver
                        .watch(upstream)
                        .map(move |endpoints| (i, endpoints))
                        .boxed()
                },
            ));
            let mut state = HashMap::new();
            while let Some((i, endpoints)) = stream.next().await {
                state.insert(i, endpoints);

                let generator = generator::Generator {
                    upstream: &args.upstream,
                    state: &state,
                    route_config_name: &args.route_config_name,
                    metadata_namespace: &args.metadata_namespace,
                    timeout: args.timeout,
                    idle_timeout: args.idle_timeout,
                };
                ads_reporter.clusters(generator.clusters()?)?;
                ads_reporter.route_configurations(vec![generator.route_configuration()?])?;
            }
            Ok(())
        },
    )
    .map_ok(|_| ())
    .await
}
