//! Metrics for Prometheus to scrape, and nothing sent anywhere: no
//! collector, no OTLP. With `OTEL_METRICS_EXPORTER=prometheus`, oxtel
//! serves them at http://localhost:9464/metrics, on a thread of its own.
//!
//! ```bash
//! OTEL_METRICS_EXPORTER=prometheus cargo run -p oxtel-examples --example prometheus
//! curl localhost:9464/metrics
//! ```
//!
//! A program of your own needs only these. Without oxtel's default
//! features, it has no OTLP code, and no HTTP or TLS crates: the endpoint
//! needs only the standard library.
//!
//! ```toml
//! [dependencies]
//! oxtel = { version = "0.4", default-features = false }
//! metrics = "0.24"
//! ```

use std::time::Duration;

fn main() -> Result<(), oxtel::Error> {
    // ANCHOR: setup
    // First, so the metrics below find oxtel's recorder. The guard keeps
    // the endpoint running.
    let _telemetry = oxtel::init()?;
    // ANCHOR_END: setup

    // Units and descriptions, for the scrape's `# UNIT` and `# HELP` lines.
    metrics::describe_counter!("orders.placed", "Orders placed, by channel.");
    metrics::describe_gauge!("orders.pending", "Orders placed but not yet shipped.");
    metrics::describe_histogram!(
        "orders.checkout.duration",
        metrics::Unit::Seconds,
        "How long checkout took."
    );

    // Stands in for a real service: an order every second, and every third
    // second two of them shipped. Each scrape reads the values as they are.
    let mut pending: u32 = 0;
    let mut second: u64 = 0;
    loop {
        let channel = if second.is_multiple_of(4) {
            "app"
        } else {
            "web"
        };
        metrics::counter!("orders.placed", "channel" => channel).increment(1);
        let checkout = 0.05 + (second % 5) as f64 * 0.02;
        metrics::histogram!("orders.checkout.duration").record(checkout);
        pending += 1;
        if second % 3 == 2 {
            pending -= pending.min(2);
        }
        metrics::gauge!("orders.pending").set(pending);
        std::thread::sleep(Duration::from_secs(1));
        second += 1;
    }
}
