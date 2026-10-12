---
title: Databases
description: Spans and duration metrics for every query, from SQLite, PostgreSQL and sqlx, and how to choose between sqlx's two options.
---

:::note
Database telemetry arrives with oxtel 0.3, which isn't released yet: the `oxtel-sql` crate, including its sqlx wrapper, and sqlx's automatic spans in `oxtel::init()`. Until then, they're on the repository's main branch.
:::

Every query can get a span, named for what it does (`SELECT users`, `INSERT orders`), and a `db.client.operation.duration` measurement. Spans sit inside the span of the request that made the query, so a trace shows which queries a request ran and how long each took.

The query's text is recorded too, with every literal replaced by `?`, so values such as names or ids never reach your telemetry: `SELECT name FROM users WHERE id = 7` becomes `SELECT name FROM users WHERE id = ?`. `oxtel_sql::record_query_text(false)` turns the text off altogether.

## Which database client

| You use | What to do | What you get |
|---|---|---|
| **rusqlite** (SQLite) | One line per connection: `oxtel_sql::rusqlite::instrument(&conn)`, with oxtel-sql's `rusqlite` feature | Every statement, timed by SQLite itself. No failure details: SQLite's hooks don't report them. |
| **tokio-postgres**, or a **deadpool-postgres** pool | Wrap the client where you connect: `oxtel_sql::postgres::Client::new(client, &config)`, or the pool with `oxtel_sql::postgres::Pool::new(pool, &config)`. Features `postgres` and `deadpool-postgres` | Everything: the database's name, host and port, and for a failed query its error code. A pool also gets connection pool metrics. |
| **sqlx** 0.8 or 0.9 | Nothing, with `oxtel::init()`. Or wrap the pool for everything: `oxtel_sql::sqlx::v0_9::Pool::new(pool)` (or `v0_8`), with the feature `sqlx-0.9` (or `sqlx-0.8`) | See the next section |

## sqlx: automatic, or complete

sqlx writes a log event as each query finishes, saying what the query was, how many rows it returned and how long it took. `oxtel::init()` turns those events into database spans and metrics automatically: there's nothing to add to your code.

That's enough to see which queries ran and how long they took. But the event doesn't carry everything, so the automatic spans have gaps. Wrapping sqlx's pool with oxtel-sql closes them, at the cost of running your queries through the wrapper: `.fetch_all(&pool)` on the wrapped pool, `.execute(&mut tx)` on a transaction from its `begin`, and `&mut conn` on a connection from its `acquire`.

| | Automatic | Wrapped pool |
|---|---|---|
| Code to change | None | Wrap the pool where you create it, and run queries through it |
| A span named for each query, its text, its duration | Yes | Yes |
| Rows returned | Yes | Yes |
| Inside the request's span | Yes | Yes |
| Which database: its type, name, host and port | Only if you say, once, in code | Yes, from the connection |
| **Failed queries and their error codes** | **No** | Yes |
| Works when sqlx's statement logging is turned off | No | Yes |
| Covers queries made outside your code, such as by a library | Yes | Only queries through the wrapper |

What the automatic option can't do, and why:

- **It can't see failures.** sqlx writes the same event whether a query succeeded or failed, and doesn't include the error. A failed query looks like one that returned nothing, and its span isn't marked as an error. If you want dashboards or alerts on database errors, use the wrapped pool.
- **It doesn't know which database.** The event doesn't say. Spans say `other_sql` unless you name the database once, in code: `oxtel::builder().with_mapping(oxtel::mappings::Sqlx::new().with_system("postgresql").with_namespace("shop"))`. A program with two databases can't tell their queries apart.
- **It needs sqlx's statement logging.** It's on by default, at `debug`. If your code turns it off for a connection (`log_statements(LevelFilter::Off)`), there's nothing to read; if it moves it to `trace`, add `sqlx::query=trace` to the traces filter. oxtel reads the events without putting them in your logs, so leaving it on doesn't make your logs noisier.
- **It reads a log line, not an interface.** sqlx could change its event in any release. oxtel is tested against sqlx 0.8 and 0.9, and leaves an event it doesn't recognise alone rather than record it wrong.

What wrapping costs:

- **Queries have to go through it.** A query run on sqlx's own types, such as `&*pool` or the connection inside a transaction, isn't recorded by the wrapper (the automatic option still records it).
- **A transaction's `BEGIN` includes waiting for a connection,** since sqlx gets the connection and begins in one step.
- **With sqlx 0.9, it needs sqlx's `macros` feature,** which is on unless you turned sqlx's default features off.

The two never count a query twice: when a query goes through the wrapped pool, the automatic option leaves it alone. So you can start with the automatic option and wrap the pool later, in all of your code or only part of it.

**Choosing:** start with the automatic option. Wrap the pool when you need failed queries in your telemetry, or when one program talks to more than one database.

To turn the automatic option off, set `OXTEL_MAPPINGS=none`, or call `Builder::without_mappings`.

## What's recorded

| | |
|---|---|
| Span name | The query's summary, such as `SELECT users` or `INSERT orders SELECT carts` |
| `db.system.name` | `sqlite`, `postgresql`, or for sqlx's automatic spans `other_sql` unless named |
| `db.query.summary`, `db.query.text` | The summary, and the text with literals replaced by `?` |
| `db.namespace`, `server.address`, `server.port` | The database's name and server, where known |
| `db.response.status_code`, `error.type` | For a failed query, through tokio-postgres or sqlx's wrapped pool: the database's error code, such as `23505` for a duplicate key in PostgreSQL |
| `db.client.operation.duration` | Each query's duration in seconds, by database and summary |
| `db.client.connection.*` | For a wrapped deadpool or sqlx pool: connections in use and idle, waiting callers, wait and use times, timeouts |
