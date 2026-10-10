//! Periodic export: when metrics are pushed, what each export carries, and
//! what happens on failure and shutdown.

use std::time::{Duration, Instant};

use oxtel_sdk::metrics::{
    ExportError, InMemoryMetricExporter, MeterProvider, MetricExporter, MetricFamily,
    PeriodicReaderConfig, PointValue,
};
use oxtel_sdk::{Resource, Value};

const HOURLY: PeriodicReaderConfig = PeriodicReaderConfig {
    interval: Duration::from_secs(3600),
    timeout: Duration::from_secs(5),
};

fn provider(exporter: impl MetricExporter, config: PeriodicReaderConfig) -> MeterProvider {
    MeterProvider::builder()
        .with_resource(
            Resource::builder()
                .without_env()
                .with_service_name("billing")
                .build(),
        )
        .with_periodic_exporter_config(exporter, config)
        .build()
}

fn counter_total(export: &[MetricFamily], name: &str) -> Option<u64> {
    export
        .iter()
        .find(|m| m.name == name)
        .and_then(|m| match m.points[0].value {
            PointValue::Counter(v) => Some(v),
            _ => None,
        })
}

#[test]
fn flush_and_shutdown_export_cumulative_totals() {
    let exporter = InMemoryMetricExporter::default();
    let meters = provider(exporter.clone(), HOURLY);
    let recorder = meters.recorder();
    metrics::with_local_recorder(&recorder, || {
        metrics::counter!("invoices.sent").increment(2);
        metrics::histogram!("invoice.amount").record(12.5);
    });
    meters.force_flush().unwrap();
    assert_eq!(exporter.exports().len(), 1);
    assert_eq!(
        counter_total(&exporter.last().unwrap(), "invoices.sent"),
        Some(2)
    );

    metrics::with_local_recorder(&recorder, || {
        metrics::counter!("invoices.sent").increment(3);
    });
    meters.shutdown().unwrap();
    let exports = exporter.exports();
    assert_eq!(exports.len(), 2, "shutdown exports once more");
    assert_eq!(
        counter_total(&exports[1], "invoices.sent"),
        Some(5),
        "cumulative"
    );
    assert_eq!(meters.stats().exported, 4, "two points per export");
    assert_eq!(
        exporter.resource().unwrap().get("service.name"),
        Some(&Value::from("billing"))
    );
    assert_eq!(meters.force_flush(), Err(ExportError::Shutdown));
    assert_eq!(meters.shutdown(), Ok(()), "shutting down twice is harmless");
    assert_eq!(exporter.exports().len(), 2);
}

#[test]
fn exports_happen_every_interval() {
    let exporter = InMemoryMetricExporter::default();
    let meters = provider(
        exporter.clone(),
        PeriodicReaderConfig {
            interval: Duration::from_millis(30),
            timeout: Duration::from_secs(5),
        },
    );
    metrics::with_local_recorder(&meters.recorder(), || {
        metrics::gauge!("queue.depth").set(7.0);
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while exporter.exports().len() < 3 {
        assert!(Instant::now() < deadline, "the timer didn't fire");
        std::thread::sleep(Duration::from_millis(10));
    }
    let times: Vec<_> = exporter.exports().iter().map(|e| e[0].time).collect();
    assert!(times.windows(2).all(|w| w[0] <= w[1]));
}

#[test]
fn nothing_recorded_means_nothing_exported() {
    let exporter = InMemoryMetricExporter::default();
    let meters = provider(exporter.clone(), HOURLY);
    meters.force_flush().unwrap();
    meters.shutdown().unwrap();
    assert!(exporter.exports().is_empty());
}

#[test]
fn the_last_clone_to_go_exports_and_stops() {
    let exporter = InMemoryMetricExporter::default();
    let meters = provider(exporter.clone(), HOURLY);
    metrics::with_local_recorder(&meters.recorder(), || {
        metrics::counter!("invoices.sent").increment(1);
    });
    let clone = meters.clone();
    drop(meters);
    assert!(exporter.exports().is_empty(), "a clone is still alive");
    drop(clone);
    assert_eq!(exporter.exports().len(), 1);
}

struct Failing;

impl MetricExporter for Failing {
    fn export(&self, _: &[MetricFamily], _: &Resource) -> Result<(), ExportError> {
        Err(ExportError::Transport("connection refused".into()))
    }
}

#[test]
fn failed_exports_are_reported_and_counted() {
    let meters = provider(Failing, HOURLY);
    metrics::with_local_recorder(&meters.recorder(), || {
        metrics::counter!("a").increment(1);
        metrics::counter!("b", "k" => "1").increment(1);
        metrics::counter!("b", "k" => "2").increment(1);
    });
    assert_eq!(
        meters.force_flush(),
        Err(ExportError::Transport("connection refused".into()))
    );
    let stats = meters.stats();
    assert_eq!((stats.exported, stats.export_failed), (0, 3));
}

#[test]
fn every_exporter_gets_every_export() {
    let (first, second) = (
        InMemoryMetricExporter::default(),
        InMemoryMetricExporter::default(),
    );
    let meters = MeterProvider::builder()
        .with_resource(Resource::builder().without_env().build())
        .with_periodic_exporter_config(first.clone(), HOURLY)
        .with_periodic_exporter_config(second.clone(), HOURLY)
        .build();
    metrics::with_local_recorder(&meters.recorder(), || {
        metrics::counter!("invoices.sent").increment(1);
    });
    meters.force_flush().unwrap();
    assert_eq!((first.exports().len(), second.exports().len()), (1, 1));
}

#[test]
fn the_reader_defaults_follow_the_specification() {
    let d = PeriodicReaderConfig::default();
    assert_eq!(d.interval, Duration::from_secs(60));
    assert_eq!(d.timeout, Duration::from_secs(30));
}
