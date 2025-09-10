use tonic_envoy::envoy::config::core::v3 as core_v3;
use tonic_envoy::envoy::config::route::v3 as route_v3;

pub fn patch_max_direct_response_body_size_bytes(
    route_configuration: &mut route_v3::RouteConfiguration,
) {
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
        .map(|max_direct_response_body_size_bytes| max_direct_response_body_size_bytes as _);
}
