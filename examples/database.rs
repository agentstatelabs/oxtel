//! Database access with rusqlite: every statement a connection runs gets a
//! span named for what it does, such as `INSERT orders`, and its duration
//! as a metric. The statement's text goes on the span with every value
//! replaced by `?`, so no customer data reaches telemetry.
//!
//! ```bash
//! cargo run -p oxtel-examples --example database
//! OTEL_TRACES_EXPORTER=console cargo run -p oxtel-examples --example database   # and the spans, on standard output
//! ```
//!
//! A program of your own needs:
//!
//! ```toml
//! [dependencies]
//! oxtel = "0.4"
//! oxtel-sql = { version = "0.4", features = ["rusqlite"] }
//! rusqlite = { version = "0.39", features = ["bundled"] }
//! tracing = "0.1"
//! ```

use rusqlite::Connection;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // First, so the statements' duration metric finds oxtel's recorder.
    // Keep the guard until main returns.
    let _telemetry = oxtel::init()?;

    let db = Connection::open_in_memory()?;
    // One line per connection: every statement it runs from here on gets a
    // span and a duration.
    oxtel_sql::rusqlite::instrument(&db);

    db.execute(
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, item TEXT NOT NULL, cents INTEGER NOT NULL)",
        (),
    )?;
    for (item, cents) in [("tea", 450), ("cups", 1200), ("tea", 450)] {
        db.execute(
            "INSERT INTO orders (item, cents) VALUES (?1, ?2)",
            (item, cents),
        )?;
    }

    // The statements run inside this span become its children, so a trace
    // shows what the report ran and how long each statement took.
    let report = tracing::info_span!("report");
    let _entered = report.enter();
    let (orders, cents): (i64, i64) = db.query_row(
        "SELECT count(*), sum(cents) FROM orders WHERE item = 'tea'",
        (),
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    tracing::info!(orders, cents, "tea sold");
    Ok(())
}
