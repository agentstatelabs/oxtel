//! An MCP server over streamable HTTP, in an axum app, and a client that
//! calls one of its tools: one trace runs from the client's work, through
//! its request, into the tool on the server. Here both are in one program,
//! which calls its own server once and stops, so one command shows each
//! side; a real server runs until it's told to stop.
//!
//! ```bash
//! cargo run -p oxtel-examples --example mcp-http
//! OTEL_TRACES_EXPORTER=console cargo run -p oxtel-examples --example mcp-http   # and the spans
//! ```
//!
//! A program of your own, with a server and a client, needs:
//!
//! ```toml
//! [dependencies]
//! oxtel = "0.4"
//! # 0.4.1 for InstrumentedHandler; `client` for the client's side, which a
//! # server alone doesn't need.
//! oxtel-rmcp = { version = "0.4.1", features = ["client"] }
//! # The HTTP server, then the client.
//! rmcp = { version = "3.5", features = ["transport-streamable-http-server", "client", "transport-streamable-http-client-reqwest"] }
//! axum = "0.8"
//! schemars = "1"
//! serde = { version = "1", features = ["derive"] }
//! serde_json = "1"
//! tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
//! tracing = "0.1"
//! ```

use std::sync::Arc;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::{StreamableHttpClientTransport, StreamableHttpService};
use rmcp::{ServiceExt, tool, tool_router};
use tracing::Instrument;

/// The arguments of the `add` tool.
#[derive(serde::Deserialize, schemars::JsonSchema)]
struct Numbers {
    a: i64,
    b: i64,
}

#[derive(Clone)]
struct Calculator;

#[tool_router(server_handler)]
impl Calculator {
    /// Runs inside the request's span, so its log lines carry the client's
    /// trace id.
    #[tool(description = "Add two whole numbers.")]
    fn add(&self, Parameters(Numbers { a, b }): Parameters<Numbers>) -> String {
        tracing::info!(a, b, "adding");
        (a + b).to_string()
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // rmcp logs each session's start and end at `info`, with everything the
    // client said about itself, in spans of its own. Keep rmcp to its
    // warnings, unless RUST_LOG says otherwise.
    let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| "info,rmcp=warn".into());
    // First, so the MCP metrics find oxtel's recorder. Keep the guard until
    // main returns: dropping it sends what's left.
    let _telemetry = oxtel::builder()
        .with_traces_filter(&filter)
        .with_logs_filter(&filter)
        .install()?;

    // ANCHOR: serve
    // A server for each session, each instrumented. rmcp's HTTP server takes
    // a handler, so it's InstrumentedHandler rather than Instrumented.
    let mcp = StreamableHttpService::new(
        || Ok(oxtel_rmcp::InstrumentedHandler::new(Calculator)),
        Arc::new(LocalSessionManager::default()),
        Default::default(),
    );
    let app = axum::Router::new().nest_service("/mcp", mcp);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3003").await?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    // ANCHOR_END: serve

    // ANCHOR: client
    let transport = StreamableHttpClientTransport::from_uri("http://127.0.0.1:3003/mcp");
    let client = ().serve(transport).await?;
    // The client's own work, in a span of its own: the tool call is part of
    // its trace, on both sides.
    async {
        let mut call = CallToolRequestParams::new("add");
        call.arguments = serde_json::json!({ "a": 2, "b": 3 }).as_object().cloned();
        let result = oxtel_rmcp::client::call_tool(client.peer(), call, Default::default()).await?;
        let answer = result.content.first().and_then(|c| c.as_text());
        tracing::info!(answer = answer.map(|t| t.text.as_str()), "answered");
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .instrument(tracing::info_span!("ask"))
    .await?;
    client.cancel().await?;
    // ANCHOR_END: client

    server.abort();
    Ok(())
}
