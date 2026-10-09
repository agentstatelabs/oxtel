//! The `key=value,...` lists read from configuration:
//! `OTEL_RESOURCE_ATTRIBUTES` and `OTEL_EXPORTER_OTLP_HEADERS`. Never
//! panics, and never yields an empty key.
#![no_main]

use libfuzzer_sys::fuzz_target;
use oxtel_sdk::resource::parse_resource_attributes;

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };
    for kv in parse_resource_attributes(s) {
        assert!(!kv.key.is_empty());
    }
    for (name, _) in oxtel_otlp::parse_headers(s) {
        assert!(!name.is_empty());
    }
});
