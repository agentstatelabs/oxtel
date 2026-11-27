//! HTTP client spans and metrics for [reqwest](https://docs.rs/reqwest),
//! as the OpenTelemetry semantic conventions describe, with the trace
//! carried on to the server. Part of
//! [oxtel](https://github.com/agentstatelabs/oxtel).
//!
//! One line wraps a reqwest client, and requests are made as before:
//!
//! ```no_run
//! # async fn run() -> Result<(), reqwest_middleware::Error> {
//! let client = oxtel_reqwest::client(reqwest::Client::new());
//! let items = client.get("https://api.example/items").send().await?.text().await?;
//! # Ok(()) }
//! ```
//!
//! Every request gets:
//!
//! - **a client span** named for its method, with `http.request.method`,
//!   `server.address`, `server.port`, `url.full`,
//!   `network.protocol.version` and `http.response.status_code`. A 4xx or
//!   5xx response, or a request that got no response, is an error, with
//!   `error.type` saying why: the status code, or `timeout`, `connect`,
//!   `redirect`, `body`, `decode` or `request`.
//! - **W3C `traceparent` and `tracestate` headers**, so a server that
//!   reads them, such as one with oxtel-tower's `HttpServerLayer`,
//!   continues the trace.
//! - **`http.client.request.duration`** in seconds, by method, server,
//!   scheme, status and `error.type`.
//!
//! The query string and any credentials in the URL are never recorded,
//! since either may hold secrets. These are the spans and metrics
//! oxtel-tower's `HttpClientLayer` records, so a program using both gets
//! the same telemetry from each.
//!
//! [`client`] returns reqwest-middleware's
//! [`ClientWithMiddleware`],
//! whose methods are reqwest's own. A program with other middleware adds
//! [`HttpClientMiddleware`] with the rest. After retry middleware, each
//! attempt gets a span of its own; a redirect that reqwest follows itself is
//! part of the same span.
//!
//! ```
//! let client = reqwest_middleware::ClientBuilder::new(reqwest::Client::new())
//!     // No trace context for requests that leave your systems.
//!     .with(oxtel_reqwest::HttpClientMiddleware::new().with_propagation(false))
//!     .build();
//! ```
//!
//! Spans are recorded with `tracing` and metrics with `metrics`, so they go
//! wherever the program's subscriber and recorder send them: with
//! `oxtel::init()`, to OTLP or Prometheus. Make the client after
//! `oxtel::init()`, so the duration is recorded in seconds with the
//! conventions' buckets.

use http::Extensions;
use oxtel_tower::{ClientCall, ClientRequestInfo};
use reqwest::{Request, Response};
use reqwest_middleware::{ClientWithMiddleware, Middleware, Next};
use tracing::Instrument;

pub use reqwest_middleware;

/// `client`, with every request it makes recorded: [`HttpClientMiddleware`]
/// with its defaults.
pub fn client(client: reqwest::Client) -> ClientWithMiddleware {
    reqwest_middleware::ClientBuilder::new(client)
        .with(HttpClientMiddleware::new())
        .build()
}

/// reqwest-middleware middleware that records each request: a client span,
/// trace context sent along, and `http.client.request.duration`. See the
/// [crate documentation](crate).
#[derive(Clone, Debug)]
pub struct HttpClientMiddleware {
    propagate: bool,
    metrics: bool,
}

impl Default for HttpClientMiddleware {
    fn default() -> Self {
        HttpClientMiddleware::new()
    }
}

impl HttpClientMiddleware {
    /// Spans, propagation and metrics on.
    pub fn new() -> Self {
        oxtel_tower::describe_metrics();
        HttpClientMiddleware {
            propagate: true,
            metrics: true,
        }
    }

    /// Send W3C trace context with each request. Default: true. Turn it off
    /// for requests leaving your trust boundary.
    pub fn with_propagation(mut self, enabled: bool) -> Self {
        self.propagate = enabled;
        self
    }

    /// Record `http.client.request.duration`. Default: true.
    pub fn with_metrics(mut self, enabled: bool) -> Self {
        self.metrics = enabled;
        self
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl Middleware for HttpClientMiddleware {
    async fn handle(
        &self,
        mut req: Request,
        extensions: &mut Extensions,
        next: Next<'_>,
    ) -> reqwest_middleware::Result<Response> {
        let url = req.url();
        let mut call = ClientCall::start(&ClientRequestInfo {
            method: req.method(),
            scheme: url.scheme(),
            // An IPv6 address without its brackets, as the conventions
            // record it.
            server_address: url
                .host_str()
                .map(|host| host.trim_start_matches('[').trim_end_matches(']')),
            server_port: url.port_or_known_default(),
            path: url.path(),
            protocol_version: req.version(),
        });
        if !self.metrics {
            call = call.without_metrics();
        }
        if self.propagate {
            call.inject_trace_context(req.headers_mut());
        }
        let span = call.span().clone();
        let result = next.run(req, extensions).instrument(span).await;
        match &result {
            Ok(response) => call.finish(response.status(), response.version()),
            Err(error) => call.fail(error_type(error)),
        }
        result
    }
}

/// `error.type` for a request that got no response: a short name for what
/// went wrong, few enough to be a metric label.
fn error_type(error: &reqwest_middleware::Error) -> &'static str {
    let error = match error {
        reqwest_middleware::Error::Reqwest(error) => error,
        reqwest_middleware::Error::Middleware(_) => return "middleware",
    };
    if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connect"
    } else if error.is_redirect() {
        "redirect"
    } else if error.is_body() {
        "body"
    } else if error.is_decode() {
        "decode"
    } else if error.is_builder() {
        "builder"
    } else if error.is_request() {
        "request"
    } else {
        "reqwest::Error"
    }
}
