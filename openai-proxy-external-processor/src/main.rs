use clap::Parser;
use futures::stream::BoxStream;
use futures::{StreamExt, TryStreamExt};
use serde::Deserialize;
use std::net::Ipv4Addr;
use tonic_envoy::envoy::config::core::v3 as core_v3;
use tonic_envoy::envoy::extensions::filters::http::ext_proc::v3::{
    ProcessingMode, processing_mode,
};
use tonic_envoy::envoy::service::ext_proc::v3 as ext_proc_v3;
use tonic_envoy::envoy::service::ext_proc::v3::external_processor_server::{
    ExternalProcessor, ExternalProcessorServer,
};

#[derive(Parser)]
struct Args {
    #[clap(long, default_value_t = 50051)]
    port: u16,
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

    health_reporter
        .set_serving::<ExternalProcessorServer<Server>>()
        .await;

    tonic::transport::Server::builder()
        .layer(tower_http::trace::TraceLayer::new_for_grpc())
        .add_service(reflection_service)
        .add_service(health_service)
        .add_service(ExternalProcessorServer::new(Server))
        .serve((Ipv4Addr::UNSPECIFIED, args.port).into())
        .await?;

    Ok(())
}

struct Server;

#[tonic::async_trait]
impl ExternalProcessor for Server {
    type ProcessStream = BoxStream<'static, Result<ext_proc_v3::ProcessingResponse, tonic::Status>>;
    async fn process(
        &self,
        request: tonic::Request<tonic::Streaming<ext_proc_v3::ProcessingRequest>>,
    ) -> Result<tonic::Response<Self::ProcessStream>, tonic::Status> {
        let stream = request.into_inner().map_ok(|request| {
            {
                let mut request = request.clone();
                if let Some(
                    ext_proc_v3::processing_request::Request::RequestBody(body)
                    | ext_proc_v3::processing_request::Request::ResponseBody(body),
                ) = &mut request.request
                {
                    body.body = "..".into();
                }
                tracing::info!(?request);
            }
            let (response, mode_override) = match request.request {
                Some(ext_proc_v3::processing_request::Request::RequestHeaders(headers)) => (
                    Some(ext_proc_v3::processing_response::Response::RequestHeaders(
                        ext_proc_v3::HeadersResponse::default(),
                    )),
                    if headers.headers.is_some_and(|headers| {
                        headers.headers.iter().any(|header| {
                            header.key == "content-type"
                                && &header.raw_value[..] == b"application/json"
                        })
                    }) {
                        Some(ProcessingMode {
                            request_body_mode: processing_mode::BodySendMode::Buffered as _,
                            ..ProcessingMode::default()
                        })
                    } else {
                        None
                    },
                ),
                Some(ext_proc_v3::processing_request::Request::ResponseHeaders(_)) => (
                    Some(ext_proc_v3::processing_response::Response::ResponseHeaders(
                        ext_proc_v3::HeadersResponse::default(),
                    )),
                    None,
                ),
                Some(ext_proc_v3::processing_request::Request::RequestBody(body)) => {
                    #[derive(Deserialize)]
                    struct Body {
                        model: String,
                    }

                    let mut response = ext_proc_v3::BodyResponse::default();
                    if let Ok(Body { model }) = serde_json::from_slice(&body.body) {
                        let response = response.response.get_or_insert_default();
                        response.header_mutation = Some(ext_proc_v3::HeaderMutation {
                            set_headers: vec![core_v3::HeaderValueOption {
                                header: Some(core_v3::HeaderValue {
                                    key: openai_proxy_common::MODEL_HEADER.to_owned(),
                                    raw_value: model.into(),
                                    ..core_v3::HeaderValue::default()
                                }),
                                ..core_v3::HeaderValueOption::default()
                            }],
                            ..ext_proc_v3::HeaderMutation::default()
                        });
                        response.clear_route_cache = true;
                    }
                    (
                        Some(ext_proc_v3::processing_response::Response::RequestBody(
                            response,
                        )),
                        None,
                    )
                }
                Some(ext_proc_v3::processing_request::Request::ResponseBody(_)) => (
                    Some(ext_proc_v3::processing_response::Response::ResponseBody(
                        ext_proc_v3::BodyResponse::default(),
                    )),
                    None,
                ),
                Some(ext_proc_v3::processing_request::Request::RequestTrailers(_)) => (
                    Some(ext_proc_v3::processing_response::Response::RequestTrailers(
                        ext_proc_v3::TrailersResponse::default(),
                    )),
                    None,
                ),
                Some(ext_proc_v3::processing_request::Request::ResponseTrailers(_)) => (
                    Some(
                        ext_proc_v3::processing_response::Response::ResponseTrailers(
                            ext_proc_v3::TrailersResponse::default(),
                        ),
                    ),
                    None,
                ),
                None => (None, None),
            };
            let response = ext_proc_v3::ProcessingResponse {
                response,
                mode_override,
                ..ext_proc_v3::ProcessingResponse::default()
            };
            tracing::info!(?response);
            response
        });
        Ok(tonic::Response::new(stream.boxed()))
    }
}
