//! A real SQLite connection, instrumented, recording into memory.

#![cfg(feature = "rusqlite")]

use oxtel_sdk::Resource;
use oxtel_sdk::metrics::{DB_SECONDS_BUCKETS, MeterProvider, MetricFamily, PointValue};
use oxtel_sdk::trace::{InMemorySpanExporter, Sampler, SpanData, SpanKind, TracerProvider};
use oxtel_sdk::{KeyValue, Value};
use oxtel_semconv::{Instrument, attribute, metric};
use rusqlite::Connection;
use tracing_subscriber::layer::SubscriberExt;

struct Harness {
    spans: InMemorySpanExporter,
    meters: MeterProvider,
    _guards: (
        tracing::subscriber::DefaultGuard,
        metrics::LocalRecorderGuard<'static>,
    ),
    _traces: TracerProvider,
}

/// Pipelines recording into memory, installed for this thread. SQLite calls
/// the hooks on the thread running the statement, so this sees them.
fn harness() -> Harness {
    let spans = InMemorySpanExporter::default();
    let traces = TracerProvider::builder()
        .with_resource(Resource::builder().without_env().build())
        .with_sampler(Sampler::AlwaysOn)
        .with_span_events(false)
        .with_simple_exporter(spans.clone())
        .build();
    let meters = MeterProvider::builder().build();
    let recorder: &'static _ = Box::leak(Box::new(meters.recorder()));
    Harness {
        spans,
        _guards: (
            tracing::subscriber::set_default(tracing_subscriber::registry().with(traces.layer())),
            metrics::set_default_local_recorder(recorder),
        ),
        meters,
        _traces: traces,
    }
}

fn attr<'a>(span: &'a SpanData, key: &str) -> Option<&'a Value> {
    span.attributes
        .iter()
        .find(|kv: &&KeyValue| kv.key == key)
        .map(|kv| &kv.value)
}

fn text(span: &SpanData, key: &str) -> Option<String> {
    match attr(span, key)? {
        Value::String(s) => Some(s.to_string()),
        other => Some(format!("{other:?}")),
    }
}

fn durations(meters: &MeterProvider) -> MetricFamily {
    meters
        .collect()
        .into_iter()
        .find(|f| f.name == "db.client.operation.duration")
        .expect("db.client.operation.duration recorded")
}

#[test]
fn every_statement_gets_a_span_named_by_its_summary() {
    let h = harness();
    let conn = Connection::open_in_memory().unwrap();
    oxtel_sql::rusqlite::instrument(&conn);

    conn.execute_batch("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    conn.execute("INSERT INTO users (name) VALUES (?1)", ["Ann"])
        .unwrap();
    // A literal in the text itself: it must not reach telemetry.
    conn.execute_batch("INSERT INTO users (name) VALUES ('secret-name')")
        .unwrap();
    let name: String = tracing::info_span!("request").in_scope(|| {
        conn.query_row("SELECT name FROM users WHERE id = ?1", [1], |row| {
            row.get(0)
        })
        .unwrap()
    });
    assert_eq!(name, "Ann");

    let spans = h.spans.finished_spans();
    let names: Vec<&str> = spans.iter().map(|s| s.name.as_ref()).collect();
    for expected in [
        "CREATE TABLE users",
        "INSERT users",
        "SELECT users",
        "request",
    ] {
        assert!(names.contains(&expected), "{expected} in {names:?}");
    }

    let select = spans.iter().find(|s| s.name == "SELECT users").unwrap();
    assert_eq!(select.kind, SpanKind::Client);
    assert_eq!(text(select, "db.system.name").as_deref(), Some("sqlite"));
    assert_eq!(
        text(select, "db.query.summary").as_deref(),
        Some("SELECT users")
    );
    assert_eq!(
        text(select, "db.query.text").as_deref(),
        Some("SELECT name FROM users WHERE id = ?1")
    );
    let request = spans.iter().find(|s| s.name == "request").unwrap();
    assert_eq!(
        select.parent_span_id,
        Some(request.context.span_id),
        "a statement inside a span is its child"
    );

    for span in &spans {
        for kv in &span.attributes {
            if let Value::String(s) = &kv.value {
                assert!(
                    !s.contains("secret-name"),
                    "{} carries a literal: {s}",
                    span.name
                );
            }
        }
    }
    let inserts: Vec<String> = spans
        .iter()
        .filter(|s| s.name == "INSERT users")
        .filter_map(|s| text(s, "db.query.text"))
        .collect();
    assert_eq!(
        inserts,
        [
            "INSERT INTO users (name) VALUES (?1)",
            "INSERT INTO users (name) VALUES (?)"
        ]
    );

    let family = durations(&h.meters);
    assert_eq!(family.unit.as_deref(), Some("s"));
    let point = family
        .points
        .iter()
        .find(|p| {
            p.attributes
                .contains(&("db.query.summary".into(), "INSERT users".into()))
        })
        .expect("a series for INSERT users");
    assert!(
        point
            .attributes
            .contains(&("db.system.name".into(), "sqlite".into()))
    );
    let PointValue::Histogram(histogram) = &point.value else {
        panic!("not a histogram")
    };
    assert_eq!(histogram.count(), 2, "two inserts");
    assert_eq!(&*histogram.bounds, DB_SECONDS_BUCKETS.as_slice());
}

#[test]
fn rows_read_one_by_one_and_nested_statements_close_in_order() {
    let h = harness();
    let conn = Connection::open_in_memory().unwrap();
    oxtel_sql::rusqlite::instrument(&conn);
    conn.execute_batch(
        "CREATE TABLE a (x INTEGER); INSERT INTO a VALUES (1), (2), (3);
         CREATE TABLE b (y INTEGER);",
    )
    .unwrap();

    // While reading `a` row by row, write to `b`.
    let mut read = conn.prepare("SELECT x FROM a").unwrap();
    let mut rows = read.query([]).unwrap();
    while let Some(row) = rows.next().unwrap() {
        let x: i64 = row.get(0).unwrap();
        conn.execute("INSERT INTO b (y) VALUES (?1)", [x]).unwrap();
    }
    drop(rows);
    drop(read);

    let spans = h.spans.finished_spans();
    let count = |name: &str| spans.iter().filter(|s| s.name == name).count();
    assert_eq!(count("SELECT a"), 1);
    assert_eq!(count("INSERT b"), 3);
    let select = spans.iter().find(|s| s.name == "SELECT a").unwrap();
    let last_insert = spans.iter().rev().find(|s| s.name == "INSERT b").unwrap();
    assert!(
        select.end_time >= last_insert.end_time,
        "the read stays open while the writes happen"
    );
}

/// `otel.*` fields steer the span itself; they aren't attributes.
fn is_instruction(key: &str) -> bool {
    key.starts_with("otel.")
}

#[test]
fn spans_and_metrics_use_only_defined_names() {
    let h = harness();
    let conn = Connection::open_in_memory().unwrap();
    oxtel_sql::rusqlite::instrument(&conn);
    conn.execute_batch("CREATE TABLE t (v TEXT); INSERT INTO t VALUES ('x');")
        .unwrap();
    let _: i64 = conn
        .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
        .unwrap();

    let spans = h.spans.finished_spans();
    assert!(spans.len() >= 3);
    for span in &spans {
        for kv in &span.attributes {
            assert!(
                is_instruction(&kv.key) || attribute::is_defined(&kv.key),
                "span {:?} has attribute {:?}, which the conventions don't define",
                span.name,
                kv.key
            );
        }
    }
    let family = durations(&h.meters);
    let definition = metric::by_name(&family.name).expect("a defined metric");
    assert_eq!(definition.instrument, Instrument::Histogram);
    assert_eq!(family.unit.as_deref(), Some(definition.unit));
    for point in &family.points {
        for (key, _) in &point.attributes {
            assert!(attribute::is_defined(key), "label {key:?} isn't defined");
        }
    }
}
