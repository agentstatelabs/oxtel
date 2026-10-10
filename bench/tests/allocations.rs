//! Heap allocations per log event, oxtel against the official stack. A log
//! record's attributes are kept inline, so oxtel allocates only for what
//! both must copy: the message and string values.

use std::alloc::System;

use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

fn event() {
    tracing::info!(user = "u-1", items = 3, cached = true, "listed items");
}

/// Allocations and bytes per event, after a warm-up.
fn per_event(dispatch: &tracing::Dispatch) -> (usize, usize) {
    tracing::dispatcher::with_default(dispatch, || {
        for _ in 0..100 {
            event();
        }
        let region = Region::new(GLOBAL);
        for _ in 0..1000 {
            event();
        }
        let change = region.change();
        (change.allocations / 1000, change.bytes_allocated / 1000)
    })
}

#[test]
fn a_log_event_allocates_no_more_than_the_official_stack() {
    let (_oxtel, oxtel) = oxtel_bench::oxtel_logs(false);
    let (_otel, otel) = oxtel_bench::otel_logs(false);
    let (ours, theirs) = (per_event(&oxtel), per_event(&otel));
    println!("allocations, bytes per event: oxtel {ours:?}, official {theirs:?}");
    assert!(ours.0 <= theirs.0, "oxtel {ours:?}, official {theirs:?}");
    assert_eq!(ours, (2, 15), "the message and the one string value");
}
