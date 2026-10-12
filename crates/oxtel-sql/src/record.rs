//! Recording one database call: a client span, and the
//! `db.client.operation.duration` histogram.

// The clients are features; without one, only the public pieces are used.
#![cfg_attr(
    not(any(
        feature = "rusqlite",
        feature = "postgres",
        feature = "sqlx-0.8",
        feature = "sqlx-0.9"
    )),
    allow(dead_code)
)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use metrics::Label;
use tracing::field::Empty;

use crate::query;

static QUERY_TEXT: AtomicBool = AtomicBool::new(true);

/// Record `db.query.text` on spans: the statement's text with every literal
/// replaced by `?`. Default: true, as the conventions advise for text
/// without values. It applies to the whole process.
pub fn record_query_text(enabled: bool) {
    QUERY_TEXT.store(enabled, Ordering::Relaxed);
}

/// Whether `db.query.text` is recorded; see [`record_query_text`]. For
/// instrumentation outside this crate that follows the same setting.
pub fn records_query_text() -> bool {
    QUERY_TEXT.load(Ordering::Relaxed)
}

/// What a statement's text says, worked out once per distinct text.
pub(crate) struct Analysis {
    pub(crate) summary: Option<String>,
    pub(crate) text: String,
}

/// What a statement whose text isn't known is recorded as: no summary, so
/// the span is named by the database system, and no text.
#[cfg(any(feature = "postgres", feature = "sqlx-0.8", feature = "sqlx-0.9"))]
pub(crate) static UNKNOWN: Analysis = Analysis {
    summary: None,
    text: String::new(),
};

/// Distinct statement texts remembered; the cache starts over past this.
const CACHE_LIMIT: usize = 1024;

pub(crate) fn analyze(sql: &str) -> Arc<Analysis> {
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<Analysis>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Mutex::default);
    if let Some(a) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(sql) {
        return Arc::clone(a);
    }
    let analysis = Arc::new(Analysis {
        summary: query::summarize(sql),
        text: query::sanitize(sql),
    });
    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
    if cache.len() >= CACHE_LIMIT {
        cache.clear();
    }
    cache.insert(sql.to_string(), Arc::clone(&analysis));
    analysis
}

/// Why a call failed.
pub(crate) struct Failure<'a> {
    /// `error.type`: the database's code for the error when it gave one,
    /// otherwise the client library's error type.
    pub(crate) error_type: &'a str,
    /// `db.response.status_code`: the database's own code, such as a
    /// PostgreSQL SQLSTATE.
    pub(crate) status_code: Option<&'a str>,
}

#[cfg(any(feature = "postgres", feature = "sqlx-0.8", feature = "sqlx-0.9"))]
impl Failure<'_> {
    /// Mark a call's span as failed.
    pub(crate) fn record(&self, span: &tracing::Span) {
        span.record("error.type", self.error_type);
        if let Some(code) = self.status_code {
            span.record("db.response.status_code", code);
        }
        span.record("otel.status_code", "error");
    }
}

/// One call to a database.
pub(crate) struct Call<'a> {
    /// `db.system.name`, such as `sqlite` or `postgresql`.
    pub(crate) system: &'static str,
    pub(crate) namespace: Option<&'a str>,
    pub(crate) server: Option<(&'a str, u16)>,
    pub(crate) analysis: &'a Analysis,
}

impl Call<'_> {
    /// A client span for the call, named by its query summary, or the
    /// database system when there's none.
    #[cfg(any(feature = "rusqlite", feature = "postgres"))]
    pub(crate) fn span(&self) -> tracing::Span {
        let span = open_span(self.system, self.namespace, self.server);
        describe(&span, self.analysis);
        span
    }

    /// Record `db.client.operation.duration`, with why the call failed if
    /// it did.
    pub(crate) fn record_duration(&self, seconds: f64, failure: Option<&Failure<'_>>) {
        let mut labels = Vec::with_capacity(8);
        labels.push(Label::new("db.system.name", self.system));
        if let Some(namespace) = self.namespace {
            labels.push(Label::new("db.namespace", namespace.to_string()));
        }
        if let Some(summary) = &self.analysis.summary {
            labels.push(Label::new("db.query.summary", summary.clone()));
        }
        if let Some((address, port)) = self.server {
            labels.push(Label::new("server.address", address.to_string()));
            labels.push(Label::new("server.port", port.to_string()));
        }
        if let Some(failure) = failure {
            labels.push(Label::new("error.type", failure.error_type.to_string()));
            if let Some(code) = failure.status_code {
                labels.push(Label::new("db.response.status_code", code.to_string()));
            }
        }
        metrics::histogram!("db.client.operation.duration", labels).record(seconds);
    }
}

/// A client span for a call to `system`, before its statement is known:
/// named by the system until [`describe`] names it by the statement. The
/// target is `oxtel_sql`, which oxtel's sqlx mapping looks for so that it
/// doesn't record a query a second time.
pub(crate) fn open_span(
    system: &'static str,
    namespace: Option<&str>,
    server: Option<(&str, u16)>,
) -> tracing::Span {
    let span = tracing::info_span!(
        target: "oxtel_sql",
        "database call",
        otel.name = system,
        otel.kind = "client",
        db.system.name = system,
        db.namespace = Empty,
        db.query.summary = Empty,
        db.query.text = Empty,
        db.response.returned_rows = Empty,
        server.address = Empty,
        server.port = Empty,
        error.type = Empty,
        db.response.status_code = Empty,
        otel.status_code = Empty,
    );
    if span.is_disabled() {
        return span;
    }
    if let Some(namespace) = namespace {
        span.record("db.namespace", namespace);
    }
    if let Some((address, port)) = server {
        span.record("server.address", address);
        span.record("server.port", port);
    }
    span
}

/// Name a call's span by its statement, and record the statement's summary
/// and text.
pub(crate) fn describe(span: &tracing::Span, analysis: &Analysis) {
    if let Some(summary) = &analysis.summary {
        span.record("otel.name", summary.as_str());
        span.record("db.query.summary", summary.as_str());
    }
    if QUERY_TEXT.load(Ordering::Relaxed) && !analysis.text.is_empty() {
        span.record("db.query.text", analysis.text.as_str());
    }
}

/// Describe the metrics this crate records (their units and descriptions)
/// to the current `metrics` recorder. The instrumentation entry points call
/// it, so it only matters for a recorder installed after them.
pub fn describe_metrics() {
    metrics::describe_histogram!(
        "db.client.operation.duration",
        metrics::Unit::Seconds,
        "Duration of database client operations."
    );
}
