//! Spans and metrics, under the semantic conventions' names, from what
//! libraries already report through `tracing`, with no code change.
//! [`oxtel::init`](crate::init) installs these; [`Builder::without_mappings`]
//! or `OXTEL_MAPPINGS=none` leaves them out.
//!
//! The one so far is [`Sqlx`]: sqlx writes an event as each query finishes,
//! and the mapping turns it into a database client span and
//! `db.client.operation.duration`.
//!
//! # sqlx: this mapping, or oxtel-sql's wrapper
//!
//! sqlx can be instrumented two ways. This mapping needs no code at all, but
//! can only record what sqlx's event says. Wrapping the pool with
//! [oxtel-sql](https://docs.rs/oxtel-sql) records everything, but the
//! queries have to run through the wrapper. The two never count a query
//! twice: the mapping leaves alone the queries the wrapper records.
//!
//! | | This mapping | oxtel-sql's wrapper |
//! |---|---|---|
//! | Code to change | None | Run queries through the wrapped pool |
//! | Span named by the query, sanitized text, duration | Yes | Yes |
//! | Rows returned | Yes | Yes |
//! | Inside the caller's span | Yes | Yes |
//! | Which database: system, name, server | Only as configured with [`Sqlx::with_system`] and its neighbours | Yes, from the connection |
//! | Failed queries and their error codes | **No**: a failed query looks like one that returned nothing | Yes |
//! | Works when sqlx's statement logging is off | **No** | Yes |
//! | Covers queries the wrapper doesn't see | Yes | No |
//!
//! The mapping's limits come from what sqlx's event carries:
//!
//! - **No failures.** sqlx writes the same event whether a query succeeded
//!   or failed, without the error, so the span isn't marked as an error.
//! - **No database identity.** The event doesn't say which database it's
//!   about. `db.system.name` is `other_sql` unless set with
//!   [`Sqlx::with_system`]; a program with two databases can't tell their
//!   queries apart.
//! - **It needs sqlx's statement logging.** sqlx writes the event only when
//!   statement logging is on, as it is by default (at `debug`). A
//!   connection configured with `log_statements(LevelFilter::Off)` writes
//!   nothing to map. oxtel lets the events through to the trace layer
//!   alone, so turning the mapping on doesn't put them in your logs.
//! - **It reads a log line, not an API.** The event's fields can change in
//!   any sqlx release. The mapping is tested against sqlx 0.8 and 0.9, and
//!   leaves an event it doesn't recognise alone rather than record it wrong.
//!
//! [`Builder::without_mappings`]: crate::Builder::without_mappings

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Once};
use std::time::Duration;

use metrics::Label;
use oxtel_sdk::KeyValue;
use oxtel_sdk::trace::{EventMapping, MappedSpan, SpanKind, Status};
use tracing_core::field::{Field, Visit};
use tracing_core::{Event, Level, Metadata};

/// The mappings [`oxtel::init`](crate::init) installs.
pub(crate) fn built_in() -> Vec<Arc<dyn EventMapping>> {
    vec![Arc::new(Sqlx::new())]
}

/// sqlx's queries, from the `sqlx::query` event it writes as each finishes:
/// a client span named by the query summary, ending when the query did and
/// as long as sqlx measured it, with `db.query.summary`, `db.query.text`
/// (every literal replaced by `?`; see `oxtel_sql::record_query_text`) and
/// `db.response.returned_rows`; and `db.client.operation.duration`. See the
/// [module documentation](self) for what it can't record.
///
/// [`oxtel::init`](crate::init) installs one that knows nothing about the
/// database. To name it, add one with
/// [`Builder::with_mapping`](crate::Builder::with_mapping), which is tried
/// first:
///
/// ```no_run
/// let _telemetry = oxtel::builder()
///     .with_mapping(
///         oxtel::mappings::Sqlx::new()
///             .with_system("postgresql")
///             .with_namespace("shop")
///             .with_server("db.internal", 5432),
///     )
///     .install()?;
/// # Ok::<(), oxtel::Error>(())
/// ```
pub struct Sqlx {
    system: Cow<'static, str>,
    namespace: Option<String>,
    server: Option<(String, u16)>,
    described: Once,
    cache: Mutex<HashMap<String, Arc<Analysis>>>,
}

/// What a query's text says, worked out once per distinct text.
struct Analysis {
    summary: Option<String>,
    text: String,
}

/// Distinct query texts remembered; the cache starts over past this.
const CACHE_LIMIT: usize = 1024;

impl Sqlx {
    /// A mapping that knows nothing about the database: `db.system.name`
    /// is `other_sql`, the conventions' value for a SQL database they can't
    /// name.
    pub fn new() -> Self {
        Sqlx {
            system: Cow::Borrowed("other_sql"),
            namespace: None,
            server: None,
            described: Once::new(),
            cache: Mutex::default(),
        }
    }

    /// `db.system.name`: `postgresql`, `mysql`, `sqlite` or another of the
    /// conventions' values.
    pub fn with_system(mut self, system: impl Into<Cow<'static, str>>) -> Self {
        self.system = system.into();
        self
    }

    /// `db.namespace`: the database's name.
    pub fn with_namespace(mut self, namespace: impl Into<String>) -> Self {
        self.namespace = Some(namespace.into());
        self
    }

    /// `server.address` and `server.port`.
    pub fn with_server(mut self, address: impl Into<String>, port: u16) -> Self {
        self.server = Some((address.into(), port));
        self
    }

    fn analyze(&self, sql: &str) -> Arc<Analysis> {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(a) = cache.get(sql) {
            return Arc::clone(a);
        }
        let analysis = Arc::new(Analysis {
            summary: oxtel_sql::summarize(sql),
            text: oxtel_sql::sanitize(sql),
        });
        if cache.len() >= CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(sql.to_string(), Arc::clone(&analysis));
        analysis
    }

    fn record_duration(&self, analysis: &Analysis, seconds: f64) {
        let mut labels = Vec::with_capacity(5);
        labels.push(Label::new("db.system.name", self.system.clone()));
        if let Some(namespace) = &self.namespace {
            labels.push(Label::new("db.namespace", namespace.clone()));
        }
        if let Some(summary) = &analysis.summary {
            labels.push(Label::new("db.query.summary", summary.clone()));
        }
        if let Some((address, port)) = &self.server {
            labels.push(Label::new("server.address", address.clone()));
            labels.push(Label::new("server.port", port.to_string()));
        }
        metrics::histogram!("db.client.operation.duration", labels).record(seconds);
    }
}

impl Default for Sqlx {
    fn default() -> Self {
        Sqlx::new()
    }
}

impl std::fmt::Debug for Sqlx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sqlx")
            .field("system", &self.system)
            .field("namespace", &self.namespace)
            .field("server", &self.server)
            .finish_non_exhaustive()
    }
}

/// The fields of sqlx's `sqlx::query` event, as sqlx 0.8 and 0.9 write
/// them: `summary` (the first four words), `db.statement` (the whole query
/// when it's longer than that, else empty), `rows_returned`,
/// `rows_affected`, `elapsed` and `elapsed_secs`.
#[derive(Default)]
struct QueryFields {
    summary: Option<String>,
    statement: Option<String>,
    rows_returned: Option<u64>,
    elapsed_secs: Option<f64>,
}

impl QueryFields {
    /// The query's text.
    fn sql(&self) -> Option<&str> {
        let statement = self.statement.as_deref().map(str::trim);
        let summary = self.summary.as_deref().map(str::trim);
        statement
            .filter(|s| !s.is_empty())
            .or(summary.filter(|s| !s.is_empty() && !s.ends_with('…')))
    }
}

impl Visit for QueryFields {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "summary" => self.summary = Some(value.to_string()),
            "db.statement" => self.statement = Some(value.to_string()),
            _ => {}
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        if field.name() == "rows_returned" {
            self.rows_returned = Some(value);
        }
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        if field.name() == "elapsed_secs" {
            self.elapsed_secs = Some(value);
        }
    }

    fn record_debug(&mut self, _: &Field, _: &dyn std::fmt::Debug) {}
}

impl EventMapping for Sqlx {
    fn map(
        &self,
        event: &Event<'_>,
        parent: Option<&'static Metadata<'static>>,
    ) -> Option<MappedSpan> {
        if event.metadata().target() != "sqlx::query" {
            return None;
        }
        // oxtel-sql's wrapper records this query itself.
        if parent
            .is_some_and(|p| p.target() == "oxtel_sql" || p.target().starts_with("oxtel_sql::"))
        {
            return None;
        }
        let mut fields = QueryFields::default();
        event.record(&mut fields);
        let seconds = fields.elapsed_secs.filter(|s| s.is_finite() && *s >= 0.0)?;
        let analysis = self.analyze(fields.sql()?);

        // Here rather than at construction: the recorder may come later.
        self.described.call_once(oxtel_sql::describe_metrics);
        self.record_duration(&analysis, seconds);

        let mut attributes = vec![KeyValue::new("db.system.name", self.system.to_string())];
        if let Some(namespace) = &self.namespace {
            attributes.push(KeyValue::new("db.namespace", namespace.clone()));
        }
        if let Some(summary) = &analysis.summary {
            attributes.push(KeyValue::new("db.query.summary", summary.clone()));
        }
        if oxtel_sql::records_query_text() {
            attributes.push(KeyValue::new("db.query.text", analysis.text.clone()));
        }
        if let Some(rows) = fields.rows_returned {
            attributes.push(KeyValue::new(
                "db.response.returned_rows",
                i64::try_from(rows).unwrap_or(i64::MAX),
            ));
        }
        if let Some((address, port)) = &self.server {
            attributes.push(KeyValue::new("server.address", address.clone()));
            attributes.push(KeyValue::new("server.port", i64::from(*port)));
        }
        Some(MappedSpan {
            name: Cow::Owned(
                analysis
                    .summary
                    .clone()
                    .unwrap_or_else(|| self.system.to_string()),
            ),
            kind: SpanKind::Client,
            duration: Duration::from_secs_f64(seconds),
            attributes,
            status: Status::Unset,
            scope: Cow::Borrowed("sqlx"),
        })
    }

    fn targets(&self) -> &'static [(&'static str, Level)] {
        // sqlx's default level for statements. One configured at `trace`
        // needs `sqlx::query=trace` in the traces filter.
        &[("sqlx::query", Level::DEBUG)]
    }
}
