//! PostgreSQL through tokio-postgres, and deadpool-postgres pools.
//!
//! tokio-postgres doesn't say what SQL a prepared statement holds, and its
//! traits are sealed, so instrumenting it means wrapping the client. Wrap it
//! where the connection is made:
//!
//! ```no_run
//! # async fn run() -> Result<(), tokio_postgres::Error> {
//! let config: tokio_postgres::Config = "host=localhost user=app dbname=shop".parse()?;
//! let (client, connection) = config.connect(tokio_postgres::NoTls).await?;
//! tokio::spawn(connection);
//! let client = oxtel_sql::postgres::Client::new(client, &config);
//!
//! let rows = client.query("SELECT name FROM users WHERE id = $1", &[&7i32]).await?;
//! # Ok(()) }
//! ```
//!
//! [`Client`] and [`Transaction`] have tokio-postgres's `query`,
//! `query_one`, `query_opt`, `execute`, `batch_execute` and `simple_query`,
//! each recorded as one database call: a client span named by the query
//! summary, with `db.namespace`, `server.address` and `server.port` from the
//! connection's configuration, and `db.client.operation.duration`. A call
//! that fails records the error's SQLSTATE as `db.response.status_code` and
//! `error.type`. Starting, committing and rolling back a transaction are
//! calls too.
//!
//! [`prepare`](Client::prepare) returns a [`Statement`] that keeps its text,
//! so calls running it are named like any other. Preparing isn't recorded
//! as a call of its own. Both types dereference to tokio-postgres's, so
//! anything else, such as `copy_in` or `query_raw`, still works, without
//! telemetry.
//!
//! Helpers that should take either a client or a transaction can be generic
//! over [`GenericClient`]. With the `deadpool-postgres` feature, [`Pool`]
//! wraps a pool so that every client taken from it is instrumented, and
//! records the conventions' connection pool metrics.

use std::future::Future;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use std::time::Instant;

use tokio_postgres::error::SqlState;
use tokio_postgres::types::ToSql;
use tokio_postgres::{Error, Row, SimpleQueryMessage};
use tracing::Instrument;

use crate::record::{Analysis, Call, Failure, analyze};

const POSTGRESQL: &str = "postgresql";

/// `error.type` for a failure that has no SQLSTATE, such as a lost
/// connection.
const CLIENT_ERROR: &str = "tokio_postgres::Error";

/// Where a client's connections go, for `db.namespace`, `server.address`
/// and `server.port`. Cheap to clone.
#[derive(Clone, Debug, Default)]
pub struct Database(Arc<Target>);

#[derive(Debug, Default)]
struct Target {
    namespace: Option<String>,
    server: Option<(String, u16)>,
}

impl Database {
    /// The database a configuration connects to. The server is recorded
    /// only when the configuration names exactly one host, since with
    /// several there's no telling which one answered.
    pub fn new(config: &tokio_postgres::Config) -> Self {
        crate::record::describe_metrics();
        let server = match config.get_hosts() {
            [host] => {
                let address = match host {
                    tokio_postgres::config::Host::Tcp(name) => name.clone(),
                    #[cfg(unix)]
                    tokio_postgres::config::Host::Unix(path) => path.display().to_string(),
                };
                let port = config.get_ports().first().copied().unwrap_or(5432);
                Some((address, port))
            }
            _ => None,
        };
        Database(Arc::new(Target {
            namespace: config.get_dbname().map(str::to_owned),
            server,
        }))
    }

    fn call<'a>(&'a self, analysis: &'a Analysis) -> Call<'a> {
        Call {
            system: POSTGRESQL,
            namespace: self.0.namespace.as_deref(),
            server: self.0.server.as_ref().map(|(a, p)| (a.as_str(), *p)),
            analysis,
        }
    }
}

/// Run one database call inside its span, then record its duration and,
/// if it failed, why.
async fn record<R>(
    database: &Database,
    analysis: &Analysis,
    operation: impl Future<Output = Result<R, Error>>,
) -> Result<R, Error> {
    let call = database.call(analysis);
    let span = call.span();
    let started = Instant::now();
    let result = operation.instrument(span.clone()).await;
    let seconds = started.elapsed().as_secs_f64();
    // Inside the span, so the measurement's exemplar points at it.
    let _entered = span.enter();
    match &result {
        Ok(_) => call.record_duration(seconds, None),
        Err(error) => {
            let code = error.code().map(SqlState::code);
            let failure = Failure {
                error_type: code.unwrap_or(CLIENT_ERROR),
                status_code: code,
            };
            failure.record(&span);
            call.record_duration(seconds, Some(&failure));
        }
    }
    result
}

/// A statement prepared through an instrumented client: tokio-postgres's
/// [`Statement`](tokio_postgres::Statement), keeping its text so the calls
/// that run it are named. It dereferences to tokio-postgres's.
#[derive(Clone)]
pub struct Statement {
    inner: tokio_postgres::Statement,
    analysis: Arc<Analysis>,
}

impl Statement {
    /// tokio-postgres's statement.
    pub fn into_inner(self) -> tokio_postgres::Statement {
        self.inner
    }
}

impl Deref for Statement {
    type Target = tokio_postgres::Statement;

    fn deref(&self) -> &tokio_postgres::Statement {
        &self.inner
    }
}

impl std::fmt::Debug for Statement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Statement")
            .field("summary", &self.analysis.summary)
            .finish_non_exhaustive()
    }
}

/// A statement the instrumented clients run: SQL text, a [`Statement`]
/// prepared through them, or one of tokio-postgres's own, which is recorded
/// without a name or text.
pub trait Query: sealed::Query + Sync {}

impl Query for str {}
impl Query for String {}
impl Query for Statement {}
impl Query for tokio_postgres::Statement {}

mod sealed {
    use crate::record::Analysis;

    pub enum Target<'a> {
        Text(&'a str),
        Prepared(Prepared<'a>),
    }

    /// A prepared statement and what's known about its text.
    pub struct Prepared<'a> {
        pub(crate) statement: &'a tokio_postgres::Statement,
        pub(crate) analysis: &'a Analysis,
    }

    pub trait Query {
        fn target(&self) -> Target<'_>;
    }

    impl Query for str {
        fn target(&self) -> Target<'_> {
            Target::Text(self)
        }
    }

    impl Query for String {
        fn target(&self) -> Target<'_> {
            Target::Text(self)
        }
    }

    impl Query for super::Statement {
        fn target(&self) -> Target<'_> {
            Target::Prepared(Prepared {
                statement: &self.inner,
                analysis: &self.analysis,
            })
        }
    }

    impl Query for tokio_postgres::Statement {
        fn target(&self) -> Target<'_> {
            Target::Prepared(Prepared {
                statement: self,
                analysis: &crate::record::UNKNOWN,
            })
        }
    }
}

use sealed::Target as Run;

/// The instrumented calls, on [`Client`] and [`Transaction`] alike. Each
/// type has `fn pg(&self)`, returning tokio-postgres's client or
/// transaction, and a `database` field.
macro_rules! calls {
    () => {
        /// Prepare a statement, keeping its text so that calls running it
        /// are named. Preparing isn't recorded as a call.
        pub async fn prepare(&self, query: &str) -> Result<Statement, Error> {
            let inner = self.pg().prepare(query).await?;
            Ok(Statement {
                inner,
                analysis: analyze(query),
            })
        }

        /// Run a statement and return the rows it produced.
        pub async fn query<T>(
            &self,
            statement: &T,
            params: &[&(dyn ToSql + Sync)],
        ) -> Result<Vec<Row>, Error>
        where
            T: ?Sized + Query,
        {
            match statement.target() {
                Run::Text(sql) => {
                    record(&self.database, &analyze(sql), self.pg().query(sql, params)).await
                }
                Run::Prepared(p) => {
                    record(
                        &self.database,
                        p.analysis,
                        self.pg().query(p.statement, params),
                    )
                    .await
                }
            }
        }

        /// Run a statement that returns exactly one row.
        pub async fn query_one<T>(
            &self,
            statement: &T,
            params: &[&(dyn ToSql + Sync)],
        ) -> Result<Row, Error>
        where
            T: ?Sized + Query,
        {
            match statement.target() {
                Run::Text(sql) => {
                    record(
                        &self.database,
                        &analyze(sql),
                        self.pg().query_one(sql, params),
                    )
                    .await
                }
                Run::Prepared(p) => {
                    record(
                        &self.database,
                        p.analysis,
                        self.pg().query_one(p.statement, params),
                    )
                    .await
                }
            }
        }

        /// Run a statement that returns at most one row.
        pub async fn query_opt<T>(
            &self,
            statement: &T,
            params: &[&(dyn ToSql + Sync)],
        ) -> Result<Option<Row>, Error>
        where
            T: ?Sized + Query,
        {
            match statement.target() {
                Run::Text(sql) => {
                    record(
                        &self.database,
                        &analyze(sql),
                        self.pg().query_opt(sql, params),
                    )
                    .await
                }
                Run::Prepared(p) => {
                    record(
                        &self.database,
                        p.analysis,
                        self.pg().query_opt(p.statement, params),
                    )
                    .await
                }
            }
        }

        /// Run a statement and return how many rows it changed.
        pub async fn execute<T>(
            &self,
            statement: &T,
            params: &[&(dyn ToSql + Sync)],
        ) -> Result<u64, Error>
        where
            T: ?Sized + Query,
        {
            match statement.target() {
                Run::Text(sql) => {
                    record(
                        &self.database,
                        &analyze(sql),
                        self.pg().execute(sql, params),
                    )
                    .await
                }
                Run::Prepared(p) => {
                    record(
                        &self.database,
                        p.analysis,
                        self.pg().execute(p.statement, params),
                    )
                    .await
                }
            }
        }

        /// Run statements separated by semicolons, without parameters, as
        /// one call.
        pub async fn batch_execute(&self, query: &str) -> Result<(), Error> {
            record(
                &self.database,
                &analyze(query),
                self.pg().batch_execute(query),
            )
            .await
        }

        /// Run statements through the simple query protocol, as one call.
        pub async fn simple_query(&self, query: &str) -> Result<Vec<SimpleQueryMessage>, Error> {
            record(
                &self.database,
                &analyze(query),
                self.pg().simple_query(query),
            )
            .await
        }
    };
}

/// An instrumented tokio-postgres client, or a pooled one (see [`AsClient`]).
/// It dereferences to tokio-postgres's [`Client`](tokio_postgres::Client),
/// so methods it doesn't wrap still work, without telemetry.
pub struct Client<C = tokio_postgres::Client> {
    inner: C,
    database: Database,
    // Declared after `inner`, so a pooled connection is back in its pool
    // when the lease ends.
    _lease: Option<Lease>,
}

// A pooled client's time out of its pool.
#[cfg(feature = "deadpool-postgres")]
use crate::pool::Lease;
#[cfg(not(feature = "deadpool-postgres"))]
type Lease = std::convert::Infallible;

/// The clients a [`Client`] can wrap: tokio-postgres's own, and with the
/// `deadpool-postgres` feature, a client taken from a deadpool pool.
pub trait AsClient {
    /// tokio-postgres's client.
    fn as_client(&self) -> &tokio_postgres::Client;
    /// tokio-postgres's client, mutably, for starting transactions.
    fn as_client_mut(&mut self) -> &mut tokio_postgres::Client;
}

impl AsClient for tokio_postgres::Client {
    fn as_client(&self) -> &tokio_postgres::Client {
        self
    }

    fn as_client_mut(&mut self) -> &mut tokio_postgres::Client {
        self
    }
}

impl<C: AsClient> Client<C> {
    /// Instrument a client connected with `config`.
    pub fn new(client: C, config: &tokio_postgres::Config) -> Self {
        Self::with_database(client, &Database::new(config))
    }

    /// Instrument a client, with what's known about where it's connected.
    /// For many clients to one database, work that out once with
    /// [`Database::new`].
    pub fn with_database(client: C, database: &Database) -> Self {
        Client {
            inner: client,
            database: database.clone(),
            _lease: None,
        }
    }

    /// The wrapped client.
    pub fn get_ref(&self) -> &C {
        &self.inner
    }

    /// The wrapped client, without the instrumentation. For a client from
    /// a [`Pool`], its use ends here as far as the pool's metrics go.
    pub fn into_inner(self) -> C {
        self.inner
    }

    fn pg(&self) -> &tokio_postgres::Client {
        self.inner.as_client()
    }

    calls!();

    /// Start a transaction. Its `BEGIN` is recorded as a call.
    pub async fn transaction(&mut self) -> Result<Transaction<'_>, Error> {
        let database = self.database.clone();
        let inner = record(
            &database,
            &analyze("BEGIN"),
            self.inner.as_client_mut().transaction(),
        )
        .await?;
        Ok(Transaction { inner, database })
    }
}

impl<C: AsClient> Deref for Client<C> {
    type Target = tokio_postgres::Client;

    fn deref(&self) -> &tokio_postgres::Client {
        self.inner.as_client()
    }
}

impl<C: AsClient> DerefMut for Client<C> {
    fn deref_mut(&mut self) -> &mut tokio_postgres::Client {
        self.inner.as_client_mut()
    }
}

impl<C> std::fmt::Debug for Client<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("database", &self.database)
            .finish_non_exhaustive()
    }
}

/// An instrumented transaction, from [`Client::transaction`]. Like
/// tokio-postgres's, dropping it without committing rolls it back, but that
/// rollback isn't recorded. It dereferences to tokio-postgres's
/// [`Transaction`](tokio_postgres::Transaction).
pub struct Transaction<'a> {
    inner: tokio_postgres::Transaction<'a>,
    database: Database,
}

impl<'a> Transaction<'a> {
    fn pg(&self) -> &tokio_postgres::Transaction<'a> {
        &self.inner
    }

    calls!();

    /// Commit, recorded as a call.
    pub async fn commit(self) -> Result<(), Error> {
        record(&self.database, &analyze("COMMIT"), self.inner.commit()).await
    }

    /// Roll back, recorded as a call.
    pub async fn rollback(self) -> Result<(), Error> {
        record(&self.database, &analyze("ROLLBACK"), self.inner.rollback()).await
    }

    /// Start a nested transaction, which is a savepoint, recorded as a call.
    pub async fn transaction(&mut self) -> Result<Transaction<'_>, Error> {
        let database = self.database.clone();
        let inner = record(&database, &analyze("SAVEPOINT"), self.inner.transaction()).await?;
        Ok(Transaction { inner, database })
    }
}

impl<'a> Deref for Transaction<'a> {
    type Target = tokio_postgres::Transaction<'a>;

    fn deref(&self) -> &tokio_postgres::Transaction<'a> {
        &self.inner
    }
}

impl std::fmt::Debug for Transaction<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Transaction")
            .field("database", &self.database)
            .finish_non_exhaustive()
    }
}

/// The instrumented calls of a [`Client`] or a [`Transaction`], for helpers
/// that take either. tokio-postgres's own `GenericClient` is sealed.
pub trait GenericClient: Sync {
    /// See [`Client::prepare`].
    fn prepare(&self, query: &str) -> impl Future<Output = Result<Statement, Error>> + Send;

    /// See [`Client::query`].
    fn query<T>(
        &self,
        statement: &T,
        params: &[&(dyn ToSql + Sync)],
    ) -> impl Future<Output = Result<Vec<Row>, Error>> + Send
    where
        T: ?Sized + Query;

    /// See [`Client::query_one`].
    fn query_one<T>(
        &self,
        statement: &T,
        params: &[&(dyn ToSql + Sync)],
    ) -> impl Future<Output = Result<Row, Error>> + Send
    where
        T: ?Sized + Query;

    /// See [`Client::query_opt`].
    fn query_opt<T>(
        &self,
        statement: &T,
        params: &[&(dyn ToSql + Sync)],
    ) -> impl Future<Output = Result<Option<Row>, Error>> + Send
    where
        T: ?Sized + Query;

    /// See [`Client::execute`].
    fn execute<T>(
        &self,
        statement: &T,
        params: &[&(dyn ToSql + Sync)],
    ) -> impl Future<Output = Result<u64, Error>> + Send
    where
        T: ?Sized + Query;

    /// See [`Client::batch_execute`].
    fn batch_execute(&self, query: &str) -> impl Future<Output = Result<(), Error>> + Send;
}

/// Implements [`GenericClient`] by calling the type's own methods.
macro_rules! generic_client {
    () => {
        fn prepare(&self, query: &str) -> impl Future<Output = Result<Statement, Error>> + Send {
            Self::prepare(self, query)
        }

        fn query<T>(
            &self,
            statement: &T,
            params: &[&(dyn ToSql + Sync)],
        ) -> impl Future<Output = Result<Vec<Row>, Error>> + Send
        where
            T: ?Sized + Query,
        {
            Self::query(self, statement, params)
        }

        fn query_one<T>(
            &self,
            statement: &T,
            params: &[&(dyn ToSql + Sync)],
        ) -> impl Future<Output = Result<Row, Error>> + Send
        where
            T: ?Sized + Query,
        {
            Self::query_one(self, statement, params)
        }

        fn query_opt<T>(
            &self,
            statement: &T,
            params: &[&(dyn ToSql + Sync)],
        ) -> impl Future<Output = Result<Option<Row>, Error>> + Send
        where
            T: ?Sized + Query,
        {
            Self::query_opt(self, statement, params)
        }

        fn execute<T>(
            &self,
            statement: &T,
            params: &[&(dyn ToSql + Sync)],
        ) -> impl Future<Output = Result<u64, Error>> + Send
        where
            T: ?Sized + Query,
        {
            Self::execute(self, statement, params)
        }

        fn batch_execute(&self, query: &str) -> impl Future<Output = Result<(), Error>> + Send {
            Self::batch_execute(self, query)
        }
    };
}

impl<C: AsClient + Sync> GenericClient for Client<C> {
    generic_client!();
}

impl GenericClient for Transaction<'_> {
    generic_client!();
}

#[cfg(feature = "deadpool-postgres")]
mod pool {
    use super::*;

    impl AsClient for deadpool_postgres::Object {
        fn as_client(&self) -> &tokio_postgres::Client {
            self
        }

        fn as_client_mut(&mut self) -> &mut tokio_postgres::Client {
            self
        }
    }

    impl Client<deadpool_postgres::Object> {
        /// Prepare a statement through the pooled connection's statement
        /// cache, keeping its text so that calls running it are named.
        pub async fn prepare_cached(&self, query: &str) -> Result<Statement, Error> {
            let inner = self.inner.prepare_cached(query).await?;
            Ok(Statement {
                inner,
                analysis: analyze(query),
            })
        }
    }

    /// A deadpool-postgres pool whose clients are instrumented. It
    /// dereferences to deadpool's [`Pool`](deadpool_postgres::Pool).
    ///
    /// It also records the conventions' connection pool metrics, labelled
    /// with `db.client.connection.pool.name`:
    ///
    /// - `db.client.connection.count`, by `db.client.connection.state`
    ///   (`idle` or `used`), and `db.client.connection.max`;
    /// - `db.client.connection.pending_requests`: callers waiting for a
    ///   connection;
    /// - `db.client.connection.wait_time` and `db.client.connection.use_time`,
    ///   in seconds;
    /// - `db.client.connection.timeouts`.
    ///
    /// The counts are taken from the pool's status whenever a client is
    /// taken or returned. These metrics are still in development in the
    /// conventions, so their names may change. The counts carry no unit,
    /// since the `metrics` facade can't express `{connection}`.
    #[derive(Clone)]
    pub struct Pool {
        inner: deadpool_postgres::Pool,
        database: Database,
        name: Arc<str>,
    }

    impl Pool {
        /// Instrument a pool whose connections are made with `config`. Its
        /// name in the pool metrics is `server.address:server.port/db.namespace`,
        /// as the conventions suggest for a pool without one.
        pub fn new(pool: deadpool_postgres::Pool, config: &tokio_postgres::Config) -> Self {
            crate::pool::describe_metrics();
            let database = Database::new(config);
            let name = crate::pool::default_name(
                POSTGRESQL,
                database.0.namespace.as_deref(),
                database.0.server.as_ref().map(|(a, p)| (a.as_str(), *p)),
            );
            Pool {
                inner: pool,
                database,
                name: name.into(),
            }
        }

        /// Name the pool in its metrics, for a program with more than one.
        pub fn with_name(mut self, name: impl Into<String>) -> Self {
            self.name = name.into().into();
            self
        }

        /// Take a client from the pool, instrumented.
        pub async fn get(
            &self,
        ) -> Result<Client<deadpool_postgres::Object>, deadpool_postgres::PoolError> {
            let waiting = crate::pool::Waiting::start(&self.name);
            let started = Instant::now();
            let result = self.inner.get().await;
            drop(waiting);
            let timed_out = matches!(&result, Err(deadpool_postgres::PoolError::Timeout(_)));
            crate::pool::record_wait(&self.name, started, timed_out);
            sample(&self.inner, &self.name);
            let client = result?;
            let (pool, name) = (self.inner.clone(), Arc::clone(&self.name));
            Ok(Client {
                inner: client,
                database: self.database.clone(),
                _lease: Some(Lease::start(&self.name, move || sample(&pool, &name))),
            })
        }
    }

    /// Set the connection counts and the pool's size limit from its status.
    fn sample(pool: &deadpool_postgres::Pool, name: &str) {
        let status = pool.status();
        crate::pool::record_counts(
            name,
            status.available,
            status.size.saturating_sub(status.available),
            status.max_size,
        );
    }

    impl Deref for Pool {
        type Target = deadpool_postgres::Pool;

        fn deref(&self) -> &deadpool_postgres::Pool {
            &self.inner
        }
    }

    impl std::fmt::Debug for Pool {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("Pool")
                .field("database", &self.database)
                .finish_non_exhaustive()
        }
    }
}

#[cfg(feature = "deadpool-postgres")]
pub use pool::Pool;
