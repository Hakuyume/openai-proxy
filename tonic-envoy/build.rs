use std::env;
use std::path::PathBuf;

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    let mut config = prost_build::Config::new();
    config
        .enable_type_names()
        .type_name_domain(["."], "type.googleapis.com");

    tonic_build::configure()
        .bytes(["."])
        .extern_path(".google.rpc", "::tonic_types")
        .file_descriptor_set_path(out_dir.join("envoy_descriptor.bin"))
        .compile_protos_with_config(
            config,
            &[
                "envoy/api/envoy/config/cluster/v3/cluster.proto",
                "envoy/api/envoy/config/route/v3/route.proto",
                "envoy/api/envoy/extensions/upstreams/http/v3/http_protocol_options.proto",
                "envoy/api/envoy/service/discovery/v3/ads.proto",
                "envoy/api/envoy/service/discovery/v3/discovery.proto",
                "envoy/api/envoy/service/ext_proc/v3/external_processor.proto",
            ],
            &[
                "api-common-protos",
                "envoy/api",
                "protoc-gen-validate",
                "xds",
            ],
        )
        .unwrap();
}
