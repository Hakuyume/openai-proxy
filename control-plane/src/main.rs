mod aggregated_discovery_service;
mod resolver;

use clap::Parser;
use futures::{StreamExt, TryFutureExt};
use sha2::{Digest, Sha224};
use std::collections::{BTreeSet, HashMap};
use std::iter;
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;
use tonic_envoy::envoy::config::cluster::v3 as cluster_v3;
use tonic_envoy::envoy::config::core::v3 as core_v3;
use tonic_envoy::envoy::config::endpoint::v3 as endpoint_v3;
use tonic_envoy::envoy::config::route::v3 as route_v3;
use tonic_envoy::envoy::extensions::upstreams::http::v3 as http_v3;
use tonic_envoy::envoy::r#type::matcher::v3 as matcher_v3;

#[derive(Parser)]
struct Args {
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let args = Args::parse();

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

                let generator = Generator {
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

struct Generator<'a> {
    upstream: &'a [resolver::Upstream],
    state: &'a HashMap<usize, Vec<resolver::Endpoint>>,
    route_config_name: &'a String,
    metadata_namespace: &'a String,
    timeout: Option<Duration>,
    idle_timeout: Option<Duration>,
}

impl Generator<'_> {
    fn cluster_name(i: usize, ip: IpAddr) -> String {
        format!(
            "cluster_{}",
            hex::encode(Sha224::digest(format!("{i}:{ip}")))
        )
    }

    fn clusters(&self) -> anyhow::Result<Vec<cluster_v3::Cluster>> {
        let mut clusters = self
            .upstream
            .iter()
            .enumerate()
            .flat_map(|(i, upstream)| {
                let mut endpoints = self.state.get(&i).into_iter().flatten().collect::<Vec<_>>();
                endpoints.sort_unstable_by_key(|endpoint| endpoint.ip);
                endpoints
                    .into_iter()
                    .map(move |endpoint| Self::cluster(i, upstream, endpoint.ip))
            })
            .collect::<Result<Vec<_>, _>>()?;
        clusters.sort_unstable_by_key(|cluster| cluster.name.clone());
        Ok(clusters)
    }

    fn cluster(
        i: usize,
        upstream: &resolver::Upstream,
        ip: IpAddr,
    ) -> anyhow::Result<cluster_v3::Cluster> {
        let name = Self::cluster_name(i, ip);
        let port = upstream.uri.port_u16().unwrap_or(80);
        let address = core_v3::address::Address::SocketAddress(core_v3::SocketAddress {
            address: ip.to_string(),
            port_specifier: Some(core_v3::socket_address::PortSpecifier::PortValue(port as _)),
            ..core_v3::SocketAddress::default()
        });
        let lb_endpoint = endpoint_v3::LbEndpoint {
            host_identifier: Some(endpoint_v3::lb_endpoint::HostIdentifier::Endpoint(
                endpoint_v3::Endpoint {
                    address: Some(core_v3::Address {
                        address: Some(address),
                    }),
                    ..endpoint_v3::Endpoint::default()
                },
            )),
            ..endpoint_v3::LbEndpoint::default()
        };
        let mut cluster = cluster_v3::Cluster {
            name: name.clone(),
            cluster_discovery_type: Some(cluster_v3::cluster::ClusterDiscoveryType::Type(
                cluster_v3::cluster::DiscoveryType::Static as _,
            )),
            load_assignment: Some(endpoint_v3::ClusterLoadAssignment {
                cluster_name: name.clone(),
                endpoints: vec![endpoint_v3::LocalityLbEndpoints {
                    lb_endpoints: vec![lb_endpoint],
                    ..endpoint_v3::LocalityLbEndpoints::default()
                }],
                ..endpoint_v3::ClusterLoadAssignment::default()
            }),
            ..cluster_v3::Cluster::default()
        };

        if upstream.http2_only {
            use http_v3::http_protocol_options::explicit_http_config::ProtocolConfig;
            let explicit_http_config = http_v3::http_protocol_options::ExplicitHttpConfig {
                protocol_config: Some(ProtocolConfig::Http2ProtocolOptions(
                    core_v3::Http2ProtocolOptions::default(),
                )),
            };
            let http_protocol_options = http_v3::HttpProtocolOptions {
                upstream_protocol_options: Some(
                    http_v3::http_protocol_options::UpstreamProtocolOptions::ExplicitHttpConfig(
                        explicit_http_config,
                    ),
                ),
                ..http_v3::HttpProtocolOptions::default()
            };
            cluster.typed_extension_protocol_options.insert(
                "envoy.extensions.upstreams.http.v3.HttpProtocolOptions".to_owned(),
                prost_types::Any::from_msg(&http_protocol_options)?,
            );
        }

        Ok(cluster)
    }

    fn route_configuration(&self) -> anyhow::Result<route_v3::RouteConfiguration> {
        let mut route_configuration = route_v3::RouteConfiguration {
            name: self.route_config_name.clone(),
            virtual_hosts: vec![self.virtual_host()?],
            ..route_v3::RouteConfiguration::default()
        };

        let max_direct_response_body_size_bytes = route_configuration
            .virtual_hosts
            .iter()
            .flat_map(|virtual_host| &virtual_host.routes)
            .filter_map(|route| match &route.action {
                Some(route_v3::route::Action::DirectResponse(route_v3::DirectResponseAction {
                    body:
                        Some(core_v3::DataSource {
                            specifier: Some(core_v3::data_source::Specifier::InlineString(body)),
                            ..
                        }),
                    ..
                })) => Some(body.len()),
                _ => None,
            })
            .max();
        route_configuration.max_direct_response_body_size_bytes =
            max_direct_response_body_size_bytes.map(|max_direct_response_body_size_bytes| {
                max_direct_response_body_size_bytes as _
            });

        Ok(route_configuration)
    }

    fn virtual_host(&self) -> anyhow::Result<route_v3::VirtualHost> {
        let model_ids = self
            .state
            .values()
            .flatten()
            .flat_map(|endpoint| &endpoint.models)
            .map(|model| &model.id)
            .collect::<BTreeSet<_>>();
        Ok(route_v3::VirtualHost {
            name: "local_service".to_owned(),
            domains: vec!["*".to_owned()],
            routes: iter::once(self.route_list_models())
                .chain(
                    model_ids
                        .into_iter()
                        .map(|model_id| self.route_model(model_id.clone())),
                )
                .collect::<Result<_, _>>()?,
            ..route_v3::VirtualHost::default()
        })
    }

    fn route_list_models(&self) -> anyhow::Result<route_v3::Route> {
        let mut data = self
            .state
            .values()
            .flatten()
            .flat_map(|endpoint| &endpoint.models)
            .collect::<Vec<_>>();
        data.sort_unstable_by_key(|model| &model.id);
        let body = serde_json::to_string(&schemas::List { data })?;

        Ok(route_v3::Route {
            r#match: Some(route_v3::RouteMatch {
                path_specifier: Some(route_v3::route_match::PathSpecifier::Path(
                    "/v1/models".to_owned(),
                )),
                headers: vec![route_v3::HeaderMatcher {
                    name: ":method".to_owned(),
                    header_match_specifier: Some(
                        route_v3::header_matcher::HeaderMatchSpecifier::StringMatch(
                            matcher_v3::StringMatcher {
                                match_pattern: Some(
                                    matcher_v3::string_matcher::MatchPattern::Exact(
                                        "GET".to_owned(),
                                    ),
                                ),
                                ..matcher_v3::StringMatcher::default()
                            },
                        ),
                    ),
                    ..route_v3::HeaderMatcher::default()
                }],
                ..route_v3::RouteMatch::default()
            }),
            response_headers_to_add: vec![core_v3::HeaderValueOption {
                header: Some(core_v3::HeaderValue {
                    key: "content-type".to_owned(),
                    value: "application/json".to_owned(),
                    ..core_v3::HeaderValue::default()
                }),
                ..core_v3::HeaderValueOption::default()
            }],
            action: Some(route_v3::route::Action::DirectResponse(
                route_v3::DirectResponseAction {
                    status: http::StatusCode::OK.as_u16() as _,
                    body: Some(core_v3::DataSource {
                        specifier: Some(core_v3::data_source::Specifier::InlineString(body)),
                        ..core_v3::DataSource::default()
                    }),
                },
            )),
            ..route_v3::Route::default()
        })
    }

    fn route_model(&self, model_id: String) -> anyhow::Result<route_v3::Route> {
        let mut endpoints = self
            .state
            .iter()
            .flat_map(|(i, endpoints)| {
                let model_id = &model_id;
                endpoints.iter().filter_map(move |endpoint| {
                    let pending = endpoint
                        .models
                        .iter()
                        .filter_map(|model| {
                            (&model.id == model_id).then_some(model.pending.unwrap_or_default())
                        })
                        .collect::<Vec<_>>();
                    (!pending.is_empty()).then_some((*i, endpoint.ip, pending))
                })
            })
            .collect::<Vec<_>>();
        endpoints.sort_unstable();
        let pending_max = endpoints
            .iter()
            .flat_map(|(_, _, pending)| pending)
            .copied()
            .max()
            .unwrap_or_default();

        Ok(route_v3::Route {
            r#match: Some(route_v3::RouteMatch {
                path_specifier: Some(route_v3::route_match::PathSpecifier::Prefix("/".to_owned())),
                dynamic_metadata: vec![matcher_v3::MetadataMatcher {
                    filter: self.metadata_namespace.clone(),
                    path: vec![matcher_v3::metadata_matcher::PathSegment {
                        segment: Some(matcher_v3::metadata_matcher::path_segment::Segment::Key(
                            "model".to_owned(),
                        )),
                    }],
                    value: Some(matcher_v3::ValueMatcher {
                        match_pattern: Some(matcher_v3::value_matcher::MatchPattern::StringMatch(
                            matcher_v3::StringMatcher {
                                match_pattern: Some(
                                    matcher_v3::string_matcher::MatchPattern::Exact(model_id),
                                ),
                                ..matcher_v3::StringMatcher::default()
                            },
                        )),
                    }),
                    ..matcher_v3::MetadataMatcher::default()
                }],
                ..route_v3::RouteMatch::default()
            }),
            action: Some(route_v3::route::Action::Route(route_v3::RouteAction {
                cluster_specifier: Some(
                    route_v3::route_action::ClusterSpecifier::WeightedClusters(
                        route_v3::WeightedCluster {
                            clusters: endpoints
                                .into_iter()
                                .map(
                                    |(i, ip, pending)| route_v3::weighted_cluster::ClusterWeight {
                                        name: Self::cluster_name(i, ip),
                                        weight: Some(
                                            pending
                                                .into_iter()
                                                .map(|pending| (1 + pending_max) / (1 + pending))
                                                .sum::<u64>()
                                                as _,
                                        ),
                                        ..route_v3::weighted_cluster::ClusterWeight::default()
                                    },
                                )
                                .collect(),
                            ..route_v3::WeightedCluster::default()
                        },
                    ),
                ),
                timeout: self.timeout.map(TryInto::try_into).transpose()?,
                idle_timeout: self.idle_timeout.map(TryInto::try_into).transpose()?,
                ..route_v3::RouteAction::default()
            })),
            ..route_v3::Route::default()
        })
    }
}
