use tonic_envoy::envoy::config::cluster::v3 as cluster_v3;
use tonic_envoy::envoy::config::core::v3 as core_v3;
use tonic_envoy::envoy::config::route::v3 as route_v3;
use tonic_envoy::envoy::extensions::upstreams::http::v3 as http_v3;
use tonic_envoy::envoy::service::discovery::v3 as discovery_v3;

pub fn discovery_response<T>(
    version_info: usize,
    resources: &[T],
) -> Result<discovery_v3::DiscoveryResponse, prost::EncodeError>
where
    T: prost::Name,
{
    Ok(discovery_v3::DiscoveryResponse {
        version_info: version_info.to_string(),
        resources: resources
            .iter()
            .map(crate::pbjson::from_msg)
            .collect::<Result<_, _>>()?,
        type_url: T::type_url(),
        nonce: uuid::Uuid::new_v4().to_string(),
        ..discovery_v3::DiscoveryResponse::default()
    })
}

pub fn http2_protocol_options(cluster: &mut cluster_v3::Cluster) -> Result<(), prost::EncodeError> {
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
        crate::pbjson::from_msg(&http_protocol_options)?,
    );
    Ok(())
}

pub fn direct_response_json<T>(
    route: &mut route_v3::Route,
    body: &T,
) -> Result<(), serde_json::Error>
where
    T: serde::Serialize,
{
    route
        .response_headers_to_add
        .push(core_v3::HeaderValueOption {
            header: Some(core_v3::HeaderValue {
                key: "content-type".to_owned(),
                value: "application/json".to_owned(),
                ..core_v3::HeaderValue::default()
            }),
            ..core_v3::HeaderValueOption::default()
        });
    let action =
        crate::get_or_insert_default!(&mut route.action, route_v3::route::Action::DirectResponse);
    action.status = http::StatusCode::OK.as_u16() as _;
    action.body.get_or_insert_default().specifier = Some(
        core_v3::data_source::Specifier::InlineString(serde_json::to_string(body)?),
    );
    Ok(())
}

pub fn max_direct_response_body_size_bytes(route_configuration: &mut route_v3::RouteConfiguration) {
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
    route_configuration.max_direct_response_body_size_bytes = max_direct_response_body_size_bytes
        .map(|max_direct_response_body_size_bytes| {
            pbjson_types::UInt32Value::from(max_direct_response_body_size_bytes as u32)
        });
}
