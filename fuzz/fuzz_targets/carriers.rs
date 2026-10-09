//! Extraction from a carrier whose `traceparent`, `tracestate` and
//! `baggage` all come from the input (split on NUL), as a request's headers
//! would: never panics, and an extracted context injects and re-extracts
//! unchanged.
#![no_main]

use std::collections::HashMap;

use libfuzzer_sys::fuzz_target;
use oxtel_sdk::propagation::{extract_baggage, extract_trace_context, inject_trace_context};

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };
    let mut parts = s.splitn(3, '\0');
    let mut carrier = HashMap::new();
    for name in ["traceparent", "tracestate", "baggage"] {
        if let Some(value) = parts.next() {
            carrier.insert(name.to_string(), value.to_string());
        }
    }
    let _ = extract_baggage(&carrier);
    if let Some(cx) = extract_trace_context(&carrier) {
        assert!(cx.is_valid() && cx.is_remote);
        let mut out = HashMap::new();
        inject_trace_context(&cx, &mut out);
        assert_eq!(extract_trace_context(&out), Some(cx));
    }
});
