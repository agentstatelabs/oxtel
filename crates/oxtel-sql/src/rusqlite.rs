//! [rusqlite](https://docs.rs/rusqlite): a span and
//! `db.client.operation.duration` for every statement a connection runs.
//!
//! It uses SQLite's own trace hooks, so nothing about how the program calls
//! rusqlite changes. SQLite reports when a statement starts running and when
//! it finishes, with its duration; the span runs between the two, and the
//! metric records SQLite's duration.
//!
//! SQLite's hooks don't say whether a statement failed, so these spans and
//! measurements carry no `error.type`. The program's own error handling sees
//! failures as before.

use std::cell::RefCell;
use std::sync::Arc;

use ::rusqlite::Connection;
use ::rusqlite::trace::{TraceEvent, TraceEventCodes};

use crate::record::{Analysis, Call, analyze, describe_metrics};

/// Statements open at once on one thread past which the oldest is dropped,
/// in case SQLite never reports one finishing.
const MAX_OPEN: usize = 32;

struct Open {
    sql: String,
    analysis: Arc<Analysis>,
    /// Held until the statement finishes: dropping it ends the span.
    _span: tracing::Span,
}

thread_local! {
    static OPEN: RefCell<Vec<Open>> = const { RefCell::new(Vec::new()) };
}

/// Record a client span and `db.client.operation.duration` for every
/// statement `connection` runs, named by its query summary (`SELECT
/// users`), with `db.system.name` `sqlite` and, unless turned off with
/// [`record_query_text`](crate::record_query_text), its text with literals
/// replaced.
///
/// This replaces any trace callback already set on the connection with
/// `trace_v2`. Call it once per connection, after `oxtel::init()` so the
/// metric's unit reaches the recorder.
///
/// ```no_run
/// let conn = rusqlite::Connection::open("app.db")?;
/// oxtel_sql::rusqlite::instrument(&conn);
/// conn.execute("INSERT INTO kv (k, v) VALUES (?1, ?2)", ("a", 1))?;
/// # Ok::<(), rusqlite::Error>(())
/// ```
pub fn instrument(connection: &Connection) {
    describe_metrics();
    connection.trace_v2(
        TraceEventCodes::SQLITE_TRACE_STMT | TraceEventCodes::SQLITE_TRACE_PROFILE,
        Some(on_event),
    );
}

const SQLITE: &str = "sqlite";

fn on_event(event: TraceEvent<'_>) {
    match event {
        TraceEvent::Stmt(statement, text) => {
            // A trigger's subprogram starting, inside a statement already open.
            if text.starts_with("--") {
                return;
            }
            let sql = statement.sql();
            let analysis = analyze(&sql);
            let call = Call {
                system: SQLITE,
                namespace: None,
                server: None,
                analysis: &analysis,
            };
            let span = call.span();
            OPEN.with(|open| {
                let mut open = open.borrow_mut();
                if open.len() >= MAX_OPEN {
                    open.remove(0);
                }
                open.push(Open {
                    sql: sql.into_owned(),
                    analysis,
                    _span: span,
                });
            });
        }
        TraceEvent::Profile(statement, duration) => {
            let sql = statement.sql();
            let finished = OPEN.with(|open| {
                let mut open = open.borrow_mut();
                let i = open.iter().rposition(|o| o.sql == sql)?;
                Some(open.remove(i))
            });
            let analysis = match &finished {
                Some(open) => Arc::clone(&open.analysis),
                None => analyze(&sql),
            };
            let call = Call {
                system: SQLITE,
                namespace: None,
                server: None,
                analysis: &analysis,
            };
            call.record_duration(duration.as_secs_f64());
            // Dropping the span ends it.
            drop(finished);
        }
        _ => {}
    }
}
