//! Two services, one calling the other over HTTP, and one trace across
//! both. The shop asks the inventory service before it takes an order: its
//! client sends the trace along in a `traceparent` header, and the
//! inventory's request span continues it. Here both services run in one
//! program, which sends the shop one order and stops, so one command shows
//! both sides; each of yours is a program of its own.
//!
//! ```bash
//! cargo run -p oxtel-examples --example two-services
//! OTEL_TRACES_EXPORTER=console cargo run -p oxtel-examples --example two-services   # and the spans
//! ```
//!
//! Services of your own need:
//!
//! ```toml
//! [dependencies]
//! oxtel = "0.4"
//! oxtel-tower = { version = "0.4", features = ["axum"] }
//! oxtel-reqwest = "0.4"
//! axum = "0.8"
//! reqwest = "0.13"
//! tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
//! tracing = "0.1"
//! ```

use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use oxtel_reqwest::reqwest_middleware::ClientWithMiddleware;
use oxtel_tower::HttpServerLayer;
use tokio::net::TcpListener;

// ANCHOR: inventory
/// The inventory service: how many of an item are in stock.
async fn stock(Path(item): Path<String>) -> String {
    tracing::info!(item, "counting stock");
    "12".into()
}
// ANCHOR_END: inventory

// ANCHOR: shop
/// The shop: it asks the inventory before it takes an order. The client
/// sends the request in a span of its own, inside the order's span, and
/// puts the trace in its headers.
async fn order(
    State(inventory): State<ClientWithMiddleware>,
    Path(item): Path<String>,
) -> Result<String, StatusCode> {
    let stock = inventory
        .get(format!("http://127.0.0.1:3002/stock/{item}"))
        .send()
        .await
        .and_then(|response| Ok(response.error_for_status()?))
        .map_err(|_| StatusCode::BAD_GATEWAY)?
        .text()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    tracing::info!(item, stock, "taking order");
    Ok(format!("ordered {item}\n"))
}
// ANCHOR_END: shop

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // First, so the HTTP layers find oxtel's recorder. Keep the guard until
    // main returns: dropping it sends what's left.
    let _telemetry = oxtel::init()?;

    // ANCHOR: serve
    let inventory = Router::new()
        .route("/stock/{item}", get(stock))
        .layer(HttpServerLayer::new());
    let listener = TcpListener::bind("127.0.0.1:3002").await?;
    tokio::spawn(async move { axum::serve(listener, inventory).await });

    // After `oxtel::init()`, so the client's durations are in seconds.
    let client = oxtel_reqwest::client(reqwest::Client::new());
    let shop = Router::new()
        .route("/orders/{item}", get(order))
        .with_state(client)
        .layer(HttpServerLayer::new());
    let listener = TcpListener::bind("127.0.0.1:3001").await?;
    tokio::spawn(async move { axum::serve(listener, shop).await });
    // ANCHOR_END: serve

    // One order, as a customer would send it, without a trace of its own.
    reqwest::get("http://127.0.0.1:3001/orders/tea")
        .await?
        .error_for_status()?;
    Ok(())
}
