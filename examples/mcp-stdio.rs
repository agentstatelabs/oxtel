//! An MCP server over standard input and output, as an MCP client starts a
//! local tool server: every request gets a span, continuing the client's
//! trace, and the MCP metrics are recorded. Standard output carries the
//! protocol, so log lines go to standard error, where oxtel writes them
//! unless told otherwise.
//!
//! ```bash
//! cargo run -p oxtel-examples --example mcp-stdio < examples/input/mcp-stdio.jsonl
//! ```
//!
//! A server of your own needs:
//!
//! ```toml
//! [dependencies]
//! oxtel = "0.4"
//! oxtel-rmcp = "0.4"
//! rmcp = { version = "3.5", features = ["transport-io"] }
//! schemars = "1"
//! serde = { version = "1", features = ["derive"] }
//! tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
//! tracing = "0.1"
//! ```

use rmcp::handler::server::wrapper::Parameters;
use rmcp::{ServiceExt, tool, tool_router};

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
    // client said about itself, in a span of its own. Keep rmcp to its
    // warnings, unless RUST_LOG says otherwise.
    let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| "info,rmcp=warn".into());
    // First, so the MCP metrics find oxtel's recorder. Keep the guard until
    // main returns: dropping it sends what's left.
    let _telemetry = oxtel::builder()
        .with_traces_filter(&filter)
        .with_logs_filter(&filter)
        .install()?;

    // ANCHOR: serve
    let server = oxtel_rmcp::Instrumented::new(Calculator)
        // What `network.transport` records: stdio is a pipe.
        .with_transport("pipe")
        .serve(rmcp::transport::stdio())
        .await?;
    // Until the client closes standard input.
    server.waiting().await?;
    // ANCHOR_END: serve
    Ok(())
}
