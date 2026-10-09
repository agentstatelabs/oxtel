//! `traceparent`: never panics; whatever is accepted has valid ids and only
//! known flags, and formats back to a header that parses to the same value.
#![no_main]

use libfuzzer_sys::fuzz_target;
use oxtel_sdk::SpanContext;
use oxtel_sdk::propagation::{format_traceparent, parse_traceparent};

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };
    if let Some((trace_id, span_id, flags)) = parse_traceparent(s) {
        assert!(trace_id.is_valid() && span_id.is_valid());
        assert_eq!(flags.to_u8() & !0x03, 0, "unknown flags kept");
        let cx = SpanContext {
            trace_id,
            span_id,
            flags,
            ..Default::default()
        };
        let header = format_traceparent(&cx);
        assert_eq!(header.len(), 55);
        assert_eq!(parse_traceparent(&header), Some((trace_id, span_id, flags)));
    }
});
