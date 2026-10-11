//! Recording one database call: a client span, and the
//! `db.client.operation.duration` histogram.

// The clients are features; without one, only the public pieces are used.
#![cfg_attr(not(feature = "rusqlite"), allow(dead_code))]

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

/// What a statement's text says, worked out once per distinct text.
pub(crate) struct Analysis {
    pub(crate) summary: Option<String>,
    pub(crate) text: String,
}

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
    pub(crate) fn span(&self) -> tracing::Span {
        let name = self.analysis.summary.as_deref().unwrap_or(self.system);
        let span = tracing::info_span!(
            "database call",
            otel.name = name,
            otel.kind = "client",
            db.system.name = self.system,
            db.namespace = Empty,
            db.query.summary = Empty,
            db.query.text = Empty,
            server.address = Empty,
            server.port = Empty,
            error.type = Empty,
            db.response.status_code = Empty,
            otel.status_code = Empty,
        );
        if span.is_disabled() {
            return span;
        }
        if let Some(namespace) = self.namespace {
            span.record("db.namespace", namespace);
        }
        if let Some(summary) = &self.analysis.summary {
            span.record("db.query.summary", summary.as_str());
        }
        if QUERY_TEXT.load(Ordering::Relaxed) {
            span.record("db.query.text", self.analysis.text.as_str());
        }
        if let Some((address, port)) = self.server {
            span.record("server.address", address);
            span.record("server.port", port);
        }
        span
    }

    /// Record `db.client.operation.duration`.
    pub(crate) fn record_duration(&self, seconds: f64) {
        let mut labels = Vec::with_capacity(6);
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
        metrics::histogram!("db.client.operation.duration", labels).record(seconds);
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
