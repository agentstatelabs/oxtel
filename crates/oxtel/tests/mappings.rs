//! The sqlx mapping, fed events shaped exactly as sqlx 0.8 and 0.9 write
//! them (see sqlx-core's `logger.rs`). `tests/sqlx.rs` checks against sqlx
//! itself.

#![cfg(feature = "mappings")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use oxtel::sdk::metrics::{DB_SECONDS_BUCKETS, PointValue};
use oxtel::sdk::trace::{InMemorySpanExporter, SpanData, SpanKind, Status};
use oxtel::{Value, mappings};

/// A writer the test can read back.
#[derive(Clone, Default)]
struct Buffer(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Buffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct Run {
    spans: Vec<SpanData>,
    local: String,
    metrics: Vec<oxtel::sdk::metrics::MetricFamily>,
}

fn run(builder: oxtel::Builder, work: impl FnOnce()) -> Run {
    let spans = InMemorySpanExporter::default();
    let local = Buffer::default();
    let telemetry = builder
        .without_env_exporters()
        .with_pull_metrics()
        .with_traces_filter("info")
        .with_logs_filter("info")
        .with_local_logs(oxtel::sdk::logs::LocalFormat::Json)
        .with_local_logs_writer(local.clone())
        .with_traces({
            let spans = spans.clone();
            move |b| b.with_simple_exporter(spans)
        })
        .build()
        .unwrap();
    let recorder = telemetry.recorder().expect("a metrics pipeline");
    tracing::dispatcher::with_default(telemetry.dispatch(), || {
        metrics::with_local_recorder(recorder, work);
    });
    let guard = telemetry.into_guard();
    let metrics = guard.meter_provider().unwrap().collect();
    Run {
        spans: spans.finished_spans(),
        local: String::from_utf8(local.0.lock().unwrap().clone()).unwrap(),
        metrics,
    }
}

/// sqlx's event for a query longer than four words.
fn long_query(sql: &str, rows_returned: u64, elapsed_secs: f64) {
    let words: Vec<&str> = sql.split_whitespace().take(4).collect();
    let summary = format!("{} …", words.join(" "));
    let statement = format!("\n\n{sql}\n");
    tracing::debug!(
        target: "sqlx::query",
        summary,
        db.statement = statement,
        rows_affected = 0u64,
        rows_returned,
        elapsed = ?Duration::from_secs_f64(elapsed_secs),
        elapsed_secs,
    );
}

fn attr(span: &SpanData, key: &str) -> Option<Value> {
    span.attributes
        .iter()
        .find(|kv| kv.key == key)
        .map(|kv| kv.value.clone())
}

#[test]
fn a_query_event_becomes_a_database_span_and_its_duration() {
    let run = run(oxtel::builder(), || {
        tracing::info_span!("request").in_scope(|| {
            long_query(
                "SELECT name FROM users WHERE id = $1 AND name <> 'secret-name'",
                1,
                0.012,
            );
        });
    });
    let request = run.spans.iter().find(|s| s.name == "request").unwrap();
    let query = run
        .spans
        .iter()
        .find(|s| s.name == "SELECT users")
        .expect("a span named by the query summary");
    assert_eq!(query.parent_span_id, Some(request.context.span_id));
    assert_eq!(query.kind, SpanKind::Client);
    assert_eq!(query.scope.name, "sqlx");
    assert_eq!(query.status, Status::Unset);
    assert_eq!(
        query.end_time.duration_since(query.start_time).unwrap(),
        Duration::from_millis(12)
    );
    assert_eq!(
        attr(query, "db.system.name"),
        Some(Value::from("other_sql"))
    );
    assert_eq!(
        attr(query, "db.query.summary"),
        Some(Value::from("SELECT users"))
    );
    assert_eq!(
        attr(query, "db.query.text"),
        Some(Value::from(
            "SELECT name FROM users WHERE id = $1 AND name <> ?"
        ))
    );
    assert_eq!(
        attr(query, "db.response.returned_rows"),
        Some(Value::I64(1))
    );
    assert!(
        request.events.is_empty(),
        "the event isn't kept as a span event too"
    );

    let family = run
        .metrics
        .iter()
        .find(|f| f.name == "db.client.operation.duration")
        .expect("the duration recorded");
    assert_eq!(family.unit.as_deref(), Some("s"));
    let point = &family.points[0];
    assert!(
        point
            .attributes
            .contains(&("db.system.name".into(), "other_sql".into()))
    );
    assert!(
        point
            .attributes
            .contains(&("db.query.summary".into(), "SELECT users".into()))
    );
    let PointValue::Histogram(histogram) = &point.value else {
        panic!("not a histogram")
    };
    assert_eq!(&*histogram.bounds, DB_SECONDS_BUCKETS.as_slice());

    // sqlx's events reach the trace layer, not the local logs at `info`.
    assert!(!run.local.contains("sqlx"), "{}", run.local);
}

#[test]
fn a_short_query_comes_whole_in_the_summary() {
    let run = run(oxtel::builder(), || {
        tracing::debug!(
            target: "sqlx::query",
            summary = "SELECT 1",
            db.statement = "",
            rows_affected = 0u64,
            rows_returned = 1u64,
            elapsed = ?Duration::from_millis(1),
            elapsed_secs = 0.001,
        );
    });
    let query = run.spans.iter().find(|s| s.name == "SELECT").unwrap();
    assert_eq!(attr(query, "db.query.text"), Some(Value::from("SELECT ?")));
}

#[test]
fn the_database_can_be_named() {
    let builder = oxtel::builder().with_mapping(
        mappings::Sqlx::new()
            .with_system("postgresql")
            .with_namespace("shop")
            .with_server("db.internal", 5432),
    );
    let run = run(builder, || {
        long_query("INSERT INTO orders (id) VALUES ($1)", 0, 0.002)
    });
    let query = run
        .spans
        .iter()
        .find(|s| s.name == "INSERT orders")
        .unwrap();
    assert_eq!(
        attr(query, "db.system.name"),
        Some(Value::from("postgresql"))
    );
    assert_eq!(attr(query, "db.namespace"), Some(Value::from("shop")));
    assert_eq!(
        attr(query, "server.address"),
        Some(Value::from("db.internal"))
    );
    assert_eq!(attr(query, "server.port"), Some(Value::I64(5432)));
    assert_eq!(
        run.spans.len(),
        1,
        "the configured mapping, not the built-in too"
    );
}

#[test]
fn the_mappings_can_be_left_out() {
    let run = run(oxtel::builder().without_mappings(), || {
        long_query("SELECT name FROM users WHERE id = $1", 1, 0.003);
    });
    assert!(run.spans.is_empty());
    assert!(
        run.metrics
            .iter()
            .all(|f| f.name != "db.client.operation.duration")
    );
}

#[test]
fn a_query_oxtel_sql_already_records_isnt_counted_twice() {
    let run = run(oxtel::builder(), || {
        // What oxtel-sql's wrapper makes around a query.
        tracing::info_span!(target: "oxtel_sql", "database call", otel.name = "SELECT users")
            .in_scope(|| long_query("SELECT name FROM users WHERE id = $1", 1, 0.003));
    });
    let names: Vec<&str> = run.spans.iter().map(|s| s.name.as_ref()).collect();
    assert_eq!(names, ["SELECT users"], "only the wrapper's span");
}

#[test]
fn an_event_of_another_shape_is_left_alone() {
    let run = run(oxtel::builder(), || {
        tracing::info_span!("request").in_scope(|| {
            // No elapsed_secs: not the shape the mapping knows.
            tracing::warn!(target: "sqlx::query", summary = "SELECT 1", "something else");
        });
    });
    assert_eq!(run.spans.len(), 1, "no database span");
    assert!(
        run.local.contains("something else"),
        "still logged: {}",
        run.local
    );
}
