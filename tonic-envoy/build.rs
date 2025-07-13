use std::env;
use std::path::PathBuf;

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    tonic_build::configure()
        .bytes(["."])
        .extern_path(".google.rpc", "::tonic_types")
        .file_descriptor_set_path(out_dir.join("envoy_descriptor.bin"))
        .compile_protos(
            &[
                "envoy/api/envoy/service/discovery/v3/ads.proto",
                "envoy/api/envoy/service/discovery/v3/discovery.proto",
            ],
            &[
                "api-common-protos",
                "envoy/api",
                "envoy/api/envoy/service/discovery/v3",
                "protoc-gen-validate",
                "xds",
            ],
        )
        .unwrap();
}
