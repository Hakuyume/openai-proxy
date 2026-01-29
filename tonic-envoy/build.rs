use quote::ToTokens;
use std::collections::{BTreeMap, BTreeSet};
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
        .compile_well_known_types(true)
        .extern_path(".google.protobuf", "::pbjson_types")
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

    pbjson_build::Builder::new()
        .register_descriptors(&fs::read(out_dir.join("envoy_descriptor.bin"))?)?
        .build(&["."])?;

    let mut module = Module::default();
    for entry in fs::read_dir(&out_dir)? {
        let entry = entry?;
        if let Some(stem) = entry.path().file_stem().and_then(OsStr::to_str)
            && entry.path().extension() == Some(OsStr::new("rs"))
        {
            let mut split = stem.split('.');
            match split.next_back() {
                Some("serde") => {
                    split
                        .fold(&mut module, |module, ident| {
                            module.modules.entry(ident.to_owned()).or_default()
                        })
                        .include
                        .insert(Include::Pbjson(stem.to_owned()));
                }
                ident => {
                    split
                        .chain(ident)
                        .fold(&mut module, |module, ident| {
                            module.modules.entry(ident.to_owned()).or_default()
                        })
                        .include
                        .insert(Include::Tonic(stem.to_owned()));
                }
            }
        }
    }
    module
        .modules
        .retain(|ident, _| ["envoy", "xds"].contains(&ident.as_str()));
    if let Some(module) = module
        .modules
        .get_mut("envoy")
        .and_then(|module| module.modules.get_mut("service"))
    {
        fn filter(module: &mut Module) {
            module
                .include
                .retain(|include| !matches!(include, Include::Pbjson(_)));
            for module in module.modules.values_mut() {
                filter(module);
            }
        }
        filter(module);
    }
    fs::write(
        out_dir.join("include.rs"),
        module.into_token_stream().to_string(),
    )?;

    Ok(())
}

#[derive(Debug, Default)]
struct Module {
    include: BTreeSet<Include>,
    modules: BTreeMap<String, Module>,
}

#[derive(Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Include {
    Pbjson(String),
    Tonic(String),
}

impl quote::ToTokens for Module {
    fn to_tokens(&self, tokens: &mut proc_macro2::TokenStream) {
        for include in &self.include {
            match include {
                Include::Pbjson(include) => {
                    let include = format!("/{include}.rs");
                    tokens.extend(quote::quote! {
                        ::std::include!(::std::concat!(::std::env!("OUT_DIR"), #include));
                    });
                }
                Include::Tonic(include) => {
                    tokens.extend(quote::quote! {
                        ::tonic::include_proto!(#include);
                    });
                }
            }
        }
        for (ident, module) in &self.modules {
            let ident = quote::format_ident!("{ident}");
            tokens.extend(quote::quote! {
                pub mod #ident { #module }
            });
        }
    }
}
