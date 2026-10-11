//! A web service: a log line for every request, a span around each with its
//! trace id on every line inside, and request durations as metrics. Once
//! `OTEL_EXPORTER_OTLP_ENDPOINT` is set, it all goes to a collector too.
//!
//! ```bash
//! cargo run -p oxtel-examples --example web
//! curl localhost:3000/orders/7
//! ```
//!
//! A service of your own needs:
//!
//! ```toml
//! [dependencies]
//! oxtel = "0.2"
//! oxtel-tower = { version = "0.2", features = ["axum"] }
//! axum = "0.8"
//! tokio = { version = "1", features = ["macros", "rt-multi-thread", "signal"] }
//! tracing = "0.1"
//! ```

use axum::Router;
use axum::extract::Path;
use axum::routing::get;

async fn order(Path(id): Path<u32>) -> String {
    tracing::info!(order.id = id, "looking up order");
    format!("order {id}\n")
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // First, so the HTTP layer finds the metrics recorder. Keep the guard
    // until main returns.
    let _telemetry = oxtel::init()?;

    // ANCHOR: serve
    let app = Router::new()
        .route("/orders/{id}", get(order))
        .layer(oxtel_tower::HttpServerLayer::new());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await?;
    tracing::info!("listening on http://127.0.0.1:3000");
    axum::serve(listener, app)
        // On Ctrl-C, finish the requests in flight and let main return, so
        // the guard is dropped and sends what's pending. Without this,
        // Ctrl-C ends the process at once and the last few seconds of
        // telemetry are lost.
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    // ANCHOR_END: serve
    Ok(())
}
