//! Help for testing what telemetry is recorded under which names.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::metrics::{MetricFamily, MetricKind};
use crate::trace::{SpanData, SpanKind};

/// The names telemetry was recorded under, as text: each span's scope (the
/// library that made it), kind and name with its attribute keys, and each
/// metric's instrument, name and unit with its label keys, all sorted.
/// Values are left out, so the text changes only when a name does.
///
/// ```text
/// span [oxtel_tower] server GET /items/{id}
///   http.request.method
///   http.route
/// metric histogram http.server.request.duration s
///   http.request.method
///   http.route
/// ```
///
/// Span fields named `otel.*`, such as `otel.name`, steer the span rather
/// than describe it, so they're left out. Kept in a file with
/// [`assert_recorded_names`], it shows when a change renames, adds or drops
/// something a backend would see.
pub fn recorded_names(spans: &[SpanData], metrics: &[MetricFamily]) -> String {
    let mut span_names: BTreeMap<(&str, &str, &str), BTreeSet<&str>> = BTreeMap::new();
    for span in spans {
        let keys = span_names
            .entry((
                span.scope.name.as_ref(),
                kind_name(span.kind),
                span.name.as_ref(),
            ))
            .or_default();
        for kv in &span.attributes {
            if !kv.key.starts_with("otel.") {
                keys.insert(kv.key.as_ref());
            }
        }
    }
    let mut metric_names: BTreeMap<&str, (&str, Option<&str>, BTreeSet<&str>)> = BTreeMap::new();
    for family in metrics {
        let (_, _, keys) = metric_names.entry(family.name.as_str()).or_insert((
            instrument_name(family.kind),
            family.unit.as_deref(),
            BTreeSet::new(),
        ));
        for point in &family.points {
            for (key, _) in &point.attributes {
                keys.insert(key.as_str());
            }
        }
    }
    let mut out = String::new();
    for ((scope, kind, name), keys) in &span_names {
        out.push_str(&format!("span [{scope}] {kind} {name}\n"));
        for key in keys {
            out.push_str(&format!("  {key}\n"));
        }
    }
    for (name, (instrument, unit, keys)) in &metric_names {
        match unit {
            Some(unit) => out.push_str(&format!("metric {instrument} {name} {unit}\n")),
            None => out.push_str(&format!("metric {instrument} {name}\n")),
        }
        for key in keys {
            out.push_str(&format!("  {key}\n"));
        }
    }
    out
}

/// Check that [`recorded_names`] matches the file at `path`, so a test
/// fails when a name changes. With `rewrite`, write the file instead, to
/// accept the change: a test might pass whether an environment variable is
/// set.
///
/// ```no_run
/// # let (spans, metrics) = (Vec::new(), Vec::new());
/// oxtel_sdk::testing::assert_recorded_names(
///     concat!(env!("CARGO_MANIFEST_DIR"), "/tests/recorded/names.txt"),
///     &spans,
///     &metrics,
///     std::env::var_os("REWRITE_RECORDED").is_some(),
/// );
/// ```
pub fn assert_recorded_names(
    path: impl AsRef<Path>,
    spans: &[SpanData],
    metrics: &[MetricFamily],
    rewrite: bool,
) {
    let path = path.as_ref();
    let recorded = recorded_names(spans, metrics);
    if rewrite {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).expect("the directory can be made");
        }
        std::fs::write(path, &recorded).expect("the file can be written");
        return;
    }
    let kept = std::fs::read_to_string(path).unwrap_or_default();
    assert!(
        kept == recorded,
        "{} doesn't match what was recorded; rewrite it if the change is meant.\nRecorded:\n{recorded}",
        path.display()
    );
}

fn kind_name(kind: SpanKind) -> &'static str {
    match kind {
        SpanKind::Internal => "internal",
        SpanKind::Server => "server",
        SpanKind::Client => "client",
        SpanKind::Producer => "producer",
        SpanKind::Consumer => "consumer",
    }
}

fn instrument_name(kind: MetricKind) -> &'static str {
    match kind {
        MetricKind::Counter => "counter",
        MetricKind::Gauge => "gauge",
        MetricKind::Histogram => "histogram",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Resource;
    use crate::metrics::MeterProvider;
    use crate::trace::{InMemorySpanExporter, Sampler, TracerProvider};
    use tracing_subscriber::layer::SubscriberExt;

    #[test]
    fn names_are_kept_and_values_left_out() {
        let spans = InMemorySpanExporter::default();
        let traces = TracerProvider::builder()
            .with_resource(Resource::builder().without_env().build())
            .with_sampler(Sampler::AlwaysOn)
            .with_simple_exporter(spans.clone())
            .build();
        let meters = MeterProvider::builder().build();
        let subscriber = tracing_subscriber::registry().with(traces.layer());
        tracing::subscriber::with_default(subscriber, || {
            metrics::with_local_recorder(&meters.recorder(), || {
                for id in [1, 2] {
                    let span = tracing::info_span!("job", job.id = id, otel.kind = "server");
                    span.in_scope(|| {
                        metrics::counter!("jobs.done", "outcome" => "ok").increment(1);
                    });
                }
            });
        });
        assert_eq!(
            recorded_names(&spans.finished_spans(), &meters.collect()),
            "span [oxtel_sdk] server job\n  job.id\nmetric counter jobs.done\n  outcome\n"
        );
    }
}
