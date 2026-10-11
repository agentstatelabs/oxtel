//! The HTTP server layer.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, ready};
use std::time::Instant;

use http::{Request, Response};
use metrics::Label;
use oxtel_sdk::propagation::extract_trace_context;
use oxtel_sdk::trace::attach_remote_parent;
use tower_layer::Layer;
use tower_service::Service;
use tracing::Span;

use crate::request::{RequestInfo, method_name, protocol_version, server_address};

type RouteFn = Arc<dyn for<'a> Fn(&'a http::Extensions) -> Option<&'a str> + Send + Sync>;
type SpanFn = Arc<dyn Fn(&RequestInfo<'_>) -> Span + Send + Sync>;

#[derive(Clone)]
struct Config {
    propagate: bool,
    metrics: bool,
    access_log: bool,
    scheme: &'static str,
    route: Option<RouteFn>,
    make_span: SpanFn,
}

/// Instruments an HTTP server, as the OpenTelemetry semantic conventions
/// for HTTP servers describe:
///
/// - **A span per request**, a child of the caller's when the request
///   carries W3C `traceparent`. Named `{method} {route}`, with
///   `http.request.method`, `url.path`, `url.scheme`, `http.route`,
///   `server.address`, `server.port`, `network.protocol.version`,
///   `user_agent.original`, `http.response.status_code`, and `error.type`
///   with status Error for 5xx responses.
/// - **Metrics**, through the `metrics` facade: `http.server.request.duration`
///   (seconds) by method, scheme, route, status, protocol version and
///   `error.type`; and `http.server.active_requests` by method and scheme.
/// - **An access log event**, if asked for: `request` at `info`, or `error`
///   for 5xx, with the status and duration.
///
/// The duration runs until the response's head is ready; a streamed body
/// may take longer. The query string isn't recorded.
#[derive(Clone)]
pub struct HttpServerLayer {
    config: Arc<Config>,
}

impl Default for HttpServerLayer {
    fn default() -> Self {
        HttpServerLayer::new()
    }
}

impl HttpServerLayer {
    /// A layer with spans, propagation, metrics and the access log on. With
    /// the `axum` feature, `http.route` comes from axum's
    /// `MatchedPath`. Make it after installing the `metrics` recorder; see
    /// [`describe_metrics`](crate::describe_metrics).
    pub fn new() -> Self {
        #[cfg(feature = "axum")]
        let route: Option<RouteFn> = Some(Arc::new(|ext: &http::Extensions| {
            ext.get::<axum::extract::MatchedPath>().map(|m| m.as_str())
        }));
        #[cfg(not(feature = "axum"))]
        let route: Option<RouteFn> = None;
        crate::describe_metrics();
        HttpServerLayer {
            config: Arc::new(Config {
                propagate: true,
                metrics: true,
                access_log: true,
                scheme: "http",
                route,
                make_span: Arc::new(|info| crate::server_span!(info)),
            }),
        }
    }

    fn config(&mut self) -> &mut Config {
        Arc::make_mut(&mut self.config)
    }

    /// Continue the caller's trace from W3C `traceparent` and `tracestate`
    /// headers. Default: true. Turn it off at a trust boundary, where
    /// callers shouldn't choose your trace ids or sampling.
    pub fn with_propagation(mut self, enabled: bool) -> Self {
        self.config().propagate = enabled;
        self
    }

    /// Record the request metrics. Default: true.
    pub fn with_metrics(mut self, enabled: bool) -> Self {
        self.config().metrics = enabled;
        self
    }

    /// Log each finished request as an event: `request` at `info`, or at
    /// `error` for a 5xx. Default: true. `RUST_LOG=info,oxtel_tower=warn`
    /// also hides it, but keeps the errors.
    pub fn with_access_log(mut self, enabled: bool) -> Self {
        self.config().access_log = enabled;
        self
    }

    /// The `url.scheme` for requests whose URI has none, as on a server
    /// behind a proxy that ends TLS. Default: `http`.
    pub fn with_scheme(mut self, scheme: &'static str) -> Self {
        self.config().scheme = scheme;
        self
    }

    /// Where the matched route template comes from, for routers other than
    /// axum's.
    pub fn with_route(
        mut self,
        route: impl for<'a> Fn(&'a http::Extensions) -> Option<&'a str> + Send + Sync + 'static,
    ) -> Self {
        self.config().route = Some(Arc::new(route));
        self
    }

    /// Make each request's span yourself, to declare fields of your own
    /// for later middleware to record. Use [`server_span!`](crate::server_span)
    /// so the standard fields are declared too:
    ///
    /// ```
    /// use oxtel_tower::{HttpServerLayer, server_span};
    ///
    /// let layer = HttpServerLayer::new()
    ///     .with_span(|info| server_span!(info, app.user.id = tracing::field::Empty));
    /// ```
    pub fn with_span(
        mut self,
        make_span: impl Fn(&RequestInfo<'_>) -> Span + Send + Sync + 'static,
    ) -> Self {
        self.config().make_span = Arc::new(make_span);
        self
    }
}

impl<S> Layer<S> for HttpServerLayer {
    type Service = HttpServer<S>;

    fn layer(&self, inner: S) -> Self::Service {
        HttpServer {
            inner,
            config: Arc::clone(&self.config),
        }
    }
}

/// The service [`HttpServerLayer`] wraps around yours.
#[derive(Clone)]
pub struct HttpServer<S> {
    inner: S,
    config: Arc<Config>,
}

impl<S, B, RB> Service<Request<B>> for HttpServer<S>
where
    S: Service<Request<B>, Response = Response<RB>>,
{
    type Response = Response<RB>;
    type Error = S::Error;
    type Future = ResponseFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<B>) -> Self::Future {
        let config = &self.config;
        let (method, method_original) = method_name(req.method());
        let (server_address, server_port) = server_address(&req);
        let route = config.route.as_ref().and_then(|f| f(req.extensions()));
        let info = RequestInfo {
            method,
            method_original,
            path: req.uri().path(),
            scheme: req.uri().scheme_str().unwrap_or(config.scheme),
            route,
            server_address,
            server_port,
            protocol_version: protocol_version(req.version()),
            user_agent: req
                .headers()
                .get(http::header::USER_AGENT)
                .and_then(|v| v.to_str().ok()),
        };
        let span = {
            let _parent = config
                .propagate
                .then(|| extract_trace_context(req.headers()).map(attach_remote_parent))
                .flatten();
            (config.make_span)(&info)
        };
        let finish = Finish {
            method,
            scheme: info.scheme.to_string(),
            route: info.route.map(str::to_string),
            protocol_version: info.protocol_version,
            config: Arc::clone(config),
        };
        let active = config
            .metrics
            .then(|| Active::start(method, finish.scheme.clone()));
        let inner = span.in_scope(|| self.inner.call(req));
        ResponseFuture {
            inner,
            span,
            started: Instant::now(),
            finish: Some(finish),
            _active: active,
        }
    }
}

pin_project_lite::pin_project! {
    /// The response future of [`HttpServer`].
    pub struct ResponseFuture<F> {
        #[pin]
        inner: F,
        span: Span,
        started: Instant,
        finish: Option<Finish>,
        _active: Option<Active>,
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

/// What's recorded once the response is ready.
struct Finish {
    method: &'static str,
    scheme: String,
    route: Option<String>,
    protocol_version: &'static str,
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
            (Some(s), _) if s >= 500 => Some(s.to_string()),
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
        if self.config.access_log {
            let duration = elapsed.as_secs_f64();
            if error_type.is_some() {
                tracing::error!(
                    http.response.status_code = status,
                    http.server.request.duration = duration,
                    error.type = error_type.as_deref(),
                    "request"
                );
            } else {
                tracing::info!(
                    http.response.status_code = status,
                    http.server.request.duration = duration,
                    "request"
                );
            }
        }
        if self.config.metrics {
            let mut labels = vec![
                Label::new("http.request.method", self.method),
                Label::new("url.scheme", self.scheme),
                Label::new("network.protocol.version", self.protocol_version),
            ];
            if let Some(route) = self.route {
                labels.push(Label::new("http.route", route));
            }
            if let Some(status) = status {
                labels.push(Label::new("http.response.status_code", status.to_string()));
            }
            if let Some(error) = error_type {
                labels.push(Label::new("error.type", error));
            }
            metrics::histogram!("http.server.request.duration", labels)
                .record(elapsed.as_secs_f64());
        }
    }
}

/// Counts a request in `http.server.active_requests` while it lives, so
/// cancelled requests are uncounted too.
struct Active {
    gauge: metrics::Gauge,
}

impl Active {
    fn start(method: &'static str, scheme: String) -> Self {
        let gauge = metrics::gauge!(
            "http.server.active_requests",
            "http.request.method" => method,
            "url.scheme" => scheme
        );
        gauge.increment(1.0);
        Active { gauge }
    }
}

impl Drop for Active {
    fn drop(&mut self) {
        self.gauge.decrement(1.0);
    }
}
