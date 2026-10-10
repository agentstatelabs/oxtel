//! MCP instrumentation for [rmcp](https://docs.rs/rmcp), following the
//! OpenTelemetry semantic conventions for MCP (development status). Part of
//! [oxtel](https://github.com/agentstatelabs/oxtel).
//!
//! - [`Instrumented`] wraps an rmcp server: a span per request and
//!   notification, continuing the caller's trace from `params._meta`, with
//!   `mcp.server.operation.duration` and `mcp.server.session.duration`.
//! - With the `client` feature, `client::send` and `client::call_tool`
//!   send requests in client spans, carrying the trace in `params._meta`,
//!   and record `mcp.client.operation.duration`.
//!
//! Spans are recorded with `tracing` and metrics with `metrics`, so they go
//! wherever the program sends them: with `oxtel::init()`, to OTLP or
//! Prometheus. Tool arguments and results are recorded only on request.

#[cfg(feature = "client")]
pub mod client;
mod common;
mod server;

pub use common::describe_metrics;
pub use server::Instrumented;
