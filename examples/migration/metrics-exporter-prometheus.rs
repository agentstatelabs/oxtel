//! Before moving to oxtel: `examples/prometheus.rs`, with its metrics served
//! by metrics-exporter-prometheus instead, on its default port, 9000. Only
//! the setup differs; the metrics calls are the same.
//!
//! It isn't one of the workspace's examples, since it needs crates they
//! don't. `cargo run -p xtask -- check-examples` checks it builds with:
//!
//! ```toml
//! [dependencies]
//! metrics = "0.24"
//! metrics-exporter-prometheus = "0.18"
//! ```

use std::time::Duration;

fn main() {
    // ANCHOR: setup
    metrics_exporter_prometheus::PrometheusBuilder::new()
        .install()
        .expect("the recorder and its listener start");
    // ANCHOR_END: setup

    metrics::describe_counter!("orders.placed", "Orders placed, by channel.");
    metrics::describe_gauge!("orders.pending", "Orders placed but not yet shipped.");
    metrics::describe_histogram!(
        "orders.checkout.duration",
        metrics::Unit::Seconds,
        "How long checkout took."
    );

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
