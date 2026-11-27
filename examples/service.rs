//! A web service ready to deploy: it listens on every interface, on the
//! port `PORT` names (8080 by default), answers health checks at `/healthz`
//! without a span or a log line for each, and stops cleanly on SIGTERM as
//! well as Ctrl-C. Docker, Kubernetes and systemd all stop a program with
//! SIGTERM, and a program that doesn't handle it loses what it hasn't sent.
//!
//! ```bash
//! cargo run -p oxtel-examples --example service
//! curl localhost:8080/orders/7
//! ```
//!
//! A service of your own needs:
//!
//! ```toml
//! [dependencies]
//! oxtel = "0.4"
//! oxtel-tower = { version = "0.4", features = ["axum"] }
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
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    let app = Router::new()
        .route("/orders/{id}", get(order))
        .layer(oxtel_tower::HttpServerLayer::new())
        // Added after the layer, so health checks aren't traced, logged or
        // counted with the real requests.
        .route("/healthz", get(|| async { "ok\n" }));
    // Every interface, so requests from outside the container arrive.
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    tracing::info!(port, "listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(stop_requested())
        .await?;
    tracing::info!("stopped");
    // ANCHOR_END: serve
    Ok(())
}

// ANCHOR: stop
/// Waits for Ctrl-C, or for SIGTERM, which is how Docker, Kubernetes and
/// systemd ask a program to stop. Then the server finishes the requests in
/// flight, main returns, and the guard sends what's pending.
async fn stop_requested() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut sigterm) => {
                sigterm.recv().await;
            }
            Err(_) => std::future::pending().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
}
// ANCHOR_END: stop
