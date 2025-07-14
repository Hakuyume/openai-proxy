#![allow(clippy::doc_lazy_continuation)]
#![allow(clippy::doc_overindented_list_items)]
#![allow(clippy::large_enum_variant)]
#![allow(rustdoc::bare_urls)]

pub mod envoy {
    pub mod config {
        pub mod accesslog {
            pub mod v3 {
                tonic::include_proto!("envoy.config.accesslog.v3");
            }
        }
        pub mod cluster {
            pub mod v3 {
                tonic::include_proto!("envoy.config.cluster.v3");
            }
        }
        pub mod core {
            pub mod v3 {
                tonic::include_proto!("envoy.config.core.v3");
            }
        }
        pub mod endpoint {
            pub mod v3 {
                tonic::include_proto!("envoy.config.endpoint.v3");
            }
        }
        pub mod route {
            pub mod v3 {
                tonic::include_proto!("envoy.config.route.v3");
            }
        }
        pub mod trace {
            pub mod v3 {
                tonic::include_proto!("envoy.config.trace.v3");
            }
        }
    }
    pub mod data {
        pub mod accesslog {
            pub mod v3 {
                tonic::include_proto!("envoy.data.accesslog.v3");
            }
        }
    }
    pub mod extensions {
        pub mod filters {
            pub mod http {
                pub mod ext_proc {
                    pub mod v3 {
                        tonic::include_proto!("envoy.extensions.filters.http.ext_proc.v3");
                    }
                }
            }
            pub mod network {
                pub mod http_connection_manager {
                    pub mod v3 {
                        tonic::include_proto!(
                            "envoy.extensions.filters.network.http_connection_manager.v3"
                        );
                    }
                }
            }
        }
        pub mod upstreams {
            pub mod http {
                pub mod v3 {
                    tonic::include_proto!("envoy.extensions.upstreams.http.v3");
                }
            }
        }
    }
    pub mod service {
        pub mod discovery {
            pub mod v3 {
                tonic::include_proto!("envoy.service.discovery.v3");
            }
        }
        pub mod ext_proc {
            pub mod v3 {
                tonic::include_proto!("envoy.service.ext_proc.v3");
            }
        }
    }
    pub mod r#type {
        pub mod http {
            pub mod v3 {
                tonic::include_proto!("envoy.r#type.http.v3");
            }
        }
        pub mod matcher {
            pub mod v3 {
                tonic::include_proto!("envoy.r#type.matcher.v3");
            }
        }
        pub mod metadata {
            pub mod v3 {
                tonic::include_proto!("envoy.r#type.metadata.v3");
            }
        }
        pub mod tracing {
            pub mod v3 {
                tonic::include_proto!("envoy.r#type.tracing.v3");
            }
        }
        pub mod v3 {
            tonic::include_proto!("envoy.r#type.v3");
        }
    }
}
pub mod xds {
    pub mod core {
        pub mod v3 {
            tonic::include_proto!("xds.core.v3");
        }
    }
    pub mod r#type {
        pub mod matcher {
            pub mod v3 {
                tonic::include_proto!("xds.r#type.matcher.v3");
            }
        }
    }
}

pub const FILE_DESCRIPTOR_SET: &[u8] = tonic::include_file_descriptor_set!("envoy_descriptor");
