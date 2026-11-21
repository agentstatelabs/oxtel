//! The application half of a library scenario. The library, `shipping`
//! (in `examples/shipping/`), records spans, log lines and metrics with
//! `tracing` and `metrics` alone, and has no oxtel dependency. This
//! program uses it, and it's the program that calls `oxtel::init()` and so
//! decides where all of it goes, the library's telemetry included.
//!
//! ```bash
//! cargo run -p oxtel-examples --example library
//! RUST_LOG=info,shipping=debug cargo run -p oxtel-examples --example library   # the library's details too
//! ```
//!
//! The application needs these, beside the library:
//!
//! ```toml
//! [dependencies]
//! oxtel = "0.3"
//! shipping = { path = "shipping" }
//! tracing = "0.1"
//! ```

fn main() -> Result<(), oxtel::Error> {
    // The application chooses: log lines here, and traces, metrics and
    // logs to a collector once an endpoint is set. Keep the guard.
    let _telemetry = oxtel::init()?;

    for (grams, country) in [(800, "FR"), (24_000, "US"), (500, "AQ")] {
        match shipping::quote(grams, country) {
            Ok(cents) => tracing::info!(grams, country, cents, "quoted"),
            Err(e) => tracing::warn!(grams, country, error = %e, "can't quote"),
        }
    }
    Ok(())
}
