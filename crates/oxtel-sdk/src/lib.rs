//! The core of [oxtel](https://github.com/agentstatelabs/oxtel):
//! OpenTelemetry's data model, W3C propagation, the `tracing` layers that
//! turn spans into OpenTelemetry spans and events into log records, and the
//! `metrics` recorder.
//!
//! Most programs use the `oxtel` crate, which sets all of this up from the
//! standard `OTEL_*` configuration. Use this crate directly to assemble a
//! pipeline yourself or to write an exporter.

pub mod console;
pub mod context;
pub mod export;
pub mod id;
pub mod logs;
pub mod metrics;
pub mod propagation;
pub mod resource;
pub mod sdk_metrics;
pub mod trace;
pub mod value;

pub use context::{SpanContext, TraceFlags, TraceState};
pub use id::{SpanId, TraceId};
pub use resource::Resource;
pub use value::{Key, KeyValue, Value};

use std::cell::Cell;

thread_local! {
    static SUPPRESSED: Cell<bool> = const { Cell::new(false) };
}

/// Run `f` with oxtel's own telemetry switched off on this thread, so that
/// an exporter's spans and events (an HTTP client's, for instance) don't
/// feed back into the pipeline exporting them. Export threads run under
/// it already.
pub fn suppress<R>(f: impl FnOnce() -> R) -> R {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            SUPPRESSED.with(|s| s.set(self.0));
        }
    }
    let _restore = Restore(SUPPRESSED.with(|s| s.replace(true)));
    f()
}

/// Whether [`suppress`] is in effect on this thread.
pub fn is_suppressed() -> bool {
    SUPPRESSED.with(Cell::get)
}
