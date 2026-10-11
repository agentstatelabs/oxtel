//! A real PostgreSQL, through instrumented tokio-postgres clients, recording
//! into memory. Set `TEST_DATABASE_URL` (such as
//! `postgres://postgres@localhost:5432/postgres`) to run these, as CI does;
//! without it they check nothing, and say so. Each test has its own
//! connection and temporary tables, so they can share one database.

#![cfg(feature = "postgres")]

use oxtel_sdk::Resource;
use oxtel_sdk::metrics::{DB_SECONDS_BUCKETS, MeterProvider, MetricFamily, PointValue};
use oxtel_sdk::trace::{InMemorySpanExporter, Sampler, SpanData, SpanKind, Status, TracerProvider};
use oxtel_sdk::{KeyValue, Value};
use oxtel_semconv::{Instrument, attribute, metric};
use oxtel_sql::postgres::{Client, GenericClient};
use tracing::Instrument as _;
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

/// Pipelines recording into memory, installed for this thread. The tests run
/// on a single-threaded runtime, so every call is made on it.
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

fn database_url() -> Option<String> {
    match std::env::var("TEST_DATABASE_URL") {
        Ok(url) if !url.is_empty() => Some(url),
        // CI has a database; passing there without one would hide a broken setup.
        _ if std::env::var_os("CI").is_some() => panic!("TEST_DATABASE_URL isn't set in CI"),
        _ => {
            eprintln!("TEST_DATABASE_URL isn't set: not checking against PostgreSQL");
            None
        }
    }
}

async fn connect(url: &str) -> (Client, tokio_postgres::Config) {
    let config: tokio_postgres::Config = url.parse().expect("TEST_DATABASE_URL parses");
    let (client, connection) = config
        .connect(tokio_postgres::NoTls)
        .await
        .expect("connect to TEST_DATABASE_URL");
    tokio::spawn(connection);
    (Client::new(client, &config), config)
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
        Value::I64(n) => Some(n.to_string()),
        other => Some(format!("{other:?}")),
    }
}

fn named<'a>(spans: &'a [SpanData], name: &str) -> Vec<&'a SpanData> {
    spans.iter().filter(|s| s.name == name).collect()
}

fn durations(meters: &MeterProvider) -> MetricFamily {
    meters
        .collect()
        .into_iter()
        .find(|f| f.name == "db.client.operation.duration")
        .expect("db.client.operation.duration recorded")
}

#[tokio::test]
async fn calls_are_spans_named_by_summary_with_the_connection() {
    let Some(url) = database_url() else { return };
    let h = harness();
    let (client, config) = connect(&url).await;

    client
        .batch_execute("CREATE TEMP TABLE users (id SERIAL PRIMARY KEY, name TEXT)")
        .await
        .unwrap();
    client
        .execute("INSERT INTO users (name) VALUES ($1)", &[&"Ann"])
        .await
        .unwrap();
    // A literal in the text itself: it must not reach telemetry.
    client
        .batch_execute("INSERT INTO users (name) VALUES ('secret-name')")
        .await
        .unwrap();
    let request = tracing::info_span!("request");
    let row = client
        .query_one("SELECT name FROM users WHERE id = $1", &[&1i32])
        .instrument(request)
        .await
        .unwrap();
    assert_eq!(row.get::<_, String>(0), "Ann");

    let spans = h.spans.finished_spans();
    let selects = named(&spans, "SELECT users");
    assert_eq!(
        selects.len(),
        1,
        "one SELECT in {:?}",
        spans.iter().map(|s| &s.name).collect::<Vec<_>>()
    );
    let select = selects[0];
    let request = named(&spans, "request")[0];
    assert_eq!(
        select.parent_span_id,
        Some(request.context.span_id),
        "inside the caller's span"
    );
    assert_eq!(select.kind, SpanKind::Client);
    assert_eq!(
        text(select, "db.system.name").as_deref(),
        Some("postgresql")
    );
    assert_eq!(
        text(select, "db.query.summary").as_deref(),
        Some("SELECT users")
    );
    assert_eq!(
        text(select, "db.query.text").as_deref(),
        Some("SELECT name FROM users WHERE id = $1")
    );
    assert_eq!(text(select, "db.namespace").as_deref(), config.get_dbname());
    if let [tokio_postgres::config::Host::Tcp(host)] = config.get_hosts() {
        assert_eq!(
            text(select, "server.address").as_deref(),
            Some(host.as_str())
        );
        let port = config.get_ports().first().copied().unwrap_or(5432);
        assert_eq!(text(select, "server.port"), Some(port.to_string()));
    }
    assert_eq!(select.status, Status::Unset);

    for span in &spans {
        for kv in &span.attributes {
            assert!(
                !format!("{:?}", kv.value).contains("secret-name"),
                "a literal reached {:?} on {:?}",
                kv.key,
                span.name
            );
        }
    }
    let inserts: Vec<String> = named(&spans, "INSERT users")
        .iter()
        .filter_map(|s| text(s, "db.query.text"))
        .collect();
    assert_eq!(
        inserts,
        [
            "INSERT INTO users (name) VALUES ($1)",
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
            .contains(&("db.system.name".into(), "postgresql".into()))
    );
    let PointValue::Histogram(histogram) = &point.value else {
        panic!("not a histogram")
    };
    assert_eq!(histogram.count(), 2, "two inserts");
    assert_eq!(&*histogram.bounds, DB_SECONDS_BUCKETS.as_slice());
}

#[tokio::test]
async fn a_failed_call_records_its_sqlstate() {
    let Some(url) = database_url() else { return };
    let h = harness();
    let (client, _) = connect(&url).await;
    client
        .batch_execute("CREATE TEMP TABLE keys (id INT PRIMARY KEY)")
        .await
        .unwrap();
    client
        .execute("INSERT INTO keys (id) VALUES ($1)", &[&1i32])
        .await
        .unwrap();
    let error = client
        .execute("INSERT INTO keys (id) VALUES ($1)", &[&1i32])
        .await
        .expect_err("a duplicate key");
    assert_eq!(error.code().map(|c| c.code()), Some("23505"));

    let spans = h.spans.finished_spans();
    let inserts = named(&spans, "INSERT keys");
    assert_eq!(inserts.len(), 2);
    let (ok, failed) = (inserts[0], inserts[1]);
    assert_eq!(text(ok, "error.type"), None);
    assert_eq!(ok.status, Status::Unset);
    assert_eq!(text(failed, "error.type").as_deref(), Some("23505"));
    assert_eq!(
        text(failed, "db.response.status_code").as_deref(),
        Some("23505")
    );
    assert!(
        matches!(failed.status, Status::Error { .. }),
        "{:?}",
        failed.status
    );

    let family = durations(&h.meters);
    let failures: Vec<_> = family
        .points
        .iter()
        .filter(|p| {
            p.attributes
                .contains(&("error.type".into(), "23505".into()))
        })
        .collect();
    assert_eq!(failures.len(), 1, "one series for the failure");
    assert!(
        failures[0]
            .attributes
            .contains(&("db.response.status_code".into(), "23505".into()))
    );
}

#[tokio::test]
async fn prepared_statements_keep_their_names() {
    let Some(url) = database_url() else { return };
    let h = harness();
    let (client, _) = connect(&url).await;
    client
        .batch_execute("CREATE TEMP TABLE items (id INT, price INT)")
        .await
        .unwrap();

    let insert = client
        .prepare("INSERT INTO items (id, price) VALUES ($1, $2)")
        .await
        .unwrap();
    for id in 0..3i32 {
        client.execute(&insert, &[&id, &(id * 10)]).await.unwrap();
    }
    // One prepared outside the wrapper: it runs, but has no name to give.
    let raw = client
        .get_ref()
        .prepare("SELECT sum(price) FROM items")
        .await
        .unwrap();
    let total: i64 = client.query_one(&raw, &[]).await.unwrap().get(0);
    assert_eq!(total, 30);

    let spans = h.spans.finished_spans();
    assert_eq!(
        named(&spans, "INSERT items").len(),
        3,
        "preparing isn't a call"
    );
    let unnamed = named(&spans, "postgresql");
    assert_eq!(unnamed.len(), 1);
    assert_eq!(text(unnamed[0], "db.query.text"), None);
    assert_eq!(text(unnamed[0], "db.query.summary"), None);
}

/// Works with a client or a transaction.
async fn count(client: &impl GenericClient) -> i64 {
    client
        .query_one("SELECT count(*) FROM events", &[])
        .await
        .unwrap()
        .get(0)
}

#[tokio::test]
async fn transactions_record_begin_commit_rollback_and_savepoints() {
    let Some(url) = database_url() else { return };
    let h = harness();
    let (mut client, _) = connect(&url).await;
    client
        .batch_execute("CREATE TEMP TABLE events (id INT)")
        .await
        .unwrap();

    let mut tx = client.transaction().await.unwrap();
    tx.execute("INSERT INTO events (id) VALUES ($1)", &[&1i32])
        .await
        .unwrap();
    let nested = tx.transaction().await.unwrap();
    nested
        .execute("INSERT INTO events (id) VALUES ($1)", &[&2i32])
        .await
        .unwrap();
    nested.rollback().await.unwrap();
    assert_eq!(count(&tx).await, 1);
    tx.commit().await.unwrap();
    assert_eq!(count(&client).await, 1);

    let spans = h.spans.finished_spans();
    let names: Vec<&str> = spans
        .iter()
        .map(|s| s.name.as_ref())
        .filter(|n| !n.starts_with("CREATE"))
        .collect();
    assert_eq!(
        names,
        [
            "BEGIN",
            "INSERT events",
            "SAVEPOINT",
            "INSERT events",
            "ROLLBACK",
            "SELECT events",
            "COMMIT",
            "SELECT events"
        ]
    );
}

#[cfg(feature = "deadpool-postgres")]
#[tokio::test]
async fn clients_from_a_pool_are_instrumented() {
    let Some(url) = database_url() else { return };
    let h = harness();
    let config: tokio_postgres::Config = url.parse().unwrap();
    let manager = deadpool_postgres::Manager::new(config.clone(), tokio_postgres::NoTls);
    let pool = deadpool_postgres::Pool::builder(manager)
        .max_size(2)
        .build()
        .unwrap();
    let pool = oxtel_sql::postgres::Pool::new(pool, &config);

    let client = pool.get().await.unwrap();
    let one = client
        .prepare_cached("SELECT $1::INT AS one")
        .await
        .unwrap();
    let row = client.query_one(&one, &[&1i32]).await.unwrap();
    assert_eq!(row.get::<_, i32>(0), 1);
    assert_eq!(pool.status().size, 1, "the pool still works as deadpool's");

    let spans = h.spans.finished_spans();
    let selects = named(&spans, "SELECT");
    assert_eq!(selects.len(), 1);
    assert_eq!(
        text(selects[0], "db.namespace").as_deref(),
        config.get_dbname()
    );
}

#[cfg(feature = "deadpool-postgres")]
fn pool_metric(meters: &MeterProvider, name: &str, labels: &[(&str, &str)]) -> PointValue {
    let family = meters
        .collect()
        .into_iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("{name} recorded"));
    family
        .points
        .into_iter()
        .find(|p| {
            labels
                .iter()
                .all(|(k, v)| p.attributes.contains(&((*k).into(), (*v).into())))
        })
        .unwrap_or_else(|| panic!("{name} has a series for {labels:?}"))
        .value
}

#[cfg(feature = "deadpool-postgres")]
#[tokio::test]
async fn the_pool_records_its_connections() {
    use std::time::Duration;

    let Some(url) = database_url() else { return };
    let h = harness();
    let config: tokio_postgres::Config = url.parse().unwrap();
    let pool = |size, wait| {
        let manager = deadpool_postgres::Manager::new(config.clone(), tokio_postgres::NoTls);
        deadpool_postgres::Pool::builder(manager)
            .max_size(size)
            .wait_timeout(wait)
            .runtime(deadpool_postgres::Runtime::Tokio1)
            .build()
            .unwrap()
    };
    let named = oxtel_sql::postgres::Pool::new(pool(2, None), &config).with_name("orders");

    let first = named.get().await.unwrap();
    let second = named.get().await.unwrap();
    let pool_name = ("db.client.connection.pool.name", "orders");
    let count = |state| {
        pool_metric(
            &h.meters,
            "db.client.connection.count",
            &[pool_name, ("db.client.connection.state", state)],
        )
    };
    assert_eq!(count("used"), PointValue::Gauge(2.0));
    assert_eq!(count("idle"), PointValue::Gauge(0.0));
    drop(first);
    assert_eq!(count("used"), PointValue::Gauge(1.0), "one returned");
    assert_eq!(count("idle"), PointValue::Gauge(1.0));
    drop(second);
    assert_eq!(
        pool_metric(&h.meters, "db.client.connection.max", &[pool_name]),
        PointValue::Gauge(2.0)
    );
    assert_eq!(
        pool_metric(
            &h.meters,
            "db.client.connection.pending_requests",
            &[pool_name]
        ),
        PointValue::Gauge(0.0)
    );
    let histogram_count = |name| match pool_metric(&h.meters, name, &[pool_name]) {
        PointValue::Histogram(h) => h.count(),
        other => panic!("{name} isn't a histogram: {other:?}"),
    };
    assert_eq!(histogram_count("db.client.connection.wait_time"), 2);
    assert_eq!(histogram_count("db.client.connection.use_time"), 2);

    // A pool of one, already in use: the next caller times out.
    let tight = oxtel_sql::postgres::Pool::new(pool(1, Some(Duration::from_millis(50))), &config)
        .with_name("tight");
    let held = tight.get().await.unwrap();
    assert!(matches!(
        tight.get().await,
        Err(deadpool_postgres::PoolError::Timeout(_))
    ));
    drop(held);
    let tight_name = ("db.client.connection.pool.name", "tight");
    assert_eq!(
        pool_metric(&h.meters, "db.client.connection.timeouts", &[tight_name]),
        PointValue::Counter(1)
    );
    assert_eq!(
        pool_metric(
            &h.meters,
            "db.client.connection.pending_requests",
            &[tight_name]
        ),
        PointValue::Gauge(0.0),
        "the caller that timed out isn't waiting any more"
    );

    // Every name and label is one the conventions define; the durations
    // are in seconds. (oxtel records up-down counters as gauges.)
    for family in h.meters.collect() {
        if !family.name.starts_with("db.client.connection.") {
            continue;
        }
        let definition = metric::by_name(&family.name)
            .unwrap_or_else(|| panic!("{} isn't defined", family.name));
        if definition.instrument == Instrument::Histogram {
            assert_eq!(
                family.unit.as_deref(),
                Some(definition.unit),
                "{}",
                family.name
            );
        }
        for point in &family.points {
            for (key, _) in &point.attributes {
                assert!(attribute::is_defined(key), "label {key:?} isn't defined");
            }
        }
    }
}

/// `otel.*` fields steer the span itself; they aren't attributes.
fn is_instruction(key: &str) -> bool {
    key.starts_with("otel.")
}

#[tokio::test]
async fn spans_and_metrics_use_only_defined_names() {
    let Some(url) = database_url() else { return };
    let h = harness();
    let (client, _) = connect(&url).await;
    client
        .batch_execute("CREATE TEMP TABLE t (v INT PRIMARY KEY)")
        .await
        .unwrap();
    client
        .execute("INSERT INTO t (v) VALUES ($1)", &[&1i32])
        .await
        .unwrap();
    // And a failure, so its attributes are checked too.
    let _ = client
        .execute("INSERT INTO t (v) VALUES ($1)", &[&1i32])
        .await;

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
