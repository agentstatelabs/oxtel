//! Properties of the W3C Trace Context and Baggage parsers, and of the
//! `key=value` list parser for `OTEL_RESOURCE_ATTRIBUTES`. These parsers
//! read input from other services and from configuration, so beyond
//! "never panics", whatever they accept must re-serialise to something that
//! parses back to the same value, and the specs' limits must hold. The
//! coverage-guided fuzz targets in `fuzz/` check the same properties
//! against inputs nobody wrote.

use std::collections::HashMap;

use oxtel_sdk::context::{SpanContext, TraceFlags};
use oxtel_sdk::propagation::{
    Baggage, extract_baggage, extract_trace_context, format_traceparent, inject_baggage,
    inject_trace_context, parse_baggage, parse_traceparent, parse_tracestate,
};
use oxtel_sdk::resource::parse_resource_attributes;
use oxtel_sdk::{SpanId, TraceId};
use proptest::prelude::*;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A tracestate key the spec allows: simple, or tenant@system.
fn tracestate_key() -> impl Strategy<Value = String> {
    prop_oneof![
        "[a-z][a-z0-9_*/-]{0,30}",
        "[a-z0-9][a-z0-9_*/-]{0,20}@[a-z][a-z0-9_*/-]{0,13}",
    ]
}

/// A tracestate value: printable ASCII except `,` and `=`, not ending in a
/// space.
fn tracestate_value() -> impl Strategy<Value = String> {
    "[!-+\\--<>-~ ]{0,40}[!-+\\--<>-~]"
}

proptest! {
    // Shrunk failures become unit tests next to the code they cover, so
    // proptest's regression files aren't needed.
    #![proptest_config(ProptestConfig {
        failure_persistence: None,
        ..ProptestConfig::with_cases(2000)
    })]

    #[test]
    fn no_input_panics(s in any::<String>()) {
        let _ = parse_traceparent(&s);
        let _ = parse_tracestate(&s);
        let _ = parse_baggage(&s);
        let _ = parse_resource_attributes(&s);
        let mut carrier = HashMap::new();
        carrier.insert("traceparent".to_string(), s.clone());
        carrier.insert("tracestate".to_string(), s.clone());
        carrier.insert("baggage".to_string(), s.clone());
        let _ = extract_trace_context(&carrier);
        let _ = extract_baggage(&carrier);
    }

    /// Long, mostly ASCII strings shaped like a traceparent, with arbitrary
    /// characters (including multi-byte ones) mixed in at any position.
    #[test]
    fn traceparent_shaped_input_never_panics(
        mut chars in proptest::collection::vec(any::<char>(), 50..80),
        dashes in proptest::bool::ANY,
    ) {
        if dashes && chars.len() > 52 {
            for i in [2, 35, 52] { chars[i] = '-'; }
        }
        let s: String = chars.into_iter().collect();
        let _ = parse_traceparent(&s);
    }

    #[test]
    fn valid_traceparents_parse_and_round_trip(
        trace in any::<[u8; 16]>(),
        span in any::<[u8; 8]>(),
        flags in any::<u8>(),
    ) {
        prop_assume!(trace != [0; 16] && span != [0; 8]);
        let header = format!("00-{}-{}-{flags:02x}", hex(&trace), hex(&span));
        let (t, s, f) = parse_traceparent(&header).expect("valid header rejected");
        prop_assert_eq!(t, TraceId::from_bytes(trace));
        prop_assert_eq!(s, SpanId::from_bytes(span));
        prop_assert_eq!(f.to_u8(), flags & 0x03, "only known flags are kept");
        let cx = SpanContext { trace_id: t, span_id: s, flags: f, ..Default::default() };
        let formatted = format_traceparent(&cx);
        prop_assert_eq!(formatted.len(), 55);
        prop_assert!(formatted.bytes().all(|b| !b.is_ascii_uppercase()));
        prop_assert_eq!(parse_traceparent(&formatted), Some((t, s, f)));
    }

    #[test]
    fn future_versions_parse_as_00_and_ff_never_does(
        trace in any::<[u8; 16]>(),
        span in any::<[u8; 8]>(),
        version in 1u8..=254,
        suffix in "(-[a-z0-9]{0,20})?",
    ) {
        prop_assume!(trace != [0; 16] && span != [0; 8]);
        let ids = format!("{}-{}-01", hex(&trace), hex(&span));
        let header = format!("{version:02x}-{ids}{suffix}");
        prop_assert!(parse_traceparent(&header).is_some(), "{}", header);
        let ff = format!("ff-{ids}");
        prop_assert!(parse_traceparent(&ff).is_none());
    }

    #[test]
    fn uppercase_or_zero_ids_are_rejected(
        trace in any::<[u8; 16]>(),
        span in any::<[u8; 8]>(),
    ) {
        let upper = format!("00-{}-{}-01", hex(&trace).to_uppercase(), hex(&span));
        if hex(&trace).bytes().any(|b| b.is_ascii_alphabetic()) {
            prop_assert!(parse_traceparent(&upper).is_none());
        }
        let zero_trace = format!("00-{}-{}-01", "0".repeat(32), hex(&span));
        prop_assert!(parse_traceparent(&zero_trace).is_none());
        let zero_span = format!("00-{}-{}-01", hex(&trace), "0".repeat(16));
        prop_assert!(parse_traceparent(&zero_span).is_none());
    }

    #[test]
    fn valid_tracestates_round_trip(
        entries in proptest::collection::vec((tracestate_key(), tracestate_value()), 0..32),
        spaces in "[ \t]{0,2}",
    ) {
        let mut seen = std::collections::HashSet::new();
        let entries: Vec<(String, String)> =
            entries.into_iter().filter(|(k, _)| seen.insert(k.clone())).collect();
        let header = entries
            .iter()
            .map(|(k, v)| format!("{spaces}{k}={v}{spaces}"))
            .collect::<Vec<_>>()
            .join(",");
        let parsed = parse_tracestate(&header).expect("valid tracestate rejected");
        prop_assert_eq!(parsed.entries(), entries.as_slice());
        prop_assert_eq!(parse_tracestate(&parsed.header_value()), Some(parsed));
    }

    #[test]
    fn accepted_tracestates_obey_the_limits(s in "[a-z0-9=, @_*/-]{0,400}") {
        if let Some(ts) = parse_tracestate(&s) {
            prop_assert!(ts.entries().len() <= 32);
            let keys: std::collections::HashSet<_> = ts.entries().iter().map(|(k, _)| k).collect();
            prop_assert_eq!(keys.len(), ts.entries().len(), "duplicate keys accepted");
            prop_assert_eq!(parse_tracestate(&ts.header_value()), Some(ts));
        }
    }

    #[test]
    fn baggage_values_round_trip_through_percent_encoding(
        entries in proptest::collection::vec(("[A-Za-z0-9!#$&'*+.^_`|~-]{1,20}", any::<String>()), 0..20),
    ) {
        let mut baggage = Baggage::default();
        for (k, v) in &entries {
            prop_assert!(baggage.insert(k.clone(), v.clone()));
        }
        let mut carrier = HashMap::new();
        inject_baggage(&baggage, &mut carrier);
        prop_assert_eq!(extract_baggage(&carrier), baggage);
    }

    #[test]
    fn parsed_baggage_obeys_the_limits(s in "[a-z0-9=,; %]{0,3000}") {
        let b = parse_baggage(&s);
        prop_assert!(b.entries().len() <= 180);
        let keys: std::collections::HashSet<_> = b.entries().iter().map(|e| &e.key).collect();
        prop_assert_eq!(keys.len(), b.entries().len(), "duplicate keys kept");
    }

    #[test]
    fn trace_context_round_trips_through_headers(
        trace in any::<[u8; 16]>(),
        span in any::<[u8; 8]>(),
        sampled in any::<bool>(),
        state in proptest::collection::vec((tracestate_key(), tracestate_value()), 0..5),
    ) {
        prop_assume!(trace != [0; 16] && span != [0; 8]);
        let mut seen = std::collections::HashSet::new();
        let state: Vec<String> = state
            .into_iter()
            .filter(|(k, _)| seen.insert(k.clone()))
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        let cx = SpanContext {
            trace_id: TraceId::from_bytes(trace),
            span_id: SpanId::from_bytes(span),
            flags: TraceFlags::RANDOM.with_sampled(sampled),
            trace_state: parse_tracestate(&state.join(",")).unwrap(),
            is_remote: true,
        };
        let mut carrier = HashMap::new();
        inject_trace_context(&cx, &mut carrier);
        prop_assert_eq!(extract_trace_context(&carrier), Some(cx));
    }
}
