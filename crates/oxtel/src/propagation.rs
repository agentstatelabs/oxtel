//! Carrying trace context across process boundaries, as `OTEL_PROPAGATORS`
//! configures: W3C Trace Context unless it's left out.
//!
//! ```
//! use std::collections::HashMap;
//!
//! // A server: continue the caller's trace.
//! let incoming: HashMap<String, String> = HashMap::new();
//! let span = {
//!     let _parent = oxtel::propagation::extract_parent(&incoming);
//!     tracing::info_span!("handle", otel.kind = "server")
//! };
//!
//! // A client: pass the current trace on.
//! let mut outgoing: HashMap<String, String> = HashMap::new();
//! span.in_scope(|| oxtel::propagation::inject_current(&mut outgoing));
//! ```
//!
//! Baggage can be read and written with [`oxtel_sdk::propagation`].

use std::sync::atomic::{AtomicBool, Ordering};

use oxtel_sdk::propagation::{extract_trace_context, inject_trace_context};
use oxtel_sdk::trace::{RemoteParentGuard, attach_remote_parent, current_span_context};

pub use oxtel_sdk::propagation::{Extractor, Injector};

static TRACE_CONTEXT: AtomicBool = AtomicBool::new(true);

pub(crate) fn set_trace_context(enabled: bool) {
    TRACE_CONTEXT.store(enabled, Ordering::Relaxed);
}

/// Write the current span's context into `carrier` (`traceparent` and
/// `tracestate`). Returns whether there was one to write.
pub fn inject_current(carrier: &mut dyn Injector) -> bool {
    if !TRACE_CONTEXT.load(Ordering::Relaxed) {
        return false;
    }
    match current_span_context() {
        Some(cx) => {
            inject_trace_context(&cx, carrier);
            true
        }
        None => false,
    }
}

/// Read the caller's context from `carrier` and make it the parent of the
/// next span created on this thread, while the guard lives. `None` if the
/// carrier has no valid context.
pub fn extract_parent(carrier: &dyn Extractor) -> Option<RemoteParentGuard> {
    if !TRACE_CONTEXT.load(Ordering::Relaxed) {
        return None;
    }
    extract_trace_context(carrier).map(attach_remote_parent)
}
