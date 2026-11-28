//! Before moving to oxtel: `examples/logs.rs`, with log lines from
//! tracing-subscriber's `fmt` instead. Only the setup differs; the tracing
//! calls are the same.
//!
//! It isn't one of the workspace's examples, since it needs crates they
//! don't. `cargo run -p xtask -- check-examples` checks it builds with:
//!
//! ```toml
//! [dependencies]
//! tracing = "0.1"
//! tracing-subscriber = { version = "0.3", features = ["env-filter"] }
//! ```

fn main() {
    // ANCHOR: setup
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    // ANCHOR_END: setup

    tracing::info!(user.id = 42, plan = "team", "signed in");

    let request = tracing::info_span!("request", http.route = "/orders", order.id = 7);
    let _entered = request.enter();
    tracing::warn!(attempt = 2, "payment provider slow; retrying");
}
