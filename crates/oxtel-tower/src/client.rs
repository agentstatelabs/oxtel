//! The HTTP client layer.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, ready};
use std::time::Instant;

use http::{Request, Response};
use metrics::Label;
use oxtel_sdk::propagation::inject_trace_context;
use oxtel_sdk::trace::{current_span_context, span_context};
use tower_layer::Layer;
use tower_service::Service;
use tracing::Span;

use crate::request::{method_name, protocol_version, server_address};

#[derive(Clone)]
struct Config {
    propagate: bool,
    metrics: bool,
}

/// Instruments an HTTP client, as the OpenTelemetry semantic conventions
/// for HTTP clients describe:
///
/// - **A span per request**, named for the method, with
///   `http.request.method`, `server.address`, `server.port`, `url.full`
///   (without credentials or query), `network.protocol.version`,
///   `http.response.status_code`, and `error.type` with status Error for
///   4xx and 5xx responses and failed requests.
/// - **W3C `traceparent` and `tracestate`** on each request, so the server
///   continues the trace.
/// - **`http.client.request.duration`** (seconds) through the `metrics`
///   facade, by method, server address and port, scheme, status and
///   `error.type`.
#[derive(Clone)]
pub struct HttpClientLayer {
    config: Arc<Config>,
}

impl Default for HttpClientLayer {
    fn default() -> Self {
        HttpClientLayer::new()
    }
}

impl HttpClientLayer {
    /// A layer with spans, propagation and metrics on.
    pub fn new() -> Self {
        crate::describe_metrics();
        HttpClientLayer {
            config: Arc::new(Config {
                propagate: true,
                metrics: true,
            }),
        }
    }

    /// Send W3C trace context with each request. Default: true. Turn it off
    /// for requests leaving your trust boundary.
    pub fn with_propagation(mut self, enabled: bool) -> Self {
        Arc::make_mut(&mut self.config).propagate = enabled;
        self
    }

    /// Record `http.client.request.duration`. Default: true.
    pub fn with_metrics(mut self, enabled: bool) -> Self {
        Arc::make_mut(&mut self.config).metrics = enabled;
        self
    }
}

impl<S> Layer<S> for HttpClientLayer {
    type Service = HttpClient<S>;

    fn layer(&self, inner: S) -> Self::Service {
        HttpClient {
            inner,
            config: Arc::clone(&self.config),
        }
    }
}

/// The service [`HttpClientLayer`] wraps around a client.
#[derive(Clone)]
pub struct HttpClient<S> {
    inner: S,
    config: Arc<Config>,
}

/// `url.full` without user information or query, which may hold secrets.
fn url_full<B>(req: &Request<B>, address: Option<&str>, port: Option<u16>) -> String {
    let uri = req.uri();
    let scheme = uri.scheme_str().unwrap_or("http");
    let mut url = format!("{scheme}://");
    if let Some(host) = address {
        if host.contains(':') {
            url.push_str(&format!("[{host}]"));
        } else {
            url.push_str(host);
        }
    }
    if let Some(port) = port {
        url.push_str(&format!(":{port}"));
    }
    url.push_str(uri.path());
    url
}

impl<S, B, RB> Service<Request<B>> for HttpClient<S>
where
    S: Service<Request<B>, Response = Response<RB>>,
{
    type Response = Response<RB>;
    type Error = S::Error;
    type Future = ResponseFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: Request<B>) -> Self::Future {
        let (method, method_original) = method_name(req.method());
        let (address, port) = server_address(&req);
        let scheme = req.uri().scheme_str().unwrap_or("http").to_string();
        let port = port.or(match scheme.as_str() {
            "https" => Some(443),
            "http" => Some(80),
            _ => None,
        });
        let version = protocol_version(req.version());
        let span = tracing::info_span!(
            target: "oxtel_tower",
            "HTTP client request",
            otel.name = if method == "_OTHER" { "HTTP" } else { method },
            otel.kind = "client",
            http.request.method = method,
            http.request.method_original = method_original,
            server.address = address,
            server.port = port,
            url.full = url_full(&req, address, port),
            network.protocol.version = version,
            http.response.status_code = tracing::field::Empty,
            error.type = tracing::field::Empty,
            otel.status_code = tracing::field::Empty,
        );
        let finish = Finish {
            method,
            address: address.map(str::to_string),
            port,
            scheme,
            config: Arc::clone(&self.config),
        };
        if self.config.propagate {
            // The client span's context, or the caller's when the span is
            // filtered out.
            if let Some(cx) = span_context(&span).or_else(current_span_context) {
                inject_trace_context(&cx, req.headers_mut());
            }
        }
        let inner = span.in_scope(|| self.inner.call(req));
        ResponseFuture {
            inner,
            span,
            started: Instant::now(),
            finish: Some(finish),
        }
    }
}

pin_project_lite::pin_project! {
    /// The response future of [`HttpClient`].
    pub struct ResponseFuture<F> {
        #[pin]
        inner: F,
        span: Span,
        started: Instant,
        finish: Option<Finish>,
    }
}

impl<F, RB, E> Future for ResponseFuture<F>
where
    F: Future<Output = Result<Response<RB>, E>>,
{
    type Output = Result<Response<RB>, E>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let _entered = this.span.enter();
        let result = ready!(this.inner.poll(cx));
        if let Some(finish) = this.finish.take() {
            finish.finish(this.span, &result, this.started.elapsed());
        }
        Poll::Ready(result)
    }
}

struct Finish {
    method: &'static str,
    address: Option<String>,
    port: Option<u16>,
    scheme: String,
    config: Arc<Config>,
}

impl Finish {
    fn finish<RB, E>(
        self,
        span: &Span,
        result: &Result<Response<RB>, E>,
        elapsed: std::time::Duration,
    ) {
        let status = result.as_ref().ok().map(|r| r.status().as_u16());
        let error_type = match (status, result) {
            (Some(s), _) if s >= 400 => Some(s.to_string()),
            (_, Err(_)) => Some(std::any::type_name::<E>().to_string()),
            _ => None,
        };
        if let Some(status) = status {
            span.record("http.response.status_code", status);
        }
        if let Some(error) = &error_type {
            span.record("error.type", error.as_str());
            span.record("otel.status_code", "error");
        }
        if self.config.metrics {
            let mut labels = vec![
                Label::new("http.request.method", self.method),
                Label::new("url.scheme", self.scheme),
            ];
            if let Some(address) = self.address {
                labels.push(Label::new("server.address", address));
            }
            if let Some(port) = self.port {
                labels.push(Label::new("server.port", port.to_string()));
            }
            if let Some(status) = status {
                labels.push(Label::new("http.response.status_code", status.to_string()));
            }
            if let Some(error) = error_type {
                labels.push(Label::new("error.type", error));
            }
            metrics::histogram!("http.client.request.duration", labels)
                .record(elapsed.as_secs_f64());
        }
    }
}
