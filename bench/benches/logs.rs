//! The cost of a log record on the instrumented thread: `tracing` alone,
//! oxtel's log layer, and `opentelemetry-appender-tracing` over the official
//! SDK, each with a processor that drops records. Inside a span, both also
//! run their trace layer, so records carry trace context.

use criterion::{Criterion, criterion_group, criterion_main};
use oxtel_bench::{baseline, otel_logs, oxtel_logs};
use tracing::dispatcher::with_default;

fn event() {
    tracing::info!(user = "u-1", items = 3, cached = true, "listed items");
}

fn compare(c: &mut Criterion, group: &str, in_span: bool) {
    let work = || {
        if in_span {
            let span = tracing::info_span!("request");
            let _entered = span.enter();
            event();
        } else {
            event();
        }
    };
    let mut g = c.benchmark_group(group);
    let base = baseline();
    g.bench_function("tracing alone", |b| with_default(&base, || b.iter(work)));
    let (_oxtel, oxtel) = oxtel_logs(in_span);
    g.bench_function("oxtel", |b| with_default(&oxtel, || b.iter(work)));
    let (_otel, otel) = otel_logs(in_span);
    g.bench_function("opentelemetry-appender-tracing", |b| {
        with_default(&otel, || b.iter(work))
    });
    g.finish();
}

fn logs(c: &mut Criterion) {
    compare(c, "log event, 3 fields", false);
    compare(c, "span with a log event", true);
}

criterion_group!(benches, logs);
criterion_main!(benches);
