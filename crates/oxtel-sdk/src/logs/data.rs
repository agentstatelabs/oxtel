//! A log record, ready to export.

use std::borrow::Cow;
use std::fmt;
use std::time::SystemTime;

use crate::context::TraceFlags;
use crate::id::{SpanId, TraceId};
use crate::trace::Scope;
use crate::value::{KeyValue, Value};

/// How severe a log record is: OpenTelemetry's severity number, from 1
/// (`TRACE`) to 24 (`FATAL4`). Each of the six ranges has four steps, so
/// log libraries with finer levels keep their order.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Severity(u8);

impl Severity {
    /// Fine-grained debugging (1).
    pub const TRACE: Severity = Severity(1);
    /// Debugging (5).
    pub const DEBUG: Severity = Severity(5);
    /// Informational (9).
    pub const INFO: Severity = Severity(9);
    /// A warning (13).
    pub const WARN: Severity = Severity(13);
    /// An error (17).
    pub const ERROR: Severity = Severity(17);
    /// A fatal error (21).
    pub const FATAL: Severity = Severity(21);

    /// The severity numbered `number`, if it is 1 to 24.
    pub fn new(number: u8) -> Option<Self> {
        (1..=24).contains(&number).then_some(Severity(number))
    }

    /// Its number, 1 to 24.
    pub fn number(self) -> u8 {
        self.0
    }

    /// The name of its range: `TRACE`, `DEBUG`, `INFO`, `WARN`, `ERROR` or
    /// `FATAL`.
    pub fn name(self) -> &'static str {
        ["TRACE", "DEBUG", "INFO", "WARN", "ERROR", "FATAL"][usize::from((self.0 - 1) / 4)]
    }
}

impl fmt::Debug for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.0 - 1) % 4 {
            0 => write!(f, "{}", self.name()),
            step => write!(f, "{}{}", self.name(), step + 1),
        }
    }
}

impl From<tracing_core::Level> for Severity {
    fn from(level: tracing_core::Level) -> Self {
        match level {
            tracing_core::Level::TRACE => Severity::TRACE,
            tracing_core::Level::DEBUG => Severity::DEBUG,
            tracing_core::Level::INFO => Severity::INFO,
            tracing_core::Level::WARN => Severity::WARN,
            tracing_core::Level::ERROR => Severity::ERROR,
        }
    }
}

/// A log record: one `tracing` event.
#[derive(Clone, PartialEq, Debug)]
pub struct LogRecord {
    /// When the event happened.
    pub time: SystemTime,
    /// When oxtel received it. For `tracing` events, the same as `time`.
    pub observed_time: SystemTime,
    /// How severe it is.
    pub severity: Severity,
    /// The severity as the source named it, such as `WARN`.
    pub severity_text: Cow<'static, str>,
    /// The message: the event's `message` field.
    pub body: Option<Value>,
    /// The event's other fields.
    pub attributes: Vec<KeyValue>,
    /// Attributes dropped by the attribute limit.
    pub dropped_attributes: u32,
    /// The event's name, when it was given one with `name:` in the `tracing`
    /// macro.
    pub event_name: Option<Cow<'static, str>>,
    /// The trace of the span it happened in, if any.
    pub trace_id: Option<TraceId>,
    /// The span it happened in, if any.
    pub span_id: Option<SpanId>,
    /// That span's trace flags.
    pub trace_flags: TraceFlags,
    /// What produced it: the event's target, such as `my_app::db`.
    pub scope: Scope,
}
