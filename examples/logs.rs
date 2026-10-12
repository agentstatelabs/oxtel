//! Structured logs and nothing else: no collector, no configuration.
//!
//! ```bash
//! cargo run -p oxtel-examples --example logs                 # text, on a terminal
//! cargo run -p oxtel-examples --example logs 2>&1 | cat      # JSON, since that isn't one
//! OXTEL_LOG_FORMAT=ecs cargo run -p oxtel-examples --example logs
//! ```
//!
//! A program of your own needs:
//!
//! ```toml
//! [dependencies]
//! oxtel = "0.3"
//! tracing = "0.1"
//! ```
//!
//! With these, the same program sends traces, metrics and logs to a
//! collector once `OTEL_EXPORTER_OTLP_ENDPOINT` is set, with no code
//! changes. A program that only ever wants logs can leave oxtel's export
//! code out altogether, and with it every HTTP and TLS crate:
//!
//! ```toml
//! [dependencies]
//! oxtel = { version = "0.3", default-features = false }
//! tracing = "0.1"
//! ```

fn main() -> Result<(), oxtel::Error> {
    // Text on a terminal, JSON anywhere else. Keep the guard alive.
    let _logs = oxtel::init()?;

    // Fields are structured: in JSON, each is a key of its own.
    tracing::info!(user.id = 42, plan = "team", "signed in");

    // Lines inside a span carry its fields and its trace id, so every line
    // about one request can be found together.
    let request = tracing::info_span!("request", http.route = "/orders", order.id = 7);
    let _entered = request.enter();
    tracing::warn!(attempt = 2, "payment provider slow; retrying");
    Ok(())
}
