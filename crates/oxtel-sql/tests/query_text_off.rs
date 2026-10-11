//! `record_query_text(false)` keeps the text off spans. Its own test binary,
//! because the setting is process-wide.

#![cfg(feature = "rusqlite")]

use oxtel_sdk::Resource;
use oxtel_sdk::trace::{InMemorySpanExporter, Sampler, TracerProvider};
use rusqlite::Connection;
use tracing_subscriber::layer::SubscriberExt;

#[test]
fn the_text_can_be_left_off() {
    let spans = InMemorySpanExporter::default();
    let traces = TracerProvider::builder()
        .with_resource(Resource::builder().without_env().build())
        .with_sampler(Sampler::AlwaysOn)
        .with_simple_exporter(spans.clone())
        .build();
    let _tracing =
        tracing::subscriber::set_default(tracing_subscriber::registry().with(traces.layer()));

    oxtel_sql::record_query_text(false);
    let conn = Connection::open_in_memory().unwrap();
    oxtel_sql::rusqlite::instrument(&conn);
    conn.execute_batch("CREATE TABLE t (v TEXT)").unwrap();

    let spans = spans.finished_spans();
    let span = spans.iter().find(|s| s.name == "CREATE TABLE t").unwrap();
    assert!(span.attributes.iter().all(|kv| kv.key != "db.query.text"));
    assert!(
        span.attributes
            .iter()
            .any(|kv| kv.key == "db.query.summary")
    );
}
