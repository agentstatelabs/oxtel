//! Carrying a trace across a boundary by hand, for transports oxtel's
//! crates don't cover, such as a message queue's headers. The sender writes
//! the current trace into the message's headers, and the receiver, perhaps
//! another program, continues it, so both are one trace. oxtel-tower and
//! oxtel-reqwest do this for HTTP by themselves.
//!
//! ```bash
//! cargo run -p oxtel-examples --example propagation
//! ```
//!
//! A program of your own needs:
//!
//! ```toml
//! [dependencies]
//! oxtel = "0.4"
//! tracing = "0.1"
//! ```

use std::collections::HashMap;

fn main() -> Result<(), oxtel::Error> {
    let _telemetry = oxtel::init()?;

    // The message's headers, which travel with it.
    let mut headers = HashMap::new();
    tracing::info_span!("send order").in_scope(|| {
        tracing::info!("sending the order");
        inject(&mut headers);
    });
    handle(&headers);
    Ok(())
}

// ANCHOR: propagation
/// A sender: write the current trace into the outgoing headers.
fn inject(outgoing: &mut HashMap<String, String>) {
    oxtel::propagation::inject_current(outgoing);
}

/// A receiver: continue the trace the incoming headers carry.
fn handle(incoming: &HashMap<String, String>) {
    let span = {
        // The next span made on this thread, while this lives, continues
        // the incoming trace.
        let _parent = oxtel::propagation::extract_parent(incoming);
        tracing::info_span!("handle order")
    };
    let _entered = span.enter();
    tracing::info!("handling the order");
}
// ANCHOR_END: propagation
