//! Testing your telemetry: unit tests that check the span and the metrics
//! a function records, by keeping them in memory instead of sending them
//! anywhere. Each test captures only its own thread's telemetry, so tests
//! run in parallel as usual.
//!
//! ```bash
//! cargo test -p oxtel-examples --example testing
//! ```
//!
//! A program of your own needs these. The in-memory exporters come with
//! oxtel, as `oxtel::sdk`; the tests need only `tracing-subscriber` beside
//! it, to record spans without installing anything globally:
//!
//! ```toml
//! [dependencies]
//! oxtel = "0.3"
//! metrics = "0.24"
//! tracing = "0.1"
//!
//! [dev-dependencies]
//! tracing-subscriber = { version = "0.3", default-features = false, features = ["registry"] }
//! ```

// ANCHOR: code
/// The total of an order, in cents. An order needs at least one item.
///
/// Each call is a span, `place_order`, marked as an error when the order is
/// rejected, and counted in `orders` by outcome.
#[tracing::instrument(
    skip(prices),
    fields(order.items = prices.len(), otel.status_code = tracing::field::Empty)
)]
fn place_order(prices: &[u32]) -> Result<u32, String> {
    if prices.is_empty() {
        tracing::Span::current().record("otel.status_code", "error");
        metrics::counter!("orders", "outcome" => "rejected").increment(1);
        return Err("an order needs at least one item".into());
    }
    let total = prices.iter().sum();
    tracing::info!(total, "placed");
    metrics::counter!("orders", "outcome" => "placed").increment(1);
    Ok(total)
}
// ANCHOR_END: code

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _telemetry = oxtel::init()?;
    place_order(&[450, 1200])?;
    Ok(())
}

// ANCHOR: tests
#[cfg(test)]
mod tests {
    use oxtel::sdk::KeyValue;
    use oxtel::sdk::metrics::{MeterProvider, MetricFamily, PointValue};
    use oxtel::sdk::trace::{InMemorySpanExporter, Sampler, SpanData, Status, TracerProvider};
    use tracing_subscriber::layer::SubscriberExt;

    use super::place_order;

    /// Runs `f` with this thread's spans and metrics kept in memory, and
    /// returns what it returned, the spans it ended and the metrics it
    /// recorded.
    fn capture<T>(f: impl FnOnce() -> T) -> (T, Vec<SpanData>, Vec<MetricFamily>) {
        let spans = InMemorySpanExporter::default();
        let traces = TracerProvider::builder()
            .with_sampler(Sampler::AlwaysOn)
            .with_simple_exporter(spans.clone())
            .build();
        let meters = MeterProvider::builder().build();
        let subscriber = tracing_subscriber::registry().with(traces.layer());
        let result = tracing::subscriber::with_default(subscriber, || {
            metrics::with_local_recorder(&meters.recorder(), f)
        });
        (result, spans.finished_spans(), meters.collect())
    }

    /// The `orders` counter's value for `outcome`.
    fn orders(metrics: &[MetricFamily], outcome: &str) -> Option<u64> {
        let family = metrics.iter().find(|m| m.name == "orders")?;
        let point = family
            .points
            .iter()
            .find(|p| p.attributes == [("outcome".to_string(), outcome.to_string())])?;
        match point.value {
            PointValue::Counter(n) => Some(n),
            _ => None,
        }
    }

    #[test]
    fn a_placed_order_is_a_span_and_a_count() {
        let (result, spans, metrics) = capture(|| place_order(&[450, 1200]));
        assert_eq!(result, Ok(1650));

        assert_eq!(spans.len(), 1);
        let span = &spans[0];
        assert_eq!(span.name, "place_order");
        assert!(span.attributes.contains(&KeyValue::new("order.items", 2)));
        assert_eq!(span.status, Status::Unset);
        // The log line written inside the span is kept as one of its events.
        let placed = &span.events[0];
        assert_eq!(placed.name, "placed");
        assert!(placed.attributes.contains(&KeyValue::new("total", 1650)));

        assert_eq!(orders(&metrics, "placed"), Some(1));
        assert_eq!(orders(&metrics, "rejected"), None);
    }

    #[test]
    fn a_rejected_order_is_an_error() {
        let (result, spans, metrics) = capture(|| place_order(&[]));
        assert!(result.is_err());

        assert!(matches!(spans[0].status, Status::Error { .. }));
        assert_eq!(orders(&metrics, "rejected"), Some(1));
        assert_eq!(orders(&metrics, "placed"), None);
    }
}
// ANCHOR_END: tests
