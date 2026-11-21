//! The HTTP client layer, and [`ClientCall`] for clients that aren't tower
//! services.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, ready};
use std::time::Instant;

use http::{HeaderMap, Method, Request, Response, StatusCode, Version};
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
///
/// For a client that isn't a tower service, [`ClientCall`] records the
/// same.
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
        let (server_address, server_port) = server_address(&req);
        let mut call = ClientCall::start(&ClientRequestInfo {
            method: req.method(),
            scheme: req.uri().scheme_str().unwrap_or("http"),
            server_address,
            server_port,
            path: req.uri().path(),
            protocol_version: req.version(),
        });
        if !self.config.metrics {
            call = call.without_metrics();
        }
        if self.config.propagate {
            call.inject_trace_context(req.headers_mut());
        }
        let inner = call.span().in_scope(|| self.inner.call(req));
        ResponseFuture {
            inner,
            call: Some(call),
        }
    }
}

pin_project_lite::pin_project! {
    /// The response future of [`HttpClient`].
    pub struct ResponseFuture<F> {
        #[pin]
        inner: F,
        call: Option<ClientCall>,
    }
}

impl<F, RB, E> Future for ResponseFuture<F>
where
    F: Future<Output = Result<Response<RB>, E>>,
{
    type Output = Result<Response<RB>, E>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let _entered = this.call.as_ref().map(|call| call.span().enter());
        let result = ready!(this.inner.poll(cx));
        drop(_entered);
        if let Some(call) = this.call.take() {
            match &result {
                Ok(response) => call.finish(response.status(), response.version()),
                Err(_) => call.fail(std::any::type_name::<E>()),
            }
        }
        Poll::Ready(result)
    }
}

/// An outgoing request, as [`ClientCall::start`] records it.
#[derive(Debug, Clone, Copy)]
pub struct ClientRequestInfo<'a> {
    /// The method. Methods outside the standard ones are recorded as
    /// `_OTHER`, with the method as sent in `http.request.method_original`.
    pub method: &'a Method,
    /// `url.scheme`, such as `https`.
    pub scheme: &'a str,
    /// `server.address`: the host the request is sent to.
    pub server_address: Option<&'a str>,
    /// `server.port`. Without one, the scheme's default port is recorded
    /// for `http` and `https`.
    pub server_port: Option<u16>,
    /// The URL's path. The query isn't taken: it may hold secrets.
    pub path: &'a str,
    /// The HTTP version asked for. The response's version is recorded
    /// instead when there is one.
    pub protocol_version: Version,
}

/// One outgoing HTTP request, recorded as [`HttpClientLayer`] records it:
/// a client span, trace context for the request's headers, and
/// `http.client.request.duration`. For instrumenting an HTTP client that
/// isn't a tower service:
///
/// ```
/// use http::{HeaderMap, Method, StatusCode, Version};
/// use oxtel_tower::{ClientCall, ClientRequestInfo};
///
/// # fn send(_: &HeaderMap) -> Result<StatusCode, ()> { Ok(StatusCode::OK) }
/// let call = ClientCall::start(&ClientRequestInfo {
///     method: &Method::GET,
///     scheme: "https",
///     server_address: Some("api.example"),
///     server_port: None,
///     path: "/items",
///     protocol_version: Version::HTTP_11,
/// });
/// let mut headers = HeaderMap::new();
/// call.inject_trace_context(&mut headers);
/// // Send the request inside the span, so what it logs is part of it.
/// match call.span().in_scope(|| send(&headers)) {
///     Ok(status) => call.finish(status, Version::HTTP_11),
///     Err(_) => call.fail("connect"),
/// }
/// ```
///
/// A call dropped without [`finish`](ClientCall::finish) or
/// [`fail`](ClientCall::fail), as when its future is cancelled, ends its
/// span without a status and records no duration.
#[derive(Debug)]
pub struct ClientCall {
    span: Span,
    started: Instant,
    method: &'static str,
    scheme: String,
    address: Option<String>,
    port: Option<u16>,
    protocol_version: Version,
    metrics: bool,
}

impl ClientCall {
    /// Starts the request's span and its clock. Make the
    /// [`HttpClientLayer`] or call [`describe_metrics`](crate::describe_metrics)
    /// once first, so the duration is recorded in seconds with the
    /// conventions' buckets.
    pub fn start(info: &ClientRequestInfo<'_>) -> ClientCall {
        let (method, method_original) = method_name(info.method);
        let port = info.server_port.or(match info.scheme {
            "https" => Some(443),
            "http" => Some(80),
            _ => None,
        });
        let span = tracing::info_span!(
            target: "oxtel_tower",
            "HTTP client request",
            otel.name = if method == "_OTHER" { "HTTP" } else { method },
            otel.kind = "client",
            http.request.method = method,
            http.request.method_original = method_original,
            server.address = info.server_address,
            server.port = port,
            url.full = url_full(info.scheme, info.server_address, port, info.path),
            network.protocol.version = tracing::field::Empty,
            http.response.status_code = tracing::field::Empty,
            error.type = tracing::field::Empty,
            otel.status_code = tracing::field::Empty,
        );
        ClientCall {
            span,
            started: Instant::now(),
            method,
            scheme: info.scheme.to_string(),
            address: info.server_address.map(str::to_string),
            port,
            protocol_version: info.protocol_version,
            metrics: true,
        }
    }

    /// Don't record `http.client.request.duration` for this request.
    pub fn without_metrics(mut self) -> Self {
        self.metrics = false;
        self
    }

    /// The request's span. Send the request inside it.
    pub fn span(&self) -> &Span {
        &self.span
    }

    /// Adds W3C `traceparent` and `tracestate` to the request's headers, so
    /// the server continues the trace: the client span's context, or the
    /// caller's when the span is filtered out.
    pub fn inject_trace_context(&self, headers: &mut HeaderMap) {
        if let Some(cx) = span_context(&self.span).or_else(current_span_context) {
            inject_trace_context(&cx, headers);
        }
    }

    /// The response came back: records its status and HTTP version, marks
    /// 4xx and 5xx as errors, and records the duration.
    pub fn finish(self, status: StatusCode, version: Version) {
        let status = status.as_u16();
        self.span.record("http.response.status_code", status);
        let error = (status >= 400).then(|| status.to_string());
        self.end(Some(status), version, error);
    }

    /// No response came back. `error_type` names what went wrong, with few
    /// enough values to be a metric label, such as `timeout` or `connect`.
    pub fn fail(self, error_type: &str) {
        let version = self.protocol_version;
        self.end(None, version, Some(error_type.to_string()));
    }

    fn end(self, status: Option<u16>, version: Version, error: Option<String>) {
        self.span
            .record("network.protocol.version", protocol_version(version));
        if let Some(error) = &error {
            self.span.record("error.type", error.as_str());
            self.span.record("otel.status_code", "error");
        }
        if !self.metrics {
            return;
        }
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
        if let Some(error) = error {
            labels.push(Label::new("error.type", error));
        }
        metrics::histogram!("http.client.request.duration", labels)
            .record(self.started.elapsed().as_secs_f64());
    }
}

/// `url.full` without user information or query, which may hold secrets.
fn url_full(scheme: &str, address: Option<&str>, port: Option<u16>, path: &str) -> String {
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
    url.push_str(path);
    url
}
