//! OTLP over gRPC to a receiver in the test: plain, and over mutual TLS.

#![cfg(feature = "grpc")]

use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::{Buf, BufMut};
use common::{Pki, span};
use oxtel_otlp::proto::collector::trace::v1::{
    ExportTracePartialSuccess, ExportTraceServiceRequest, ExportTraceServiceResponse,
};
use oxtel_otlp::{OtlpHttpExporterBuilder, Protocol, Signal};
use oxtel_sdk::Resource;
use oxtel_sdk::export::ExportError;
use oxtel_sdk::trace::SpanExporter;
use prost::Message;
use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};
use tonic::transport::{Certificate, Identity, Server, ServerTlsConfig};
use tonic::{Request, Response, Status};

mod common;

/// What the receiver saw: each call's path, its `api-key` metadata, and the
/// spans in it.
type Seen = Arc<Mutex<Vec<(String, Option<String>, usize)>>>;

/// Answers every `Export` call, rejecting one span of each traces request
/// as a partial success, or failing every call with `fail`.
#[derive(Clone)]
struct Receiver {
    seen: Seen,
    fail: Option<tonic::Code>,
}

impl tower_service::Service<http::Request<tonic::body::Body>> for Receiver {
    type Response = http::Response<tonic::body::Body>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Infallible>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: http::Request<tonic::body::Body>) -> Self::Future {
        let path = req.uri().path().to_string();
        let key = req
            .headers()
            .get("api-key")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let (seen, fail) = (Arc::clone(&self.seen), self.fail);
        Box::pin(async move {
            let handler = move |request: Request<Vec<u8>>| {
                let spans = ExportTraceServiceRequest::decode(request.get_ref().as_slice())
                    .map(|r| {
                        r.resource_spans
                            .iter()
                            .flat_map(|rs| &rs.scope_spans)
                            .map(|ss| ss.spans.len())
                            .sum()
                    })
                    .unwrap_or(0);
                seen.lock()
                    .unwrap()
                    .push((path.clone(), key.clone(), spans));
                async move {
                    if let Some(code) = fail {
                        return Err(Status::new(code, "no"));
                    }
                    let reply = ExportTraceServiceResponse {
                        partial_success: Some(ExportTracePartialSuccess {
                            rejected_spans: 1,
                            error_message: "one too many".into(),
                        }),
                    };
                    Ok(Response::new(reply.encode_to_vec()))
                }
            };
            let mut grpc = tonic::server::Grpc::new(RawCodec)
                .accept_compressed(tonic::codec::CompressionEncoding::Gzip);
            Ok(grpc.unary(Unary(handler), req).await)
        })
    }
}

/// A closure as a unary handler.
struct Unary<F>(F);

impl<F, Fut> tonic::server::UnaryService<Vec<u8>> for Unary<F>
where
    F: FnMut(Request<Vec<u8>>) -> Fut,
    Fut: Future<Output = Result<Response<Vec<u8>>, Status>>,
{
    type Response = Vec<u8>;
    type Future = Fut;

    fn call(&mut self, request: Request<Vec<u8>>) -> Fut {
        (self.0)(request)
    }
}

/// Messages as bytes, as the exporter sends them.
#[derive(Clone, Copy, Default)]
struct RawCodec;

impl Codec for RawCodec {
    type Encode = Vec<u8>;
    type Decode = Vec<u8>;
    type Encoder = RawCodec;
    type Decoder = RawCodec;
    fn encoder(&mut self) -> RawCodec {
        RawCodec
    }
    fn decoder(&mut self) -> RawCodec {
        RawCodec
    }
}

impl Encoder for RawCodec {
    type Item = Vec<u8>;
    type Error = Status;
    fn encode(&mut self, item: Vec<u8>, dst: &mut EncodeBuf<'_>) -> Result<(), Status> {
        dst.put_slice(&item);
        Ok(())
    }
}

impl Decoder for RawCodec {
    type Item = Vec<u8>;
    type Error = Status;
    fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<Vec<u8>>, Status> {
        let mut message = vec![0; src.remaining()];
        src.copy_to_slice(&mut message);
        Ok(Some(message))
    }
}

/// Starts a receiver on a free port, with TLS if given, on a runtime of
/// its own that lives as long as the test.
fn start(tls: Option<ServerTlsConfig>, fail: Option<tonic::Code>) -> (u16, Seen) {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let seen = Seen::default();
    let receiver = Receiver {
        seen: Arc::clone(&seen),
        fail,
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.spawn(async move {
        let mut server = Server::builder();
        if let Some(tls) = tls {
            server = server.tls_config(tls).unwrap();
        }
        let addr: SocketAddr = ([127, 0, 0, 1], port).into();
        server.serve(addr, receiver).await.unwrap();
    });
    // Leak it: the server runs until the test process ends.
    std::mem::forget(runtime);
    std::thread::sleep(Duration::from_millis(200));
    (port, seen)
}

fn export(exporter: &oxtel_otlp::OtlpHttpExporter) -> Result<(), ExportError> {
    SpanExporter::export(
        exporter,
        &span(),
        &Resource::builder().without_env().build(),
    )
}

#[test]
fn spans_go_to_the_trace_service_with_their_metadata() {
    let (port, seen) = start(None, None);
    let exporter = OtlpHttpExporterBuilder::new()
        .with_protocol(Protocol::Grpc)
        .with_endpoint(&format!("http://127.0.0.1:{port}"))
        .with_header("api-key", "secret")
        .build();
    assert_eq!(
        exporter.destination(Signal::Traces),
        format!("http://127.0.0.1:{port} (OTLP/gRPC)")
    );
    export(&exporter).expect("exported");
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        [(
            "/opentelemetry.proto.collector.trace.v1.TraceService/Export".to_string(),
            Some("secret".to_string()),
            1
        )]
    );
    assert_eq!(
        exporter.rejected(Signal::Traces),
        1,
        "the partial success is read"
    );
}

#[test]
fn gzip_is_negotiated() {
    let (port, seen) = start(None, None);
    let exporter = OtlpHttpExporterBuilder::new()
        .with_protocol(Protocol::Grpc)
        .with_endpoint(&format!("http://127.0.0.1:{port}"))
        .with_gzip(true)
        .build();
    export(&exporter).expect("exported");
    assert_eq!(seen.lock().unwrap()[0].2, 1, "the receiver decompressed it");
}

#[test]
fn refusals_are_explained_and_only_some_are_retried() {
    let (port, seen) = start(None, Some(tonic::Code::Unauthenticated));
    let exporter = OtlpHttpExporterBuilder::new()
        .with_protocol(Protocol::Grpc)
        .with_endpoint(&format!("http://127.0.0.1:{port}"))
        .with_initial_backoff(Duration::from_millis(10))
        .build();
    let Err(ExportError::Rejected { status, message }) = export(&exporter) else {
        panic!("refused")
    };
    assert_eq!(status, 401, "as HTTP says it, so the hints apply");
    assert!(message.contains("Unauthenticated"), "{message}");
    assert_eq!(seen.lock().unwrap().len(), 1, "not retried");

    let (port, seen) = start(None, Some(tonic::Code::Unavailable));
    let exporter = OtlpHttpExporterBuilder::new()
        .with_protocol(Protocol::Grpc)
        .with_endpoint(&format!("http://127.0.0.1:{port}"))
        .with_timeout(Duration::from_millis(500))
        .with_initial_backoff(Duration::from_millis(10))
        .build();
    assert!(export(&exporter).is_err());
    assert!(seen.lock().unwrap().len() > 1, "retried");
}

#[test]
fn mutual_tls_with_the_same_files_as_http() {
    let pki = Pki::new("grpc");
    let read = |name| std::fs::read(pki.path(name)).unwrap();
    let tls = ServerTlsConfig::new()
        .identity(Identity::from_pem(read("server.pem"), read("server.key")))
        .client_ca_root(Certificate::from_pem(read("ca.pem")));
    let (port, seen) = start(Some(tls), None);
    let exporter = OtlpHttpExporterBuilder::new()
        .with_protocol(Protocol::Grpc)
        .with_endpoint(&format!("https://localhost:{port}"))
        .with_certificate(pki.path("ca.pem"))
        .with_client_certificate(pki.path("client.pem"), pki.path("client.key"))
        .with_timeout(Duration::from_secs(3))
        .build();
    assert_eq!(exporter.setup_error(Signal::Traces), None);
    export(&exporter).expect("exported over mutual TLS");
    assert_eq!(seen.lock().unwrap().len(), 1);

    let without_client = OtlpHttpExporterBuilder::new()
        .with_protocol(Protocol::Grpc)
        .with_endpoint(&format!("https://localhost:{port}"))
        .with_certificate(pki.path("ca.pem"))
        .with_timeout(Duration::from_millis(500))
        .with_initial_backoff(Duration::from_millis(10))
        .build();
    assert!(export(&without_client).is_err(), "mutual TLS refuses it");
}

#[test]
fn the_endpoint_is_used_as_is() {
    let exporter = OtlpHttpExporterBuilder::new()
        .with_protocol(Protocol::Grpc)
        .build();
    assert_eq!(exporter.url(Signal::Logs), "http://localhost:4317");
    let exporter = OtlpHttpExporterBuilder::new()
        .with_protocol(Protocol::Grpc)
        .with_endpoint("https://collector.example:4317/")
        .build();
    assert_eq!(
        exporter.url(Signal::Metrics),
        "https://collector.example:4317/",
        "no /v1/metrics"
    );
}

#[test]
fn it_works_from_inside_a_runtime() {
    let (port, seen) = start(None, None);
    let exporter = OtlpHttpExporterBuilder::new()
        .with_protocol(Protocol::Grpc)
        .with_endpoint(&format!("http://127.0.0.1:{port}"))
        .build();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    runtime.block_on(async {
        // As a simple processor does, on whatever thread ends the span.
        export(&exporter).expect("exported");
        drop(exporter);
    });
    assert_eq!(seen.lock().unwrap().len(), 1);
}
