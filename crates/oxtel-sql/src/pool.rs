//! The conventions' connection pool metrics, for the pools this crate
//! wraps: `db.client.connection.*`, labelled with
//! `db.client.connection.pool.name`. They're still in development in the
//! conventions.

use std::sync::Arc;
use std::time::Instant;

const POOL_NAME: &str = "db.client.connection.pool.name";

/// A pool's name for its metrics when it has none of its own, as the
/// conventions suggest: `server.address:server.port/db.namespace`, leaving
/// out what isn't known, or the database system if nothing is.
pub(crate) fn default_name(
    system: &str,
    namespace: Option<&str>,
    server: Option<(&str, u16)>,
) -> String {
    let mut name = match server {
        Some((address, port)) => format!("{address}:{port}"),
        None => String::new(),
    };
    if let Some(namespace) = namespace {
        name.push('/');
        name.push_str(namespace);
    }
    if name.is_empty() {
        name.push_str(system);
    }
    name
}

/// Set `db.client.connection.count` by state, and `db.client.connection.max`.
pub(crate) fn record_counts(name: &str, idle: usize, used: usize, max: usize) {
    let count = |state: &'static str, n: usize| {
        metrics::gauge!(
            "db.client.connection.count",
            POOL_NAME => name.to_string(),
            "db.client.connection.state" => state
        )
        .set(n as f64);
    };
    count("idle", idle);
    count("used", used);
    metrics::gauge!("db.client.connection.max", POOL_NAME => name.to_string()).set(max as f64);
}

/// Record how long a caller waited for a connection, and whether the wait
/// timed out.
pub(crate) fn record_wait(name: &str, started: Instant, timed_out: bool) {
    metrics::histogram!(
        "db.client.connection.wait_time",
        POOL_NAME => name.to_string()
    )
    .record(started.elapsed().as_secs_f64());
    if timed_out {
        metrics::counter!(
            "db.client.connection.timeouts",
            POOL_NAME => name.to_string()
        )
        .increment(1);
    }
}

/// Counts a caller in `db.client.connection.pending_requests` while it
/// waits, so a cancelled wait is uncounted too.
pub(crate) struct Waiting(metrics::Gauge);

impl Waiting {
    pub(crate) fn start(name: &str) -> Self {
        let gauge = metrics::gauge!(
            "db.client.connection.pending_requests",
            POOL_NAME => name.to_string()
        );
        gauge.increment(1.0);
        Waiting(gauge)
    }
}

impl Drop for Waiting {
    fn drop(&mut self) {
        self.0.decrement(1.0);
    }
}

/// A connection's time out of its pool: `db.client.connection.use_time`,
/// recorded when it's dropped, followed by `after`, which records the
/// pool's counts once the connection is back.
pub(crate) struct Lease {
    name: Arc<str>,
    since: Instant,
    after: Option<Box<dyn FnOnce() + Send + Sync>>,
}

impl Lease {
    pub(crate) fn start(name: &Arc<str>, after: impl FnOnce() + Send + Sync + 'static) -> Self {
        Lease {
            name: Arc::clone(name),
            since: Instant::now(),
            after: Some(Box::new(after)),
        }
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        metrics::histogram!(
            "db.client.connection.use_time",
            POOL_NAME => self.name.to_string()
        )
        .record(self.since.elapsed().as_secs_f64());
        if let Some(after) = self.after.take() {
            after();
        }
    }
}

/// Describe the pool metrics to the current recorder.
pub(crate) fn describe_metrics() {
    metrics::describe_gauge!(
        "db.client.connection.count",
        "The number of connections that are currently in state described by the `state` attribute."
    );
    metrics::describe_gauge!(
        "db.client.connection.max",
        "The maximum number of open connections allowed."
    );
    metrics::describe_gauge!(
        "db.client.connection.pending_requests",
        "The number of current pending requests for an open connection."
    );
    metrics::describe_histogram!(
        "db.client.connection.wait_time",
        metrics::Unit::Seconds,
        "The time it took to obtain an open connection from the pool."
    );
    metrics::describe_histogram!(
        "db.client.connection.use_time",
        metrics::Unit::Seconds,
        "The time between borrowing a connection and returning it to the pool."
    );
    metrics::describe_counter!(
        "db.client.connection.timeouts",
        "The number of connection timeouts that have occurred trying to obtain a connection from the pool."
    );
}
