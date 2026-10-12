//! sqlx 0.9. See the [`sqlx`](super) module for what's recorded.

use std::fmt;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use std::time::Instant;

use futures_core::future::BoxFuture;
use futures_core::stream::BoxStream;
use futures_util::TryStreamExt;
use sqlx::database::Database;
use sqlx::error::BoxDynError;
use sqlx::pool::PoolConnection;
use sqlx::{
    ConnectOptions, Connection as _, Describe, Either, Error, Execute, Executor, SqlStr,
    Statement as _,
};
use sqlx_0_9 as sqlx;

use super::{Fail, QueryCall, Recorded, Target, has_url, record_one, statement};
use crate::pool::{Lease, Waiting};
use crate::record::analyze;

/// What a query yields as it runs: a statement's result, or a row.
type Step<DB> = Either<<DB as Database>::QueryResult, <DB as Database>::Row>;

fn is_row<DB: Database>(step: &Step<DB>) -> bool {
    step.is_right()
}

fn one_row<R>(row: &Option<R>) -> u64 {
    u64::from(row.is_some())
}

fn no_rows<T>(_: &T) -> u64 {
    0
}

fn fail(error: &Error) -> Fail {
    let kind = match error {
        Error::Database(e) => return Fail::new(e.code().map(|c| c.into_owned()), Some("Database")),
        Error::Configuration(_) => "Configuration",
        Error::InvalidArgument(_) => "InvalidArgument",
        Error::Io(_) => "Io",
        Error::Tls(_) => "Tls",
        Error::Protocol(_) => "Protocol",
        Error::RowNotFound => "RowNotFound",
        Error::TypeNotFound { .. } => "TypeNotFound",
        Error::ColumnIndexOutOfBounds { .. } => "ColumnIndexOutOfBounds",
        Error::ColumnNotFound(_) => "ColumnNotFound",
        Error::ColumnDecode { .. } => "ColumnDecode",
        Error::Encode(_) => "Encode",
        Error::Decode(_) => "Decode",
        Error::AnyDriverError(_) => "AnyDriverError",
        Error::PoolTimedOut => "PoolTimedOut",
        Error::PoolClosed => "PoolClosed",
        Error::WorkerCrashed => "WorkerCrashed",
        Error::InvalidSavePointStatement => "InvalidSavePointStatement",
        Error::BeginFailed => "BeginFailed",
        _ => return Fail::new(None, None),
    };
    Fail::new(None, Some(kind))
}

/// In sqlx 0.9, reading a query's text consumes the query, so it's read
/// as sqlx reads it: the query runs wrapped in this, which keeps the text.
struct Watched<E> {
    inner: E,
    statement: super::Statement,
}

/// Wrap `query`, returning where its text will be kept.
fn watch<E>(query: E) -> (super::Statement, Watched<E>) {
    let statement = statement(None);
    let watched = Watched {
        inner: query,
        statement: std::sync::Arc::clone(&statement),
    };
    (statement, watched)
}

fn remember(statement: &super::Statement, sql: &str) {
    let mut slot = statement.lock().unwrap_or_else(|e| e.into_inner());
    if slot.is_none() {
        *slot = Some(analyze(sql));
    }
}

impl<'q, DB: Database, E: Execute<'q, DB>> Execute<'q, DB> for Watched<E> {
    fn sql(self) -> SqlStr {
        let sql = self.inner.sql();
        remember(&self.statement, sql.as_str());
        sql
    }

    fn statement(&self) -> Option<&DB::Statement> {
        let statement = self.inner.statement();
        if let Some(prepared) = statement {
            remember(&self.statement, prepared.sql().as_str());
        }
        statement
    }

    fn take_arguments(&mut self) -> Result<Option<DB::Arguments>, BoxDynError> {
        self.inner.take_arguments()
    }

    fn persistent(&self) -> bool {
        self.inner.persistent()
    }
}

/// An instrumented sqlx pool. Run queries on `&pool`; [`begin`](Pool::begin)
/// and [`acquire`](Pool::acquire) give an instrumented transaction or
/// connection. It dereferences to sqlx's [`Pool`](sqlx::Pool).
pub struct Pool<DB: Database> {
    inner: sqlx::Pool<DB>,
    target: Arc<Target>,
}

/// A connection out of the pool, and the lease that records its use when
/// it's back. `conn` is declared first, so it's returned before the lease
/// ends.
struct Leased<DB: Database> {
    conn: PoolConnection<DB>,
    _lease: Lease,
}

impl<DB: Database> Pool<DB> {
    /// Instrument a pool. The database's name and server come from its
    /// connection options; its name in the pool metrics is
    /// `server.address:server.port/db.namespace`.
    pub fn new(pool: sqlx::Pool<DB>) -> Self {
        crate::record::describe_metrics();
        crate::pool::describe_metrics();
        let url = has_url(DB::NAME).then(|| pool.connect_options().to_url_lossy());
        let target = Target::new(
            DB::NAME,
            url.as_ref().map(|u| (u.host_str(), u.port(), u.path())),
        );
        Pool {
            inner: pool,
            target: Arc::new(target),
        }
    }

    /// Name the pool in its metrics, for a program with more than one.
    pub fn with_name(mut self, name: &str) -> Self {
        self.target = Arc::new(self.target.with_pool_name(name));
        self
    }

    /// The wrapped pool.
    pub fn get_ref(&self) -> &sqlx::Pool<DB> {
        &self.inner
    }

    /// Take a connection from the pool, instrumented.
    pub async fn acquire(&self) -> Result<Connection<DB>, Error> {
        Ok(Connection {
            leased: self.lease().await?,
            target: Arc::clone(&self.target),
        })
    }

    /// Start a transaction. Its `BEGIN`, which includes waiting for a
    /// connection, is recorded as a call.
    pub async fn begin(&self) -> Result<Transaction<'static, DB>, Error> {
        let call = QueryCall::start(&self.target, statement(Some(analyze("BEGIN"))));
        let inner = record_one(call, self.inner.begin(), no_rows, fail).await?;
        sample(&self.inner, &self.target.pool_name);
        Ok(Transaction {
            inner,
            target: Arc::clone(&self.target),
            _lease: Some(self.lease_for_use()),
        })
    }

    async fn lease(&self) -> Result<Leased<DB>, Error> {
        let name = &self.target.pool_name;
        let waiting = Waiting::start(name);
        let started = Instant::now();
        let result = self.inner.acquire().await;
        drop(waiting);
        crate::pool::record_wait(name, started, matches!(&result, Err(Error::PoolTimedOut)));
        sample(&self.inner, name);
        Ok(Leased {
            conn: result?,
            _lease: self.lease_for_use(),
        })
    }

    fn lease_for_use(&self) -> Lease {
        let (pool, name) = (self.inner.clone(), Arc::clone(&self.target.pool_name));
        Lease::start(&self.target.pool_name, move || {
            sample_returning(&pool, &name)
        })
    }
}

/// Set the connection counts and the pool's size limit.
fn sample<DB: Database>(pool: &sqlx::Pool<DB>, name: &str) {
    record(pool, name, pool.num_idle());
}

/// The counts as a connection is given back. sqlx returns it to the pool
/// in a task of its own, so it isn't idle yet; count it as idle. Where that
/// task has already run, the next sample puts it right.
fn sample_returning<DB: Database>(pool: &sqlx::Pool<DB>, name: &str) {
    record(pool, name, pool.num_idle() + 1);
}

fn record<DB: Database>(pool: &sqlx::Pool<DB>, name: &str, idle: usize) {
    let size = pool.size() as usize;
    let idle = idle.min(size);
    let max = pool.options().get_max_connections() as usize;
    crate::pool::record_counts(name, idle, size - idle, max);
}

impl<DB: Database> Clone for Pool<DB> {
    fn clone(&self) -> Self {
        Pool {
            inner: self.inner.clone(),
            target: Arc::clone(&self.target),
        }
    }
}

impl<DB: Database> Deref for Pool<DB> {
    type Target = sqlx::Pool<DB>;

    fn deref(&self) -> &sqlx::Pool<DB> {
        &self.inner
    }
}

impl<DB: Database> fmt::Debug for Pool<DB> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pool")
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

impl<'p, DB> Executor<'p> for &'p Pool<DB>
where
    DB: Database,
    for<'c> &'c mut DB::Connection: Executor<'c, Database = DB>,
{
    type Database = DB;

    fn fetch_many<'e, 'q: 'e, E>(self, query: E) -> BoxStream<'e, Result<Step<DB>, Error>>
    where
        'p: 'e,
        E: 'q + Execute<'q, DB>,
    {
        let (statement, query) = watch(query);
        Box::pin(async_stream::try_stream! {
            let mut leased = self.lease().await?;
            let call = QueryCall::start(&self.target, statement);
            let mut steps = Recorded::new(
                (&mut *leased.conn).fetch_many(query),
                call,
                is_row::<DB>,
                fail,
            );
            while let Some(step) = steps.try_next().await? {
                yield step;
            }
        })
    }

    fn fetch_optional<'e, 'q: 'e, E>(
        self,
        query: E,
    ) -> BoxFuture<'e, Result<Option<DB::Row>, Error>>
    where
        'p: 'e,
        E: 'q + Execute<'q, DB>,
    {
        let (statement, query) = watch(query);
        Box::pin(async move {
            let mut leased = self.lease().await?;
            let call = QueryCall::start(&self.target, statement);
            record_one(
                call,
                (&mut *leased.conn).fetch_optional(query),
                one_row,
                fail,
            )
            .await
        })
    }

    fn prepare_with<'e>(
        self,
        sql: SqlStr,
        parameters: &'e [DB::TypeInfo],
    ) -> BoxFuture<'e, Result<DB::Statement, Error>>
    where
        'p: 'e,
    {
        (&self.inner).prepare_with(sql, parameters)
    }

    fn describe<'e>(self, sql: SqlStr) -> BoxFuture<'e, Result<Describe<DB>, Error>>
    where
        'p: 'e,
    {
        (&self.inner).describe(sql)
    }
}

/// An instrumented connection out of a [`Pool`]. Run queries on
/// `&mut connection`. It dereferences to sqlx's
/// [`PoolConnection`].
pub struct Connection<DB: Database> {
    leased: Leased<DB>,
    target: Arc<Target>,
}

impl<DB: Database> Connection<DB> {
    /// Start a transaction on this connection. Its `BEGIN` is recorded as a
    /// call.
    pub async fn begin(&mut self) -> Result<Transaction<'_, DB>, Error> {
        let call = QueryCall::start(&self.target, statement(Some(analyze("BEGIN"))));
        let inner = record_one(call, (*self.leased.conn).begin(), no_rows, fail).await?;
        Ok(Transaction {
            inner,
            target: Arc::clone(&self.target),
            _lease: None,
        })
    }
}

impl<DB: Database> Deref for Connection<DB> {
    type Target = PoolConnection<DB>;

    fn deref(&self) -> &PoolConnection<DB> {
        &self.leased.conn
    }
}

impl<DB: Database> DerefMut for Connection<DB> {
    fn deref_mut(&mut self) -> &mut PoolConnection<DB> {
        &mut self.leased.conn
    }
}

impl<DB: Database> fmt::Debug for Connection<DB> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Connection")
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

impl<'c, DB> Executor<'c> for &'c mut Connection<DB>
where
    DB: Database,
    for<'x> &'x mut DB::Connection: Executor<'x, Database = DB>,
{
    type Database = DB;

    fn fetch_many<'e, 'q: 'e, E>(self, query: E) -> BoxStream<'e, Result<Step<DB>, Error>>
    where
        'c: 'e,
        E: 'q + Execute<'q, DB>,
    {
        let (statement, query) = watch(query);
        let call = QueryCall::start(&self.target, statement);
        Box::pin(Recorded::new(
            (&mut *self.leased.conn).fetch_many(query),
            call,
            is_row::<DB>,
            fail,
        ))
    }

    fn fetch_optional<'e, 'q: 'e, E>(
        self,
        query: E,
    ) -> BoxFuture<'e, Result<Option<DB::Row>, Error>>
    where
        'c: 'e,
        E: 'q + Execute<'q, DB>,
    {
        let (statement, query) = watch(query);
        let call = QueryCall::start(&self.target, statement);
        Box::pin(record_one(
            call,
            (&mut *self.leased.conn).fetch_optional(query),
            one_row,
            fail,
        ))
    }

    fn prepare_with<'e>(
        self,
        sql: SqlStr,
        parameters: &'e [DB::TypeInfo],
    ) -> BoxFuture<'e, Result<DB::Statement, Error>>
    where
        'c: 'e,
    {
        (&mut *self.leased.conn).prepare_with(sql, parameters)
    }

    fn describe<'e>(self, sql: SqlStr) -> BoxFuture<'e, Result<Describe<DB>, Error>>
    where
        'c: 'e,
    {
        (&mut *self.leased.conn).describe(sql)
    }
}

/// An instrumented transaction, from [`Pool::begin`] or
/// [`Connection::begin`]. Run queries on `&mut transaction`. Like sqlx's,
/// dropping it without committing rolls it back, but that rollback isn't
/// recorded. It dereferences to sqlx's [`Transaction`](sqlx::Transaction).
pub struct Transaction<'c, DB: Database> {
    inner: sqlx::Transaction<'c, DB>,
    target: Arc<Target>,
    // After `inner`, so the connection is back in the pool when it ends.
    _lease: Option<Lease>,
}

impl<DB: Database> Transaction<'_, DB> {
    /// Commit, recorded as a call.
    pub async fn commit(self) -> Result<(), Error> {
        let call = QueryCall::start(&self.target, statement(Some(analyze("COMMIT"))));
        record_one(call, self.inner.commit(), no_rows, fail).await
    }

    /// Roll back, recorded as a call.
    pub async fn rollback(self) -> Result<(), Error> {
        let call = QueryCall::start(&self.target, statement(Some(analyze("ROLLBACK"))));
        record_one(call, self.inner.rollback(), no_rows, fail).await
    }
}

impl<'c, DB: Database> Deref for Transaction<'c, DB> {
    type Target = sqlx::Transaction<'c, DB>;

    fn deref(&self) -> &sqlx::Transaction<'c, DB> {
        &self.inner
    }
}

impl<'c, DB: Database> DerefMut for Transaction<'c, DB> {
    fn deref_mut(&mut self) -> &mut sqlx::Transaction<'c, DB> {
        &mut self.inner
    }
}

impl<DB: Database> fmt::Debug for Transaction<'_, DB> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Transaction")
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

impl<'t, DB> Executor<'t> for &'t mut Transaction<'_, DB>
where
    DB: Database,
    for<'x> &'x mut DB::Connection: Executor<'x, Database = DB>,
{
    type Database = DB;

    fn fetch_many<'e, 'q: 'e, E>(self, query: E) -> BoxStream<'e, Result<Step<DB>, Error>>
    where
        't: 'e,
        E: 'q + Execute<'q, DB>,
    {
        let (statement, query) = watch(query);
        let call = QueryCall::start(&self.target, statement);
        Box::pin(Recorded::new(
            (&mut *self.inner).fetch_many(query),
            call,
            is_row::<DB>,
            fail,
        ))
    }

    fn fetch_optional<'e, 'q: 'e, E>(
        self,
        query: E,
    ) -> BoxFuture<'e, Result<Option<DB::Row>, Error>>
    where
        't: 'e,
        E: 'q + Execute<'q, DB>,
    {
        let (statement, query) = watch(query);
        let call = QueryCall::start(&self.target, statement);
        Box::pin(record_one(
            call,
            (&mut *self.inner).fetch_optional(query),
            one_row,
            fail,
        ))
    }

    fn prepare_with<'e>(
        self,
        sql: SqlStr,
        parameters: &'e [DB::TypeInfo],
    ) -> BoxFuture<'e, Result<DB::Statement, Error>>
    where
        't: 'e,
    {
        (&mut *self.inner).prepare_with(sql, parameters)
    }

    fn describe<'e>(self, sql: SqlStr) -> BoxFuture<'e, Result<Describe<DB>, Error>>
    where
        't: 'e,
    {
        (&mut *self.inner).describe(sql)
    }
}
