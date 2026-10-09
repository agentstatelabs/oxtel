//! Span context: what identifies a span to the world, and what crosses
//! process boundaries in W3C Trace Context headers.

use std::sync::Arc;

use crate::id::{SpanId, TraceId};

/// W3C trace flags.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Debug)]
pub struct TraceFlags(u8);

impl TraceFlags {
    /// The caller may have recorded this trace.
    pub const SAMPLED: TraceFlags = TraceFlags(0x01);
    /// The trace id's low 56 bits are random (W3C Trace Context Level 2).
    pub const RANDOM: TraceFlags = TraceFlags(0x02);
    /// The flags this implementation understands. Others are dropped when
    /// propagating.
    pub(crate) const KNOWN: u8 = 0x03;

    /// Flags from their byte, keeping only the ones this implementation
    /// understands.
    pub const fn from_u8(bits: u8) -> Self {
        TraceFlags(bits & Self::KNOWN)
    }

    /// The flags as a byte.
    pub const fn to_u8(self) -> u8 {
        self.0
    }

    /// Whether `SAMPLED` is set.
    pub const fn is_sampled(self) -> bool {
        self.0 & Self::SAMPLED.0 != 0
    }

    /// Whether `RANDOM` is set.
    pub const fn is_random(self) -> bool {
        self.0 & Self::RANDOM.0 != 0
    }

    /// These flags with `other` added.
    pub const fn with(self, other: TraceFlags) -> Self {
        TraceFlags(self.0 | other.0)
    }

    /// These flags with `SAMPLED` set or cleared.
    pub const fn with_sampled(self, sampled: bool) -> Self {
        if sampled {
            TraceFlags(self.0 | Self::SAMPLED.0)
        } else {
            TraceFlags(self.0 & !Self::SAMPLED.0)
        }
    }
}

/// A validated W3C `tracestate`: vendor-specific key/value pairs that
/// travel with the trace. Cheap to clone.
#[derive(Clone, PartialEq, Eq, Default, Debug)]
pub struct TraceState(Option<Arc<[(String, String)]>>);

impl TraceState {
    /// No entries.
    pub const EMPTY: TraceState = TraceState(None);

    pub(crate) fn from_entries(entries: Vec<(String, String)>) -> Self {
        if entries.is_empty() {
            TraceState(None)
        } else {
            TraceState(Some(entries.into()))
        }
    }

    /// The entries, most recently updated first.
    pub fn entries(&self) -> &[(String, String)] {
        self.0.as_deref().unwrap_or(&[])
    }

    /// The value for `key`.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries()
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// Whether there are no entries.
    pub fn is_empty(&self) -> bool {
        self.0.is_none()
    }

    /// The `tracestate` header value.
    pub fn header_value(&self) -> String {
        let mut out = String::new();
        for (i, (k, v)) in self.entries().iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(k);
            out.push('=');
            out.push_str(v);
        }
        out
    }
}

/// The identity of a span: its trace, its own id, its flags and trace
/// state, and whether it came from another process.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct SpanContext {
    /// The trace the span belongs to.
    pub trace_id: TraceId,
    /// The span's own id.
    pub span_id: SpanId,
    /// W3C trace flags.
    pub flags: TraceFlags,
    /// W3C trace state.
    pub trace_state: TraceState,
    /// Whether this context was received from another process.
    pub is_remote: bool,
}

impl SpanContext {
    /// Whether both ids are valid.
    pub fn is_valid(&self) -> bool {
        self.trace_id.is_valid() && self.span_id.is_valid()
    }

    /// Whether the span is being recorded and exported.
    pub fn is_sampled(&self) -> bool {
        self.flags.is_sampled()
    }
}
