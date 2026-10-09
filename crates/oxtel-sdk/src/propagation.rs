//! W3C Trace Context and W3C Baggage: reading them from incoming requests
//! and writing them onto outgoing ones.
//!
//! - [Trace Context](https://www.w3.org/TR/trace-context-2/): `traceparent`
//!   and `tracestate`.
//! - [Baggage](https://www.w3.org/TR/baggage/): `baggage`.
//!
//! Parsing is strict where the specs say "MUST" and drops what it can't
//! read, never the request: an invalid `traceparent` means "no parent", an
//! invalid `tracestate` is ignored as a whole, and an invalid baggage member
//! is skipped.

use std::collections::HashMap;

use crate::context::{SpanContext, TraceFlags, TraceState};
use crate::id::{SpanId, TraceId};

/// The `traceparent` header.
pub const TRACEPARENT: &str = "traceparent";
/// The `tracestate` header.
pub const TRACESTATE: &str = "tracestate";
/// The `baggage` header.
pub const BAGGAGE: &str = "baggage";

/// Most `tracestate` entries accepted.
const TRACESTATE_MAX_MEMBERS: usize = 32;
/// Most baggage entries accepted.
const BAGGAGE_MAX_MEMBERS: usize = 180;
/// Most bytes of `baggage` read.
const BAGGAGE_MAX_BYTES: usize = 8192;

/// Reads headers (or any key/value carrier) for extraction.
pub trait Extractor {
    /// The value for `key`, matched case-insensitively. Several values for
    /// one key are joined with commas, as HTTP allows.
    fn get(&self, key: &str) -> Option<String>;
}

/// Writes headers (or any key/value carrier) for injection.
pub trait Injector {
    /// Set `key` to `value`, replacing any earlier value.
    fn set(&mut self, key: &str, value: String);
}

impl Extractor for HashMap<String, String> {
    fn get(&self, key: &str) -> Option<String> {
        self.get(key)
            .or_else(|| {
                self.iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(key))
                    .map(|(_, v)| v)
            })
            .cloned()
    }
}

impl Injector for HashMap<String, String> {
    fn set(&mut self, key: &str, value: String) {
        self.insert(key.to_string(), value);
    }
}

#[cfg(feature = "http")]
impl Extractor for http::HeaderMap {
    fn get(&self, key: &str) -> Option<String> {
        let mut values = self.get_all(key).iter().filter_map(|v| v.to_str().ok());
        let first = values.next()?;
        Some(values.fold(first.to_string(), |mut joined, v| {
            joined.push(',');
            joined.push_str(v);
            joined
        }))
    }
}

#[cfg(feature = "http")]
impl Injector for http::HeaderMap {
    fn set(&mut self, key: &str, value: String) {
        if let (Ok(name), Ok(value)) = (
            http::header::HeaderName::from_bytes(key.as_bytes()),
            http::header::HeaderValue::from_str(&value),
        ) {
            self.insert(name, value);
        }
    }
}

// ─── Trace Context ──────────────────────────────────────────

/// The remote parent described by `traceparent` and `tracestate`, if
/// `traceparent` is present and valid.
pub fn extract_trace_context(carrier: &dyn Extractor) -> Option<SpanContext> {
    let (trace_id, span_id, flags) = parse_traceparent(&carrier.get(TRACEPARENT)?)?;
    let trace_state = carrier
        .get(TRACESTATE)
        .and_then(|v| parse_tracestate(&v))
        .unwrap_or_default();
    Some(SpanContext {
        trace_id,
        span_id,
        flags,
        trace_state,
        is_remote: true,
    })
}

/// Write `traceparent`, and `tracestate` when it has entries. An invalid
/// context writes nothing.
pub fn inject_trace_context(cx: &SpanContext, carrier: &mut dyn Injector) {
    if !cx.is_valid() {
        return;
    }
    carrier.set(TRACEPARENT, format_traceparent(cx));
    if !cx.trace_state.is_empty() {
        carrier.set(TRACESTATE, cx.trace_state.header_value());
    }
}

/// `00-{trace-id}-{parent-id}-{flags}`.
pub fn format_traceparent(cx: &SpanContext) -> String {
    format!("00-{}-{}-{:02x}", cx.trace_id, cx.span_id, cx.flags.to_u8())
}

/// Parse a `traceparent` value. Versions above `00` are read as `00`, as
/// the spec requires, provided their first four fields have the same shape.
pub fn parse_traceparent(value: &str) -> Option<(TraceId, SpanId, TraceFlags)> {
    let value = value.trim_matches(|c| c == ' ' || c == '\t');
    let bytes = value.as_bytes();
    if bytes.len() < 55 {
        return None;
    }
    let version = &value[..2];
    if !is_lower_hex(version) || version == "ff" {
        return None;
    }
    if version == "00" {
        if bytes.len() != 55 {
            return None;
        }
    } else if bytes.len() > 55 && bytes[55] != b'-' {
        return None;
    }
    if bytes[2] != b'-' || bytes[35] != b'-' || bytes[52] != b'-' {
        return None;
    }
    let trace_id = TraceId::from_hex(&value[3..35]).filter(TraceId::is_valid)?;
    let span_id = SpanId::from_hex(&value[36..52]).filter(SpanId::is_valid)?;
    let flags = &value[53..55];
    if !is_lower_hex(flags) {
        return None;
    }
    let flags = u8::from_str_radix(flags, 16).ok()?;
    Some((trace_id, span_id, TraceFlags::from_u8(flags)))
}

/// Parse a `tracestate` value. Any invalid entry, a duplicate key, or more
/// than 32 entries discards the whole value. Empty entries are skipped.
pub fn parse_tracestate(value: &str) -> Option<TraceState> {
    let mut entries: Vec<(String, String)> = Vec::new();
    for member in value.split(',') {
        let member = member.trim_matches(|c| c == ' ' || c == '\t');
        if member.is_empty() {
            continue;
        }
        let (key, val) = member.split_once('=')?;
        if !valid_tracestate_key(key) || !valid_tracestate_value(val) {
            return None;
        }
        if entries.iter().any(|(k, _)| k == key) {
            return None;
        }
        entries.push((key.to_string(), val.to_string()));
        if entries.len() > TRACESTATE_MAX_MEMBERS {
            return None;
        }
    }
    Some(TraceState::from_entries(entries))
}

fn is_lower_hex(s: &str) -> bool {
    s.bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn key_char(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-' | b'*' | b'/')
}

fn valid_tracestate_key(key: &str) -> bool {
    let b = key.as_bytes();
    match key.split_once('@') {
        None => {
            !b.is_empty()
                && b.len() <= 256
                && b[0].is_ascii_lowercase()
                && b[1..].iter().all(|&c| key_char(c))
        }
        Some((tenant, system)) => {
            let (t, s) = (tenant.as_bytes(), system.as_bytes());
            !t.is_empty()
                && t.len() <= 241
                && (t[0].is_ascii_lowercase() || t[0].is_ascii_digit())
                && t[1..].iter().all(|&c| key_char(c))
                && !s.is_empty()
                && s.len() <= 14
                && s[0].is_ascii_lowercase()
                && s[1..].iter().all(|&c| key_char(c))
        }
    }
}

fn valid_tracestate_value(value: &str) -> bool {
    let b = value.as_bytes();
    let chr = |c: u8| (0x20..=0x7e).contains(&c) && c != b',' && c != b'=';
    !b.is_empty() && b.len() <= 256 && b.iter().all(|&c| chr(c)) && b[b.len() - 1] != b' '
}

// ─── Baggage ────────────────────────────────────────────────

/// One baggage entry.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BaggageEntry {
    /// The key, an HTTP token.
    pub key: String,
    /// The value, percent-decoded.
    pub value: String,
    /// The entry's properties, as written after the first `;`, or empty.
    pub metadata: String,
}

/// W3C baggage: application key/value pairs that travel with a request.
#[derive(Clone, PartialEq, Eq, Default, Debug)]
pub struct Baggage {
    entries: Vec<BaggageEntry>,
}

impl Baggage {
    /// The entries, in header order.
    pub fn entries(&self) -> &[BaggageEntry] {
        &self.entries
    }

    /// The value for `key`.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|e| e.key == key)
            .map(|e| e.value.as_str())
    }

    /// Set `key` to `value`, replacing an existing entry. Returns false,
    /// changing nothing, if `key` isn't an HTTP token.
    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<String>) -> bool {
        let key = key.into();
        if !is_token(&key) {
            return false;
        }
        let value = value.into();
        match self.entries.iter_mut().find(|e| e.key == key) {
            Some(entry) => entry.value = value,
            None => self.entries.push(BaggageEntry {
                key,
                value,
                metadata: String::new(),
            }),
        }
        true
    }

    /// Whether there are no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// The baggage in `carrier`. Invalid entries are skipped; at most 180
/// entries and 8192 bytes are read.
pub fn extract_baggage(carrier: &dyn Extractor) -> Baggage {
    carrier
        .get(BAGGAGE)
        .map(|v| parse_baggage(&v))
        .unwrap_or_default()
}

/// Write `baggage`, if there are entries.
pub fn inject_baggage(baggage: &Baggage, carrier: &mut dyn Injector) {
    if !baggage.is_empty() {
        carrier.set(BAGGAGE, format_baggage(baggage));
    }
}

/// Parse a `baggage` value.
pub fn parse_baggage(value: &str) -> Baggage {
    let mut entries = Vec::new();
    let mut used = 0usize;
    for member in value.split(',') {
        used += member.len() + 1;
        if used > BAGGAGE_MAX_BYTES + 1 || entries.len() == BAGGAGE_MAX_MEMBERS {
            break;
        }
        if let Some(entry) = parse_baggage_member(member) {
            entries.retain(|e: &BaggageEntry| e.key != entry.key);
            entries.push(entry);
        }
    }
    Baggage { entries }
}

fn ows(s: &str) -> &str {
    s.trim_matches(|c| c == ' ' || c == '\t')
}

fn parse_baggage_member(member: &str) -> Option<BaggageEntry> {
    let (pair, metadata) = match member.split_once(';') {
        Some((pair, props)) => (pair, ows(props)),
        None => (member, ""),
    };
    let (key, value) = pair.split_once('=')?;
    let key = ows(key);
    let value = ows(value);
    if !is_token(key) || !value.bytes().all(baggage_octet_or_percent) {
        return None;
    }
    if !metadata.is_empty() && !metadata.split(';').all(valid_baggage_property) {
        return None;
    }
    Some(BaggageEntry {
        key: key.to_string(),
        value: percent_decode(value)?,
        metadata: metadata.to_string(),
    })
}

fn valid_baggage_property(property: &str) -> bool {
    match property.split_once('=') {
        Some((k, v)) => is_token(ows(k)) && ows(v).bytes().all(baggage_octet_or_percent),
        None => is_token(ows(property)),
    }
}

fn format_baggage(baggage: &Baggage) -> String {
    let mut out = String::new();
    for (i, e) in baggage.entries.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&e.key);
        out.push('=');
        percent_encode_into(&e.value, &mut out);
        if !e.metadata.is_empty() {
            out.push(';');
            out.push_str(&e.metadata);
        }
    }
    out
}

/// RFC 7230 `token`.
fn is_token(s: &str) -> bool {
    !s.is_empty()
        && s.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

/// W3C `baggage-octet`: printable ASCII except space, `"`, `,`, `;` and `\`.
fn baggage_octet(b: u8) -> bool {
    matches!(b, 0x21 | 0x23..=0x2b | 0x2d..=0x3a | 0x3c..=0x5b | 0x5d..=0x7e)
}

fn baggage_octet_or_percent(b: u8) -> bool {
    baggage_octet(b) || b == b'%'
}

fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn percent_encode_into(s: &str, out: &mut String) {
    for b in s.bytes() {
        if baggage_octet(b) && b != b'%' {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";

    #[test]
    fn traceparent_valid_and_round_trip() {
        let (t, s, f) = parse_traceparent(VALID).unwrap();
        assert_eq!(t.to_string(), "4bf92f3577b34da6a3ce929d0e0e4736");
        assert_eq!(s.to_string(), "00f067aa0ba902b7");
        assert!(f.is_sampled());
        let cx = SpanContext {
            trace_id: t,
            span_id: s,
            flags: f,
            ..Default::default()
        };
        assert_eq!(format_traceparent(&cx), VALID);
    }

    #[test]
    fn traceparent_rejections() {
        for bad in [
            "",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-0",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01-extra",
            "ff-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            "00-4BF92F3577B34DA6A3CE929D0E0E4736-00f067aa0ba902b7-01",
            "00-00000000000000000000000000000000-00f067aa0ba902b7-01",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-0g",
            "00_4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            "0g-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            "01-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01x",
        ] {
            assert!(parse_traceparent(bad).is_none(), "accepted {bad:?}");
        }
    }

    #[test]
    fn future_versions_are_read_as_00() {
        let (_, _, f) = parse_traceparent(
            "cc-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-03-what-the-future-holds",
        )
        .unwrap();
        assert!(f.is_sampled() && f.is_random());
        assert!(
            parse_traceparent("cc-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01").is_some()
        );
    }

    #[test]
    fn unknown_flags_are_dropped() {
        let (_, _, f) =
            parse_traceparent("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-ff").unwrap();
        assert_eq!(f.to_u8(), 0x03);
    }

    #[test]
    fn tracestate_parsing() {
        let ts =
            parse_tracestate("rojo=00f067aa0ba902b7, congo=t61rcWkgMzE,,tenant@vendor=x").unwrap();
        assert_eq!(ts.get("congo"), Some("t61rcWkgMzE"));
        assert_eq!(ts.get("tenant@vendor"), Some("x"));
        assert_eq!(
            ts.header_value(),
            "rojo=00f067aa0ba902b7,congo=t61rcWkgMzE,tenant@vendor=x"
        );
        // Whitespace around members is optional whitespace, not part of the
        // value; spaces inside a value are allowed.
        assert_eq!(
            parse_tracestate("rojo=a b ").unwrap().get("rojo"),
            Some("a b")
        );
        for bad in [
            "Rojo=1",
            "rojo=1,rojo=2",
            "rojo",
            "rojo=a,b",
            "rojo=a=b",
            "rojo=caf\u{e9}",
            "1abc=x",
            "t@toolongsystemidxx=x",
        ] {
            assert!(parse_tracestate(bad).is_none(), "accepted {bad:?}");
        }
        let many: Vec<String> = (0..33).map(|i| format!("k{i}=v")).collect();
        assert!(parse_tracestate(&many.join(",")).is_none());
        let ok: Vec<String> = (0..32).map(|i| format!("k{i}=v")).collect();
        assert_eq!(parse_tracestate(&ok.join(",")).unwrap().entries().len(), 32);
    }

    #[test]
    fn extract_and_inject_trace_context() {
        let mut headers = HashMap::new();
        headers.insert("Traceparent".to_string(), VALID.to_string());
        headers.insert("tracestate".to_string(), "rojo=1".to_string());
        let cx = extract_trace_context(&headers).unwrap();
        assert!(cx.is_remote && cx.is_sampled());
        assert_eq!(cx.trace_state.get("rojo"), Some("1"));

        let mut out = HashMap::new();
        inject_trace_context(&cx, &mut out);
        assert_eq!(out.get(TRACEPARENT).unwrap(), VALID);
        assert_eq!(out.get(TRACESTATE).unwrap(), "rojo=1");

        let mut none = HashMap::new();
        inject_trace_context(&SpanContext::default(), &mut none);
        assert!(none.is_empty());
    }

    #[test]
    fn baggage_parsing() {
        let b = parse_baggage(
            "userId=alice, serverNode = DF%2028 ;prop1=x;flag, isProduction=false,bad key=1, =v",
        );
        assert_eq!(b.get("userId"), Some("alice"));
        assert_eq!(b.get("serverNode"), Some("DF 28"));
        assert_eq!(b.entries()[1].metadata, "prop1=x;flag");
        assert_eq!(b.get("isProduction"), Some("false"));
        assert_eq!(b.entries().len(), 3, "invalid members are skipped");
        assert!(parse_baggage("k=%zz").is_empty());
        assert!(parse_baggage("k=%ff").is_empty(), "not UTF-8");
    }

    #[test]
    fn baggage_round_trip_and_limits() {
        let mut b = Baggage::default();
        assert!(b.insert("user", "Ana María, \"x\";y"));
        assert!(!b.insert("bad key", "v"));
        let mut out = HashMap::new();
        inject_baggage(&b, &mut out);
        let header = out.get(BAGGAGE).unwrap().clone();
        assert_eq!(parse_baggage(&header), b);

        let many: Vec<String> = (0..200).map(|i| format!("k{i}=v")).collect();
        assert_eq!(parse_baggage(&many.join(",")).entries().len(), 180);
        let big = format!("a={},b=1", "x".repeat(9000));
        assert!(parse_baggage(&big).is_empty());
    }
}
