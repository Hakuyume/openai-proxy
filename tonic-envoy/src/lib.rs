#![allow(clippy::doc_lazy_continuation)]
#![allow(clippy::doc_overindented_list_items)]
#![allow(clippy::large_enum_variant)]
#![allow(rustdoc::bare_urls)]
#![allow(rustdoc::invalid_html_tags)]

include!(concat!(env!("OUT_DIR"), "/include.rs"));

pub const FILE_DESCRIPTOR_SET: &[u8] = tonic::include_file_descriptor_set!("envoy_descriptor");
