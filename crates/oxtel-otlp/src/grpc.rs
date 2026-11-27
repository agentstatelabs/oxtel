//! OTLP over gRPC: unary `Export` calls through tonic's client, on a small
//! tokio runtime the exporter owns, so it works from any thread, in any
//! program, async or not.

use std::sync::mpsc;
use std::time::Duration;

use bytes::{Buf, BufMut};
use http::uri::PathAndQuery;
use tonic::codec::{Codec, CompressionEncoding, DecodeBuf, Decoder, EncodeBuf, Encoder};
use tonic::metadata::{AsciiMetadataKey, AsciiMetadataValue};
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint, Identity};
use tonic::{Code, Status};

use crate::Signal;
use crate::tls::{TlsFiles, read};

/// The `Export` method of `signal`'s collector service.
fn path(signal: Signal) -> &'static str {
    match signal {
        Signal::Traces => "/opentelemetry.proto.collector.trace.v1.TraceService/Export",
        Signal::Metrics => "/opentelemetry.proto.collector.metrics.v1.MetricsService/Export",
        Signal::Logs => "/opentelemetry.proto.collector.logs.v1.LogsService/Export",
    }
}

/// A connection to one gRPC receiver.
pub(crate) struct GrpcClient {
    /// Taken on drop, to shut down without blocking.
    runtime: Option<tokio::runtime::Runtime>,
    channel: Channel,
}

impl GrpcClient {
    /// A client for `url`, such as `http://localhost:4317`. It connects on
    /// first use, and reconnects as needed.
    pub(crate) fn new(url: &str, tls: &TlsFiles) -> Result<GrpcClient, String> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("oxtel-otlp-grpc")
            .enable_all()
            .build()
            .map_err(|e| format!("can't start the gRPC runtime: {e}"))?;
        let mut endpoint = Endpoint::from_shared(url.to_string())
            .map_err(|e| format!("{url} isn't a gRPC endpoint: {e}"))?
            .user_agent(concat!("oxtel/", env!("CARGO_PKG_VERSION")))
            .map_err(|e| e.to_string())?;
        if url.starts_with("https://") {
            let mut config = match &tls.certificate {
                Some(ca) => ClientTlsConfig::new()
                    .ca_certificate(Certificate::from_pem(read(ca, "CA certificate file")?)),
                None => ClientTlsConfig::new().with_webpki_roots(),
            };
            if let Some((cert, key)) = tls.client_identity()? {
                config = config.identity(Identity::from_pem(
                    read(cert, "client certificate file")?,
                    read(key, "client key file")?,
                ));
            }
            endpoint = endpoint
                .tls_config(config)
                .map_err(|e| format!("TLS for {url}: {e}"))?;
        }
        let channel = {
            let _entered = runtime.enter();
            endpoint.connect_lazy()
        };
        Ok(GrpcClient {
            runtime: Some(runtime),
            channel,
        })
    }

    /// One `Export` call of `signal` with the encoded `request`, returning
    /// the encoded response.
    pub(crate) fn export(
        &self,
        signal: Signal,
        request: Vec<u8>,
        headers: &[(String, String)],
        gzip: bool,
        timeout: Duration,
    ) -> Result<Vec<u8>, Status> {
        let mut grpc = tonic::client::Grpc::new(self.channel.clone());
        if gzip {
            grpc = grpc
                .send_compressed(CompressionEncoding::Gzip)
                .accept_compressed(CompressionEncoding::Gzip);
        }
        let mut request = tonic::Request::new(request);
        request.set_timeout(timeout);
        for (name, value) in headers {
            let (Ok(name), Ok(value)) = (
                AsciiMetadataKey::from_bytes(name.to_ascii_lowercase().as_bytes()),
                AsciiMetadataValue::try_from(value.as_str()),
            ) else {
                continue;
            };
            request.metadata_mut().insert(name, value);
        }
        let call = async move {
            grpc.ready()
                .await
                .map_err(|e| Status::unavailable(e.to_string()))?;
            grpc.unary(request, PathAndQuery::from_static(path(signal)), RawCodec)
                .await
                .map(tonic::Response::into_inner)
        };
        // Run on the exporter's runtime and wait here, rather than block_on,
        // which panics on a thread that's already in a runtime.
        let (done, result) = mpsc::sync_channel(1);
        let runtime = self.runtime.as_ref().expect("the runtime lives until drop");
        runtime.spawn(async move {
            let _ = done.send(tokio::time::timeout(timeout, call).await);
        });
        match result.recv() {
            Ok(Ok(reply)) => reply,
            Ok(Err(_)) => Err(Status::deadline_exceeded("no answer in time")),
            Err(_) => Err(Status::cancelled("the gRPC runtime stopped")),
        }
    }
}

impl Drop for GrpcClient {
    fn drop(&mut self) {
        // Without blocking: the last clone may be dropped inside a runtime.
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

/// Whether the OTLP specification lets a call that failed with `code` be
/// retried.
pub(crate) fn retryable(code: Code) -> bool {
    matches!(
        code,
        Code::Cancelled
            | Code::DeadlineExceeded
            | Code::Aborted
            | Code::OutOfRange
            | Code::Unavailable
            | Code::DataLoss
    )
}

/// The HTTP status a gRPC code corresponds to, so failures read the same
/// whichever protocol carried them.
pub(crate) fn http_status(code: Code) -> u16 {
    match code {
        Code::Ok => 200,
        Code::InvalidArgument | Code::FailedPrecondition | Code::OutOfRange => 400,
        Code::Unauthenticated => 401,
        Code::PermissionDenied => 403,
        // No such service at that address.
        Code::NotFound | Code::Unimplemented => 404,
        Code::AlreadyExists | Code::Aborted => 409,
        Code::ResourceExhausted => 429,
        Code::Cancelled => 499,
        Code::Unavailable => 503,
        Code::DeadlineExceeded => 504,
        Code::Unknown | Code::Internal | Code::DataLoss => 500,
    }
}

/// Passes already-encoded messages through as bytes, so requests are
/// encoded once, as for OTLP/HTTP, and responses read the same way.
#[derive(Clone, Copy, Debug, Default)]
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
