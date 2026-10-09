//! `tracestate`: never panics; whatever is accepted has at most 32 entries
//! with unique keys, and re-serialises to a value that parses the same.
#![no_main]

use libfuzzer_sys::fuzz_target;
use oxtel_sdk::propagation::parse_tracestate;

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };
    if let Some(ts) = parse_tracestate(s) {
        assert!(ts.entries().len() <= 32);
        let mut keys: Vec<_> = ts.entries().iter().map(|(k, _)| k).collect();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), ts.entries().len(), "duplicate keys accepted");
        assert_eq!(parse_tracestate(&ts.header_value()), Some(ts));
    }
});
