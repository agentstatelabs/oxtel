//! The cost of recording a measurement: oxtel's `metrics` recorder,
//! `metrics-exporter-prometheus` (the most used `metrics` backend), and the
//! official SDK.

use std::time::{Duration, Instant};

use criterion::{Criterion, criterion_group, criterion_main};
use opentelemetry::KeyValue;
use opentelemetry::metrics::MeterProvider as _;
use oxtel_bench::{otel_meters, oxtel_meters, oxtel_traces, prometheus_recorder};

/// The `metrics` macros, recording into `recorder`.
fn facade_cases(
    g: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    recorder: &dyn metrics::Recorder,
    name: &str,
    histogram: bool,
) {
    g.bench_function(name, |b| {
        metrics::with_local_recorder(recorder, || {
            if histogram {
                b.iter(|| metrics::histogram!("latency", "route" => "/items").record(0.042));
            } else {
                b.iter(|| metrics::counter!("requests", "route" => "/items").increment(1));
            }
        })
    });
}

fn per_call(c: &mut Criterion) {
    for histogram in [false, true] {
        let title = if histogram {
            "histogram, attributes on each call"
        } else {
            "counter, attributes on each call"
        };
        let mut g = c.benchmark_group(title);
        let oxtel = oxtel_meters(false).recorder();
        facade_cases(&mut g, &oxtel, "oxtel", histogram);
        let prometheus = prometheus_recorder();
        facade_cases(
            &mut g,
            &prometheus,
            "metrics-exporter-prometheus",
            histogram,
        );
        let provider = otel_meters();
        let meter = provider.meter("bench");
        let attrs = [KeyValue::new("route", "/items")];
        if histogram {
            let h = meter.f64_histogram("latency").build();
            g.bench_function("opentelemetry_sdk", |b| b.iter(|| h.record(0.042, &attrs)));
        } else {
            let counter = meter.u64_counter("requests").build();
            g.bench_function("opentelemetry_sdk", |b| b.iter(|| counter.add(1, &attrs)));
        }
        g.finish();
    }
}

fn handles(c: &mut Criterion) {
    let mut g = c.benchmark_group("counter, cached handle");
    let oxtel = oxtel_meters(false);
    let h = metrics::with_local_recorder(
        &oxtel.recorder(),
        || metrics::counter!("requests", "route" => "/items"),
    );
    g.bench_function("oxtel", |b| b.iter(|| h.increment(1)));
    let prometheus = prometheus_recorder();
    let h = metrics::with_local_recorder(
        &prometheus,
        || metrics::counter!("requests", "route" => "/items"),
    );
    g.bench_function("metrics-exporter-prometheus", |b| b.iter(|| h.increment(1)));
    let provider = otel_meters();
    let bound = provider
        .meter("bench")
        .u64_counter("requests")
        .build()
        .bind(&[KeyValue::new("route", "/items")]);
    g.bench_function("opentelemetry_sdk (bound, experimental)", |b| {
        b.iter(|| bound.add(1))
    });
    g.finish();

    let mut g = c.benchmark_group("histogram, cached handle");
    let h = metrics::with_local_recorder(
        &oxtel.recorder(),
        || metrics::histogram!("latency", "route" => "/items"),
    );
    g.bench_function("oxtel", |b| b.iter(|| h.record(0.042)));
    let h = metrics::with_local_recorder(
        &prometheus,
        || metrics::histogram!("latency", "route" => "/items"),
    );
    g.bench_function("metrics-exporter-prometheus", |b| {
        b.iter(|| h.record(0.042))
    });
    let bound = provider
        .meter("bench")
        .f64_histogram("latency")
        .build()
        .bind(&[KeyValue::new("route", "/items")]);
    g.bench_function("opentelemetry_sdk (bound, experimental)", |b| {
        b.iter(|| bound.record(0.042))
    });
    g.finish();
}

/// What an exemplar costs: recording inside a sampled span, with the
/// trace-based exemplar filter on and off.
fn exemplars(c: &mut Criterion) {
    let mut g = c.benchmark_group("histogram in a sampled span (oxtel)");
    let (_traces, dispatch) = oxtel_traces(true);
    for (name, on) in [("exemplars off", false), ("exemplars on", true)] {
        let meters = oxtel_meters(on);
        let h = metrics::with_local_recorder(
            &meters.recorder(),
            || metrics::histogram!("latency", "route" => "/items"),
        );
        g.bench_function(name, |b| {
            tracing::dispatcher::with_default(&dispatch, || {
                let span = tracing::info_span!("request");
                let _entered = span.enter();
                b.iter(|| h.record(0.042));
            })
        });
    }
    g.finish();
}

/// Eight threads incrementing one series at once.
fn contended(c: &mut Criterion) {
    const THREADS: u64 = 8;
    let mut g = c.benchmark_group("counter, 8 threads, one series");
    let run = |iters: u64, add: &(dyn Fn() + Sync)| -> Duration {
        let per_thread = iters.div_ceil(THREADS);
        let start = Instant::now();
        std::thread::scope(|s| {
            for _ in 0..THREADS {
                s.spawn(|| {
                    for _ in 0..per_thread {
                        add();
                    }
                });
            }
        });
        start.elapsed()
    };
    let oxtel = oxtel_meters(false);
    let h = metrics::with_local_recorder(&oxtel.recorder(), || metrics::counter!("requests"));
    g.bench_function("oxtel", |b| {
        b.iter_custom(|iters| run(iters, &|| h.increment(1)))
    });
    let prometheus = prometheus_recorder();
    let h = metrics::with_local_recorder(&prometheus, || metrics::counter!("requests"));
    g.bench_function("metrics-exporter-prometheus", |b| {
        b.iter_custom(|iters| run(iters, &|| h.increment(1)))
    });
    let provider = otel_meters();
    let counter = provider.meter("bench").u64_counter("requests").build();
    g.bench_function("opentelemetry_sdk", |b| {
        b.iter_custom(|iters| run(iters, &|| counter.add(1, &[])))
    });
    g.finish();
}

criterion_group!(benches, per_call, handles, exemplars, contended);
criterion_main!(benches);
