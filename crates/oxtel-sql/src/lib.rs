//! Database client spans and metrics, following the OpenTelemetry semantic
//! conventions for databases. Part of [oxtel](https://github.com/agentstatelabs/oxtel).
//!
//! With the `rusqlite` feature, one line instruments a SQLite connection:
//!
//! ```no_run
//! # #[cfg(feature = "rusqlite")] {
//! let conn = rusqlite::Connection::open("app.db").unwrap();
//! oxtel_sql::rusqlite::instrument(&conn);
//! # }
//! ```
//!
//! With the `postgres` feature, one line wraps a tokio-postgres client where
//! the connection is made, and with `deadpool-postgres`, a pool (see
//! [`postgres`]):
//!
//! ```no_run
//! # #[cfg(feature = "postgres")]
//! # async fn run(client: tokio_postgres::Client, config: tokio_postgres::Config) {
//! let client = oxtel_sql::postgres::Client::new(client, &config);
//! # }
//! ```
//!
//! Every statement they run then gets:
//!
//! - **a client span**, named by its query summary (`SELECT users`, `INSERT
//!   orders SELECT carts`), with `db.system.name`, `db.query.summary` and
//!   `db.query.text`, plus `db.namespace` and the server for PostgreSQL;
//! - **`db.client.operation.duration`**, through the `metrics` facade, by
//!   system and query summary;
//! - for PostgreSQL, when it fails, the error's SQLSTATE as
//!   `db.response.status_code` and `error.type`, on both.
//!
//! `db.query.text` is the statement with every literal replaced by `?` and
//! comments removed, so values never reach telemetry; turn it off with
//! [`record_query_text`]. As the conventions say for raw SQL, the operation
//! and table names aren't taken from the text into `db.operation.name` and
//! `db.collection.name`: the summary carries them.
//!
//! Spans are recorded with `tracing` and metrics with `metrics`, so they go
//! wherever the program sends them: with `oxtel::init()`, to OTLP or
//! Prometheus. The spans' target is `oxtel_sql`, so `RUST_LOG=…,oxtel_sql=off`
//! turns them off.
//!
//! For sqlx, `oxtel::init()` makes the same spans and metric from sqlx's own
//! query events, with no code change, but without failures or which database
//! a query went to; see `oxtel::mappings`.
//!
//! [`summarize`] and [`sanitize`] are available on their own, for
//! instrumenting a database client this crate doesn't cover.

#[cfg(feature = "postgres")]
pub mod postgres;
mod query;
mod record;
#[cfg(feature = "rusqlite")]
pub mod rusqlite;

pub use query::{sanitize, summarize};
pub use record::{describe_metrics, record_query_text, records_query_text};
