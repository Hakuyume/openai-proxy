use quote::ToTokens;
use std::collections::BTreeMap;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = PathBuf::from(env::var("OUT_DIR")?);

    let mut config = prost_build::Config::new();
    config
        .enable_type_names()
        .type_name_domain(["."], "type.googleapis.com");
    tonic_prost_build::configure()
        .bytes(".")
        .extern_path(".google.rpc", "::tonic_types")
        .file_descriptor_set_path(out_dir.join("envoy_descriptor.bin"))
        .compile_with_config(
            config,
            &[
                "envoy/api/envoy/config/cluster/v3/cluster.proto",
                "envoy/api/envoy/config/route/v3/route.proto",
                "envoy/api/envoy/extensions/upstreams/http/v3/http_protocol_options.proto",
                "envoy/api/envoy/service/discovery/v3/ads.proto",
                "envoy/api/envoy/service/ext_proc/v3/external_processor.proto",
            ],
            &[
                "api-common-protos",
                "envoy/api",
                "protoc-gen-validate",
                "xds",
            ],
        )?;

    let mut module = Module::default();
    for entry in fs::read_dir(&out_dir)? {
        let entry = entry?;
        if let Some(stem) = entry.path().file_stem().and_then(OsStr::to_str)
            && entry.path().extension() == Some(OsStr::new("rs"))
        {
            stem.split('.')
                .fold(&mut module, |module, ident| {
                    module.modules.entry(ident.to_owned()).or_default()
                })
                .include_proto = Some(stem.to_owned());
        }
    }
    module
        .modules
        .retain(|ident, _| ["envoy", "xds"].contains(&ident.as_str()));
    fs::write(
        out_dir.join("include_proto.rs"),
        module.into_token_stream().to_string(),
    )?;

    Ok(())
}

#[derive(Debug, Default)]
struct Module {
    include_proto: Option<String>,
    modules: BTreeMap<String, Module>,
}

impl quote::ToTokens for Module {
    fn to_tokens(&self, tokens: &mut proc_macro2::TokenStream) {
        if let Some(include_proto) = &self.include_proto {
            tokens.extend(quote::quote! {
                ::tonic::include_proto!(#include_proto);
            });
        }
        for (ident, module) in &self.modules {
            let ident = quote::format_ident!("{ident}");
            tokens.extend(quote::quote! {
                pub mod #ident { #module }
            });
        }
    }
}
