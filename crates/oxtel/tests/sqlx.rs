//! The sqlx mapping against sqlx itself, each supported version, on a real
//! PostgreSQL. Set `TEST_DATABASE_URL` to run these, as CI does; without
//! it they check nothing, and say so.

#![cfg(feature = "mappings")]

use oxtel::Value;
use oxtel::sdk::trace::{InMemorySpanExporter, SpanData, Status};

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

struct Harness {
    spans: InMemorySpanExporter,
    telemetry: oxtel::Telemetry,
}

fn harness() -> Harness {
    let spans = InMemorySpanExporter::default();
    let telemetry = oxtel::builder()
        .without_env_exporters()
        .without_local_logs()
        .with_pull_metrics()
        .with_traces_filter("info")
        .with_traces({
            let spans = spans.clone();
            move |b| b.with_simple_exporter(spans)
        })
        .build()
        .unwrap();
    Harness { spans, telemetry }
}

fn attr(span: &SpanData, key: &str) -> Option<Value> {
    span.attributes
        .iter()
        .find(|kv| kv.key == key)
        .map(|kv| kv.value.clone())
}

fn named<'a>(spans: &'a [SpanData], name: &str) -> Vec<&'a SpanData> {
    spans.iter().filter(|s| s.name == name).collect()
}

macro_rules! against {
    ($module:ident, $sqlx:ident, $wrapper:ident) => {
        mod $module {
            use super::*;
            use oxtel_sql::sqlx::$wrapper;
            use sqlx::ConnectOptions as _;
            use tracing::Instrument as _;
            use $sqlx as sqlx;

            async fn pool(options: sqlx::postgres::PgConnectOptions) -> sqlx::PgPool {
                // One connection, so a temporary table stays in view.
                sqlx::postgres::PgPoolOptions::new()
                    .max_connections(1)
                    .connect_with(options)
                    .await
                    .expect("connect to TEST_DATABASE_URL")
            }

            fn options(url: &str) -> sqlx::postgres::PgConnectOptions {
                url.parse().expect("TEST_DATABASE_URL parses")
            }

            #[tokio::test]
            async fn queries_become_database_spans() {
                let Some(url) = database_url() else { return };
                let h = harness();
                let _dispatch = tracing::dispatcher::set_default(h.telemetry.dispatch());
                let _metrics = metrics::set_default_local_recorder(h.telemetry.recorder().unwrap());
                let pool = pool(options(&url)).await;

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
                // A failure: recorded, but sqlx's event can't say it failed.
                let missing = sqlx::query("SELECT id FROM no_such_table")
                    .fetch_all(&pool)
                    .await;
                assert!(missing.is_err());
                pool.close().await;

                let spans = h.spans.finished_spans();
                let select = named(&spans, "SELECT users");
                assert_eq!(
                    select.len(),
                    1,
                    "{:?}",
                    spans.iter().map(|s| &s.name).collect::<Vec<_>>()
                );
                let select = select[0];
                let request = named(&spans, "request")[0];
                assert_eq!(select.parent_span_id, Some(request.context.span_id));
                assert_eq!(select.scope.name, "sqlx");
                assert_eq!(
                    attr(select, "db.system.name"),
                    Some(Value::from("other_sql"))
                );
                assert_eq!(
                    attr(select, "db.query.text"),
                    Some(Value::from("SELECT name FROM users WHERE id = $1"))
                );
                assert_eq!(
                    attr(select, "db.response.returned_rows"),
                    Some(Value::I64(1))
                );
                assert!(select.end_time > select.start_time);

                let insert = named(&spans, "INSERT users")[0];
                assert_eq!(
                    attr(insert, "db.query.text"),
                    Some(Value::from("INSERT INTO users (id, name) VALUES (?, ?)"))
                );
                let failed = named(&spans, "SELECT no_such_table");
                assert_eq!(failed.len(), 1);
                assert_eq!(
                    failed[0].status,
                    Status::Unset,
                    "sqlx doesn't say it failed"
                );

                let durations = h
                    .telemetry
                    .guard()
                    .meter_provider()
                    .unwrap()
                    .collect()
                    .into_iter()
                    .find(|f| f.name == "db.client.operation.duration")
                    .expect("durations recorded");
                assert!(durations.points.iter().any(|p| {
                    p.attributes
                        .contains(&("db.query.summary".into(), "SELECT users".into()))
                }));
            }

            #[tokio::test]
            async fn a_query_through_oxtel_sqls_wrapper_is_recorded_once() {
                let Some(url) = database_url() else { return };
                let h = harness();
                let _dispatch = tracing::dispatcher::set_default(h.telemetry.dispatch());
                let _metrics = metrics::set_default_local_recorder(h.telemetry.recorder().unwrap());
                let pool = $wrapper::Pool::new(pool(options(&url)).await);
                sqlx::query("SELECT 1 AS one")
                    .fetch_all(&pool)
                    .await
                    .unwrap();
                pool.close().await;

                let spans = h.spans.finished_spans();
                let selects = named(&spans, "SELECT");
                assert_eq!(
                    selects.len(),
                    1,
                    "the wrapper's span, not the mapping's too"
                );
                assert_eq!(
                    attr(selects[0], "db.system.name"),
                    Some(Value::from("postgresql")),
                    "the wrapper knows the database"
                );
            }

            #[tokio::test]
            async fn nothing_is_recorded_when_sqlx_logs_no_statements() {
                let Some(url) = database_url() else { return };
                let h = harness();
                let _dispatch = tracing::dispatcher::set_default(h.telemetry.dispatch());
                let pool = pool(options(&url).log_statements(log::LevelFilter::Off)).await;
                sqlx::query("SELECT 1").fetch_one(&pool).await.unwrap();
                pool.close().await;
                assert!(named(&h.spans.finished_spans(), "SELECT").is_empty());
            }
        }
    };
}

against!(sqlx_0_8, sqlx08, v0_8);
against!(sqlx_0_9, sqlx09, v0_9);
