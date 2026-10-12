//! sqlx 0.8 and 0.9, by wrapping the pool: [`v0_8`] with the `sqlx-0.8`
//! feature, [`v0_9`] with `sqlx-0.9`.
//!
//! ```no_run
//! # #[cfg(feature = "sqlx-0.9")]
//! # async fn run(pool: sqlx_0_9::PgPool) -> Result<(), sqlx_0_9::Error> {
//! # use sqlx_0_9 as sqlx;
//! let pool = oxtel_sql::sqlx::v0_9::Pool::new(pool);
//!
//! let users = sqlx::query("SELECT name FROM users").fetch_all(&pool).await?;
//! let mut tx = pool.begin().await?;
//! sqlx::query("UPDATE users SET seen = now()").execute(&mut tx).await?;
//! tx.commit().await?;
//! # Ok(()) }
//! ```
//!
//! Queries run on the wrapped [`Pool`](v0_9::Pool), on a
//! [`Transaction`](v0_9::Transaction) from its `begin`, or on a
//! [`Connection`](v0_9::Connection) from its `acquire` are each recorded as
//! a database call:
//!
//! - **a client span**, named by the query summary, with `db.system.name`
//!   (`postgresql`, `mysql` or `sqlite`), `db.namespace` and the server from
//!   the pool's connection options, `db.query.text` (literals replaced by
//!   `?`) and `db.response.returned_rows`;
//! - **`db.client.operation.duration`**, from when the query starts on its
//!   connection;
//! - for a failed query, the database's error code (a SQLSTATE for
//!   PostgreSQL and MySQL) as `db.response.status_code` and `error.type`, or
//!   sqlx's error kind, such as `sqlx::Error::PoolTimedOut`.
//!
//! `BEGIN`, `COMMIT` and `ROLLBACK` are calls too. Preparing a statement
//! isn't. The pool also records the conventions' connection pool metrics
//! (`db.client.connection.*`): connections in use and idle, callers
//! waiting, wait and use times, and timeouts. They're still in development
//! in the conventions.
//!
//! The wrappers dereference to sqlx's types, so the rest of their methods
//! still work. A query run through sqlx's own types, such as `&*pool` or
//! `&mut **tx`, isn't recorded by the wrapper.
//!
//! # This, or oxtel's sqlx mapping
//!
//! `oxtel::init()` also records sqlx's queries with no code change, from
//! the event sqlx writes as each finishes. That event carries no error and
//! doesn't say which database it's about, so its spans can't be marked as
//! failed and their `db.system.name` is `other_sql` unless configured. It
//! needs sqlx's statement logging, which is on by default. The wrapper
//! records all of it, but only for queries run through it. The two never
//! count a query twice: the mapping leaves alone the queries the wrapper
//! records. See `oxtel::mappings` and the
//! [databases guide](https://agentstatelabs.github.io/oxtel/guide/databases/).

use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Instant;

use futures_core::Stream;
use futures_core::stream::BoxStream;

use crate::record::{self, Analysis, Call, Failure, UNKNOWN};

#[cfg(feature = "sqlx-0.8")]
pub mod v0_8;
#[cfg(feature = "sqlx-0.9")]
pub mod v0_9;

/// `db.system.name` for a sqlx database, from its `Database::NAME`.
fn system_of(name: &str) -> &'static str {
    match name {
        "PostgreSQL" => "postgresql",
        "MySQL" => "mysql",
        "SQLite" => "sqlite",
        _ => "other_sql",
    }
}

/// Whether a database's connection options can be read as a URL: the
/// drivers that override `ConnectOptions::to_url_lossy`, which panics
/// otherwise.
fn has_url(name: &str) -> bool {
    matches!(name, "PostgreSQL" | "MySQL" | "SQLite")
}

/// What's known about a pool's database, and the pool's name in its
/// metrics.
#[derive(Debug)]
pub(crate) struct Target {
    system: &'static str,
    namespace: Option<String>,
    server: Option<(String, u16)>,
    pub(crate) pool_name: Arc<str>,
}

impl Target {
    /// For a database named `name` (its `Database::NAME`) at `url`'s host,
    /// port and path. A file database's path isn't a namespace or a
    /// server.
    fn new(name: &str, url: Option<(Option<&str>, Option<u16>, &str)>) -> Self {
        let system = system_of(name);
        let (namespace, server) = match url {
            Some((host, port, path)) if system != "sqlite" => {
                let namespace = path.trim_start_matches('/');
                let namespace = (!namespace.is_empty()).then(|| namespace.to_string());
                let default_port = if system == "mysql" { 3306 } else { 5432 };
                let server = host
                    .filter(|h| !h.is_empty())
                    .map(|h| (h.to_string(), port.unwrap_or(default_port)));
                (namespace, server)
            }
            _ => (None, None),
        };
        let pool_name = crate::pool::default_name(
            system,
            namespace.as_deref(),
            server.as_ref().map(|(a, p)| (a.as_str(), *p)),
        );
        Target {
            system,
            namespace,
            server,
            pool_name: pool_name.into(),
        }
    }

    fn call<'a>(&'a self, analysis: &'a Analysis) -> Call<'a> {
        Call {
            system: self.system,
            namespace: self.namespace.as_deref(),
            server: self.server.as_ref().map(|(a, p)| (a.as_str(), *p)),
            analysis,
        }
    }

    fn with_pool_name(&self, name: &str) -> Self {
        Target {
            system: self.system,
            namespace: self.namespace.clone(),
            server: self.server.clone(),
            pool_name: name.into(),
        }
    }
}

/// Where a query's text is put once it's known: before the call, for sqlx
/// 0.8, or as sqlx reads it, for 0.9.
pub(crate) type Statement = Arc<Mutex<Option<Arc<Analysis>>>>;

fn statement(analysis: Option<Arc<Analysis>>) -> Statement {
    Arc::new(Mutex::new(analysis))
}

/// Why a query failed: `error.type`, and the database's code.
pub(crate) struct Fail {
    error_type: String,
    code: Option<String>,
}

impl Fail {
    /// A failure the database reported with `code`, or sqlx reported as
    /// `kind` (such as `PoolTimedOut`), or as a kind this crate doesn't know.
    fn new(code: Option<String>, kind: Option<&str>) -> Self {
        let error_type = match (&code, kind) {
            (Some(code), _) => code.clone(),
            (None, Some(kind)) => format!("sqlx::Error::{kind}"),
            (None, None) => "sqlx::Error".to_string(),
        };
        Fail { error_type, code }
    }
}

/// One query: its span, opened when the call is made, and what's recorded
/// when it finishes, fails or is dropped.
pub(crate) struct QueryCall {
    target: Arc<Target>,
    span: tracing::Span,
    started: Instant,
    rows: u64,
    statement: Statement,
    done: bool,
}

impl QueryCall {
    fn start(target: &Arc<Target>, statement: Statement) -> Self {
        let span = record::open_span(
            target.system,
            target.namespace.as_deref(),
            target.server.as_ref().map(|(a, p)| (a.as_str(), *p)),
        );
        QueryCall {
            target: Arc::clone(target),
            span,
            started: Instant::now(),
            rows: 0,
            statement,
            done: false,
        }
    }

    fn finish(&mut self, failure: Option<Fail>) {
        if std::mem::replace(&mut self.done, true) {
            return;
        }
        let seconds = self.started.elapsed().as_secs_f64();
        let analysis = self
            .statement
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        let analysis = analysis.as_deref().unwrap_or(&UNKNOWN);
        record::describe(&self.span, analysis);
        let failure = failure.as_ref().map(|f| Failure {
            error_type: &f.error_type,
            status_code: f.code.as_deref(),
        });
        match &failure {
            Some(failure) => failure.record(&self.span),
            None => {
                self.span.record("db.response.returned_rows", self.rows);
            }
        }
        // Inside the span, so the measurement's exemplar points at it.
        let _entered = self.span.enter();
        self.target
            .call(analysis)
            .record_duration(seconds, failure.as_ref());
    }
}

impl Drop for QueryCall {
    fn drop(&mut self) {
        // Dropped before the end, such as after the first row of a stream.
        self.finish(None);
    }
}

/// A query's results, recorded as they're read.
pub(crate) struct Recorded<'e, T, E> {
    inner: BoxStream<'e, Result<T, E>>,
    call: QueryCall,
    is_row: fn(&T) -> bool,
    fail: fn(&E) -> Fail,
}

impl<'e, T, E> Recorded<'e, T, E> {
    fn new(
        inner: BoxStream<'e, Result<T, E>>,
        call: QueryCall,
        is_row: fn(&T) -> bool,
        fail: fn(&E) -> Fail,
    ) -> Self {
        Recorded {
            inner,
            call,
            is_row,
            fail,
        }
    }
}

impl<T, E> Stream for Recorded<'_, T, E> {
    type Item = Result<T, E>;

    fn poll_next(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let span = this.call.span.clone();
        let _entered = span.enter();
        let poll = this.inner.as_mut().poll_next(cx);
        match &poll {
            Poll::Ready(Some(Ok(item))) if (this.is_row)(item) => this.call.rows += 1,
            Poll::Ready(Some(Ok(_))) | Poll::Pending => {}
            Poll::Ready(Some(Err(error))) => this.call.finish(Some((this.fail)(error))),
            Poll::Ready(None) => this.call.finish(None),
        }
        poll
    }
}

/// Run one query future inside its call, recording rows and the outcome.
async fn record_one<R, E>(
    mut call: QueryCall,
    operation: impl std::future::Future<Output = Result<R, E>>,
    rows: fn(&R) -> u64,
    fail: fn(&E) -> Fail,
) -> Result<R, E> {
    use tracing::Instrument;
    let result = operation.instrument(call.span.clone()).await;
    match &result {
        Ok(value) => {
            call.rows = rows(value);
            call.finish(None);
        }
        Err(error) => call.finish(Some(fail(error))),
    }
    result
}
