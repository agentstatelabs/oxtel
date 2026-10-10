//! HTTP instrumentation for [tower](https://docs.rs/tower) services, and so
//! for axum and hyper, following the OpenTelemetry semantic conventions.
//! Part of [oxtel](https://github.com/agentstatelabs/oxtel).
//!
//! - [`HttpServerLayer`]: a span per request, continuing the caller's trace;
//!   `http.server.request.duration` and `http.server.active_requests`; an
//!   optional access log.
//! - [`HttpClientLayer`]: a span per request, trace context sent along, and
//!   `http.client.request.duration`.
//! - [`MetricsService`]: Prometheus `/metrics` as a tower service, for an
//!   axum router or a hyper server.
//!
//! ```no_run
//! # #[cfg(feature = "axum")] {
//! use axum::Router;
//! use axum::routing::get;
//! use oxtel_tower::{HttpServerLayer, MetricsService};
//!
//! # fn app(exporter: oxtel_prometheus::PrometheusExporter) -> Router {
//! Router::new()
//!     .route("/items/{id}", get(|| async { "item" }))
//!     // `Router::layer` runs after routing, so `http.route` is the template.
//!     .layer(HttpServerLayer::new())
//!     .route_service("/metrics", MetricsService::new(exporter))
//! # }
//! # }
//! ```
//!
//! Spans and events are recorded with `tracing` and metrics with `metrics`,
//! so they go wherever the program's subscriber and recorder send them:
//! with `oxtel::init()`, to OTLP or Prometheus.

mod client;
mod metrics_service;
mod request;
mod server;

pub use client::{HttpClient, HttpClientLayer};
pub use metrics_service::MetricsService;
pub use request::{RequestInfo, SpanName};
pub use server::{HttpServer, HttpServerLayer};

/// The futures the services return.
pub mod future {
    pub use crate::client::ResponseFuture as ClientResponseFuture;
    pub use crate::server::ResponseFuture as ServerResponseFuture;
}

#[doc(hidden)]
pub mod __private {
    pub use tracing;
}

/// Give the current `metrics` recorder the units and descriptions of the
/// HTTP metrics, so the duration histograms are in seconds with the
/// conventions' buckets. The layers do this when they're made, so make them
/// after installing the recorder (as `oxtel::init()` does), or call this
/// once the recorder is in place.
pub fn describe_metrics() {
    metrics::describe_histogram!(
        "http.server.request.duration",
        metrics::Unit::Seconds,
        "Duration of HTTP server requests."
    );
    metrics::describe_histogram!(
        "http.client.request.duration",
        metrics::Unit::Seconds,
        "Duration of HTTP client requests."
    );
    metrics::describe_gauge!(
        "http.server.active_requests",
        "Number of active HTTP server requests."
    );
}

/// The span [`HttpServerLayer`] makes for a request, from its
/// [`RequestInfo`], with every standard field declared. Add fields of your
/// own after the info, for middleware to record later:
///
/// ```
/// # let info = oxtel_tower::RequestInfo {
/// #     method: "GET", method_original: None, path: "/", scheme: "http", route: None,
/// #     server_address: None, server_port: None, protocol_version: "1.1", user_agent: None,
/// # };
/// let span = oxtel_tower::server_span!(&info, app.user.id = tracing::field::Empty);
/// ```
#[macro_export]
macro_rules! server_span {
    ($info:expr $(,)?) => {
        $crate::server_span!($info, otel.status_description = $crate::__private::tracing::field::Empty)
    };
    ($info:expr, $($field:tt)+) => {{
        let info: &$crate::RequestInfo<'_> = $info;
        $crate::__private::tracing::info_span!(
            target: "oxtel_tower",
            "HTTP server request",
            otel.name = %info.span_name(),
            otel.kind = "server",
            http.request.method = info.method,
            http.request.method_original = info.method_original,
            url.path = info.path,
            url.scheme = info.scheme,
            http.route = info.route,
            server.address = info.server_address,
            server.port = info.server_port,
            network.protocol.version = info.protocol_version,
            user_agent.original = info.user_agent,
            http.response.status_code = $crate::__private::tracing::field::Empty,
            error.type = $crate::__private::tracing::field::Empty,
            otel.status_code = $crate::__private::tracing::field::Empty,
            $($field)+
        )
    }};
}
