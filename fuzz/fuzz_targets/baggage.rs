//! `baggage`: never panics; keeps at most 180 entries with unique keys, and
//! what it keeps survives injection and extraction unchanged.
#![no_main]

use std::collections::HashMap;

use libfuzzer_sys::fuzz_target;
use oxtel_sdk::propagation::{extract_baggage, inject_baggage, parse_baggage};

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };
    let baggage = parse_baggage(s);
    assert!(baggage.entries().len() <= 180);
    let mut keys: Vec<_> = baggage.entries().iter().map(|e| &e.key).collect();
    keys.sort();
    keys.dedup();
    assert_eq!(keys.len(), baggage.entries().len(), "duplicate keys kept");
    let mut carrier = HashMap::new();
    inject_baggage(&baggage, &mut carrier);
    assert_eq!(extract_baggage(&carrier), baggage);
});
