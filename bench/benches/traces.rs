//! The cost of tracing on the instrumented thread: `tracing` alone, oxtel,
//! and `tracing-opentelemetry` over the official SDK, each with a processor
//! that drops finished spans.

use criterion::{Criterion, criterion_group, criterion_main};
use opentelemetry::trace::TraceContextExt;
use oxtel_bench::{baseline, otel_traces, oxtel_traces};
use tracing::dispatcher::with_default;
use tracing_opentelemetry::OpenTelemetrySpanExt;

fn one_span() {
    let span = tracing::info_span!("op", a = 1, b = "x", c = true);
    let _entered = span.enter();
}

fn nested() {
    let parent = tracing::info_span!("parent", a = 1);
    let _p = parent.enter();
    for i in 0..3 {
        let child = tracing::info_span!("child", i);
        let _c = child.enter();
    }
}

fn with_events() {
    let span = tracing::info_span!("op", a = 1);
    let _entered = span.enter();
    for i in 0..5 {
        tracing::info!(i, "step");
    }
}

fn compare(c: &mut Criterion, group: &str, sampled: bool, work: fn()) {
    let mut g = c.benchmark_group(group);
    let base = baseline();
    g.bench_function("tracing alone", |b| with_default(&base, || b.iter(work)));
    let (_oxtel, oxtel) = oxtel_traces(sampled);
    g.bench_function("oxtel", |b| with_default(&oxtel, || b.iter(work)));
    let (_otel, otel) = otel_traces(sampled);
    g.bench_function("tracing-opentelemetry", |b| {
        with_default(&otel, || b.iter(work))
    });
    g.finish();
}

fn spans(c: &mut Criterion) {
    compare(c, "span, 3 fields", true, one_span);
    compare(c, "span, 3 fields, not sampled", false, one_span);
    compare(c, "parent and 3 children", true, nested);
    compare(c, "span with 5 events", true, with_events);
}

/// Reading the current span's context, as propagation and exemplars do.
fn current_context(c: &mut Criterion) {
    let mut g = c.benchmark_group("current span context");
    let (_oxtel, oxtel) = oxtel_traces(true);
    g.bench_function("oxtel", |b| {
        with_default(&oxtel, || {
            let span = tracing::info_span!("op");
            let _entered = span.enter();
            b.iter(oxtel_sdk::trace::current_span_context);
        })
    });
    let (_otel, otel) = otel_traces(true);
    g.bench_function("tracing-opentelemetry", |b| {
        with_default(&otel, || {
            let span = tracing::info_span!("op");
            let _entered = span.enter();
            b.iter(|| {
                tracing::Span::current()
                    .context()
                    .span()
                    .span_context()
                    .clone()
            });
        })
    });
    g.finish();
}

criterion_group!(benches, spans, current_context);
criterion_main!(benches);
