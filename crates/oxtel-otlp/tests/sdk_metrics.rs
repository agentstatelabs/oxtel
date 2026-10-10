//! oxtel's own metrics, end to end: pipelines exporting over OTLP report on
//! themselves through the global `metrics` recorder. In its own test binary
//! because it installs that recorder and turns the metrics on for the whole
//! process.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::time::Duration;

use oxtel_otlp::OtlpHttpExporter;
use oxtel_sdk::Resource;
use oxtel_sdk::export::BatchConfig;
use oxtel_sdk::metrics::{MeterProvider, MetricFamily, PeriodicReaderConfig, PointValue};
use oxtel_sdk::trace::{Sampler, TracerProvider};
use tracing_subscriber::layer::SubscriberExt;

/// A receiver answering each request with the next status. Returns its
/// base URL and port.
fn receiver(statuses: Vec<u16>) -> (String, u16) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for status in statuses {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut len = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let line = line.trim_end();
                if line.is_empty() {
                    break;
                }
                if let Some((k, v)) = line.split_once(':')
                    && k.eq_ignore_ascii_case("content-length")
                {
                    len = v.trim().parse().unwrap();
                }
            }
            reader.read_exact(&mut vec![0; len]).unwrap();
            let mut stream = stream;
            write!(
                stream,
                "HTTP/1.1 {status} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
        }
    });
    (format!("http://127.0.0.1:{port}"), port)
}

fn resource() -> Resource {
    Resource::builder().without_env().build()
}

/// The value of `name`'s series whose attributes include all of `want`,
/// and no `error.type` unless `want` has one: a counter's total, or a
/// histogram's count.
fn value(metrics: &[MetricFamily], name: &str, want: &[(&str, &str)]) -> Option<u64> {
    let family = metrics.iter().find(|m| m.name == name)?;
    family
        .points
        .iter()
        .find(|p| {
            let has = |key: &str| p.attributes.iter().any(|(k, _)| k == key);
            want.iter()
                .all(|(k, v)| p.attributes.iter().any(|(pk, pv)| pk == k && pv == v))
                && (want.iter().any(|(k, _)| *k == "error.type") || !has("error.type"))
        })
        .map(|p| match &p.value {
            PointValue::Counter(v) => *v,
            PointValue::Histogram(h) => h.count(),
            PointValue::Gauge(v) => *v as u64,
        })
}

#[test]
fn pipelines_report_on_themselves_when_asked() {
    let meters = MeterProvider::builder().with_resource(resource()).build();
    metrics::set_global_recorder(meters.recorder()).unwrap();

    let spans = |provider: &TracerProvider, n: usize| {
        let subscriber = tracing_subscriber::registry().with(provider.layer());
        tracing::subscriber::with_default(subscriber, || {
            for _ in 0..n {
                tracing::info_span!("work").in_scope(|| {});
            }
        });
    };
    let traces = |base: &str| {
        TracerProvider::builder()
            .with_resource(resource())
            .with_sampler(Sampler::AlwaysOn)
            .with_batch_exporter_config(
                OtlpHttpExporter::builder().with_endpoint(base).build(),
                BatchConfig {
                    scheduled_delay: Duration::from_secs(3600),
                    ..BatchConfig::default()
                },
            )
            .build()
    };

    // Off by default: an export records nothing.
    let (base, _) = receiver(vec![200]);
    let quiet = traces(&base);
    spans(&quiet, 2);
    quiet.force_flush().unwrap();
    assert!(
        meters
            .collect()
            .iter()
            .all(|m| !m.name.starts_with("otel.sdk.")),
        "{:?}",
        meters.collect()
    );

    oxtel_sdk::sdk_metrics::set_enabled(true);

    // Spans: three handed to the exporter and exported, then one dropped
    // after shutdown.
    let (base, port) = receiver(vec![200]);
    let provider = traces(&base);
    spans(&provider, 3);
    provider.force_flush().unwrap();
    provider.shutdown().unwrap();
    spans(&provider, 1);

    // Metrics: an export the receiver refuses.
    // Two refusals: the flush, and the final export when it's dropped.
    let (refusing, _) = receiver(vec![400, 400]);
    let pushed = MeterProvider::builder()
        .with_resource(resource())
        .with_periodic_exporter_config(
            OtlpHttpExporter::builder().with_endpoint(&refusing).build(),
            PeriodicReaderConfig {
                interval: Duration::from_secs(3600),
                timeout: Duration::from_secs(5),
            },
        )
        .build();
    metrics::with_local_recorder(&pushed.recorder(), || {
        metrics::counter!("jobs.done").increment(1);
        metrics::counter!("jobs.failed").increment(1);
    });
    assert!(pushed.force_flush().is_err());

    let collected = meters.collect();
    let processor = [("otel.component.type", "batching_span_processor")];
    assert_eq!(
        value(&collected, "otel.sdk.processor.span.processed", &processor),
        Some(3),
        "{collected:#?}"
    );
    assert_eq!(
        value(
            &collected,
            "otel.sdk.processor.span.processed",
            &[processor[0], ("error.type", "shutdown")]
        ),
        Some(1)
    );
    let port = port.to_string();
    let exporter = [
        ("otel.component.type", "otlp_http_span_exporter"),
        ("server.address", "127.0.0.1"),
        ("server.port", port.as_str()),
    ];
    assert_eq!(
        value(&collected, "otel.sdk.exporter.span.exported", &exporter),
        Some(3)
    );
    assert_eq!(
        value(
            &collected,
            "otel.sdk.exporter.operation.duration",
            &[
                exporter[0],
                exporter[2],
                ("http.response.status_code", "200")
            ]
        ),
        Some(1)
    );
    assert_eq!(
        value(
            &collected,
            "otel.sdk.exporter.metric_data_point.exported",
            &[
                ("otel.component.type", "otlp_http_metric_exporter"),
                ("error.type", "400")
            ]
        ),
        Some(2)
    );
    assert!(
        value(
            &collected,
            "otel.sdk.metric_reader.collection.duration",
            &[("otel.component.type", "periodic_metric_reader")]
        )
        .is_some_and(|n| n >= 1)
    );
    let duration = collected
        .iter()
        .find(|m| m.name == "otel.sdk.exporter.operation.duration")
        .unwrap();
    assert_eq!(duration.unit.as_deref(), Some("s"));
    let names: Vec<_> = collected
        .iter()
        .flat_map(|m| &m.points)
        .flat_map(|p| &p.attributes)
        .filter(|(k, _)| k == "otel.component.name")
        .map(|(_, v)| v.as_str())
        .collect();
    assert!(names.contains(&"otlp_http_span_exporter/1"), "{names:?}");
}
