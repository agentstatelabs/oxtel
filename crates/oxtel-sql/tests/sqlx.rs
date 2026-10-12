//! sqlx 0.8 and 0.9 through the wrapped pool, on a real PostgreSQL,
//! recording into memory. Set `TEST_DATABASE_URL` to run these, as CI does;
//! without it they check nothing, and say so.

#![cfg(any(feature = "sqlx-0.8", feature = "sqlx-0.9"))]

use oxtel_sdk::Resource;
use oxtel_sdk::metrics::{MeterProvider, PointValue};
use oxtel_sdk::trace::{InMemorySpanExporter, Sampler, SpanData, SpanKind, Status, TracerProvider};
use oxtel_sdk::{KeyValue, Value};
use oxtel_semconv::{Instrument, attribute, metric};
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

/// Pipelines recording into memory, for this thread; the tests run on a
/// single-threaded runtime.
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
            eprintln!("TEST_DATABASE_URL isn't set: not checking against sqlx");
            None
        }
    }
}

fn attr(span: &SpanData, key: &str) -> Option<Value> {
    span.attributes
        .iter()
        .find(|kv: &&KeyValue| kv.key == key)
        .map(|kv| kv.value.clone())
}

fn named<'a>(spans: &'a [SpanData], name: &str) -> Vec<&'a SpanData> {
    spans.iter().filter(|s| s.name == name).collect()
}

fn point(meters: &MeterProvider, name: &str, labels: &[(&str, &str)]) -> Option<PointValue> {
    meters
        .collect()
        .into_iter()
        .find(|f| f.name == name)?
        .points
        .into_iter()
        .find(|p| {
            labels
                .iter()
                .all(|(k, v)| p.attributes.contains(&((*k).into(), (*v).into())))
        })
        .map(|p| p.value)
}

/// `otel.*` fields steer the span itself; they aren't attributes.
fn check_names(h: &Harness) {
    for span in h.spans.finished_spans() {
        for kv in &span.attributes {
            assert!(
                kv.key.starts_with("otel.") || attribute::is_defined(&kv.key),
                "span {:?} has attribute {:?}, which the conventions don't define",
                span.name,
                kv.key
            );
        }
    }
    for family in h.meters.collect() {
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
        for p in &family.points {
            for (key, _) in &p.attributes {
                assert!(attribute::is_defined(key), "label {key:?} isn't defined");
            }
        }
    }
}

macro_rules! against {
    ($module:ident, $feature:literal, $sqlx:ident, $wrapper:ident) => {
        #[cfg(feature = $feature)]
        mod $module {
            use super::*;
            use ::$sqlx as sqlx;
            use oxtel_sql::sqlx::$wrapper::Pool;
            use tracing::Instrument as _;

            /// One connection, so temporary tables stay in view.
            async fn pool(url: &str) -> Pool<sqlx::Postgres> {
                let pool = sqlx::postgres::PgPoolOptions::new()
                    .max_connections(1)
                    .connect(url)
                    .await
                    .expect("connect to TEST_DATABASE_URL");
                Pool::new(pool)
            }

            #[tokio::test]
            async fn queries_on_the_pool_are_database_calls() {
                let Some(url) = database_url() else { return };
                let h = harness();
                let options: sqlx::postgres::PgConnectOptions = url.parse().unwrap();
                let pool = pool(&url).await;

                sqlx::query("CREATE TEMP TABLE users (id INT PRIMARY KEY, name TEXT)")
                    .execute(&pool)
                    .await
                    .unwrap();
                sqlx::query("INSERT INTO users (id, name) VALUES (1, 'secret-name')")
                    .execute(&pool)
                    .await
                    .unwrap();
                let request = tracing::info_span!("request");
                let rows = sqlx::query("SELECT name FROM users WHERE id = $1")
                    .bind(1i32)
                    .fetch_all(&pool)
                    .instrument(request)
                    .await
                    .unwrap();
                assert_eq!(rows.len(), 1);
                let one = sqlx::query("SELECT id FROM users WHERE id = $1")
                    .bind(1i32)
                    .fetch_optional(&pool)
                    .await
                    .unwrap();
                assert!(one.is_some());

                let spans = h.spans.finished_spans();
                let select = named(&spans, "SELECT users");
                assert_eq!(
                    select.len(),
                    2,
                    "{:?}",
                    spans.iter().map(|s| &s.name).collect::<Vec<_>>()
                );
                let (all, optional) = (select[0], select[1]);
                let request = named(&spans, "request")[0];
                assert_eq!(all.parent_span_id, Some(request.context.span_id));
                assert_eq!(all.kind, SpanKind::Client);
                assert_eq!(all.status, Status::Unset);
                assert_eq!(attr(all, "db.system.name"), Some(Value::from("postgresql")));
                assert_eq!(
                    attr(all, "db.namespace"),
                    Some(Value::from(
                        options.get_database().unwrap_or("postgres").to_string()
                    ))
                );
                assert_eq!(
                    attr(all, "server.address"),
                    Some(Value::from(options.get_host().to_string()))
                );
                assert_eq!(
                    attr(all, "server.port"),
                    Some(Value::I64(i64::from(options.get_port())))
                );
                assert_eq!(
                    attr(all, "db.query.text"),
                    Some(Value::from("SELECT name FROM users WHERE id = $1"))
                );
                assert_eq!(attr(all, "db.response.returned_rows"), Some(Value::I64(1)));
                assert_eq!(
                    attr(optional, "db.response.returned_rows"),
                    Some(Value::I64(1))
                );
                assert_eq!(
                    attr(named(&spans, "INSERT users")[0], "db.query.text"),
                    Some(Value::from("INSERT INTO users (id, name) VALUES (?, ?)"))
                );
                let Some(PointValue::Histogram(durations)) = point(
                    &h.meters,
                    "db.client.operation.duration",
                    &[
                        ("db.query.summary", "SELECT users"),
                        ("db.system.name", "postgresql"),
                    ],
                ) else {
                    panic!("no durations for SELECT users")
                };
                assert_eq!(durations.count(), 2);
                check_names(&h);
            }

            #[tokio::test]
            async fn a_failed_query_records_its_sqlstate() {
                let Some(url) = database_url() else { return };
                let h = harness();
                let pool = pool(&url).await;
                sqlx::query("CREATE TEMP TABLE keys (id INT PRIMARY KEY)")
                    .execute(&pool)
                    .await
                    .unwrap();
                let insert = "INSERT INTO keys (id) VALUES (1)";
                sqlx::query(insert).execute(&pool).await.unwrap();
                let error = sqlx::query(insert)
                    .execute(&pool)
                    .await
                    .expect_err("a duplicate");
                assert_eq!(
                    error.as_database_error().and_then(|e| e.code()).as_deref(),
                    Some("23505")
                );

                let spans = h.spans.finished_spans();
                let inserts = named(&spans, "INSERT keys");
                assert_eq!(inserts.len(), 2);
                assert_eq!(attr(inserts[1], "error.type"), Some(Value::from("23505")));
                assert_eq!(
                    attr(inserts[1], "db.response.status_code"),
                    Some(Value::from("23505"))
                );
                assert!(matches!(inserts[1].status, Status::Error { .. }));
                assert!(
                    point(
                        &h.meters,
                        "db.client.operation.duration",
                        &[("error.type", "23505")]
                    )
                    .is_some()
                );
                check_names(&h);
            }

            #[tokio::test]
            async fn transactions_and_connections_record_their_calls() {
                let Some(url) = database_url() else { return };
                let h = harness();
                let pool = pool(&url).await;
                sqlx::query("CREATE TEMP TABLE events (id INT)")
                    .execute(&pool)
                    .await
                    .unwrap();

                let mut tx = pool.begin().await.unwrap();
                sqlx::query("INSERT INTO events (id) VALUES ($1)")
                    .bind(1i32)
                    .execute(&mut tx)
                    .await
                    .unwrap();
                tx.commit().await.unwrap();
                let mut tx = pool.begin().await.unwrap();
                sqlx::query("INSERT INTO events (id) VALUES ($1)")
                    .bind(2i32)
                    .execute(&mut tx)
                    .await
                    .unwrap();
                tx.rollback().await.unwrap();

                let mut conn = pool.acquire().await.unwrap();
                let count: (i64,) = sqlx::query_as("SELECT count(*) FROM events")
                    .fetch_one(&mut conn)
                    .await
                    .unwrap();
                assert_eq!(count.0, 1);
                drop(conn);

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
                        "COMMIT",
                        "BEGIN",
                        "INSERT events",
                        "ROLLBACK",
                        "SELECT events"
                    ]
                );
                check_names(&h);
            }

            #[tokio::test]
            async fn the_pool_records_its_connections() {
                let Some(url) = database_url() else { return };
                let h = harness();
                let pool = pool(&url).await.with_name("orders");
                let name = ("db.client.connection.pool.name", "orders");

                let conn = pool.acquire().await.unwrap();
                let count = |state| {
                    point(
                        &h.meters,
                        "db.client.connection.count",
                        &[name, ("db.client.connection.state", state)],
                    )
                };
                assert_eq!(count("used"), Some(PointValue::Gauge(1.0)));
                assert_eq!(count("idle"), Some(PointValue::Gauge(0.0)));
                drop(conn);
                assert_eq!(count("used"), Some(PointValue::Gauge(0.0)), "returned");
                assert_eq!(count("idle"), Some(PointValue::Gauge(1.0)));
                sqlx::query("SELECT 1").execute(&pool).await.unwrap();

                assert_eq!(
                    point(&h.meters, "db.client.connection.max", &[name]),
                    Some(PointValue::Gauge(1.0))
                );
                assert_eq!(
                    point(&h.meters, "db.client.connection.pending_requests", &[name]),
                    Some(PointValue::Gauge(0.0))
                );
                let histogram_count = |metric| match point(&h.meters, metric, &[name]) {
                    Some(PointValue::Histogram(h)) => h.count(),
                    other => panic!("{metric}: {other:?}"),
                };
                assert_eq!(histogram_count("db.client.connection.wait_time"), 2);
                assert_eq!(histogram_count("db.client.connection.use_time"), 2);
                check_names(&h);
            }
        }
    };
}

against!(sqlx_0_8, "sqlx-0.8", sqlx_0_8, v0_8);
against!(sqlx_0_9, "sqlx-0.9", sqlx_0_9, v0_9);
