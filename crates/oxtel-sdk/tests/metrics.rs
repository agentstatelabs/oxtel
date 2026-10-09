//! The `metrics` facade recorder: what it stores, and the exemplars it
//! keeps.

use oxtel_sdk::Resource;
use oxtel_sdk::metrics::{
    ExemplarFilter, MeterProvider, MetricFamily, MetricKind, OVERFLOW_ATTRIBUTE, PointValue,
    SECONDS_BUCKETS,
};
use oxtel_sdk::trace::{Sampler, TracerProvider, span_context};
use tracing_subscriber::layer::SubscriberExt;

fn provider() -> MeterProvider {
    MeterProvider::builder()
        .with_exemplar_filter(ExemplarFilter::TraceBased)
        .build()
}

fn family<'a>(families: &'a [MetricFamily], name: &str) -> &'a MetricFamily {
    families
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("no metric {name}"))
}

#[test]
fn counters_gauges_and_histograms_record_through_the_macros() {
    let provider = provider();
    metrics::with_local_recorder(&provider.recorder(), || {
        metrics::describe_histogram!(
            "http.server.request.duration",
            metrics::Unit::Seconds,
            "Duration of HTTP server requests."
        );
        metrics::counter!("jobs.done", "queue" => "email").increment(2);
        metrics::counter!("jobs.done", "queue" => "email").increment(3);
        metrics::counter!("jobs.done", "queue" => "sms").increment(1);
        let depth = metrics::gauge!("queue.depth");
        depth.set(10.0);
        depth.increment(2.5);
        depth.decrement(0.5);
        let duration =
            metrics::histogram!("http.server.request.duration", "http.route" => "/items");
        for v in [0.003, 0.02, 0.02, 3.0, 30.0, 0.005] {
            duration.record(v);
        }
    });
    let families = provider.collect();

    let jobs = family(&families, "jobs.done");
    assert_eq!(jobs.kind, MetricKind::Counter);
    assert_eq!(jobs.scope.name, "metrics", "the test crate's name");
    let values: Vec<_> = jobs
        .points
        .iter()
        .map(|p| (&p.attributes[0].1, &p.value))
        .collect();
    assert_eq!(
        values,
        vec![
            (&"email".to_string(), &PointValue::Counter(5)),
            (&"sms".to_string(), &PointValue::Counter(1)),
        ]
    );

    assert_eq!(
        family(&families, "queue.depth").points[0].value,
        PointValue::Gauge(12.0)
    );

    let duration = family(&families, "http.server.request.duration");
    assert_eq!(duration.unit.as_deref(), Some("s"));
    assert_eq!(
        duration.description.as_deref(),
        Some("Duration of HTTP server requests.")
    );
    let PointValue::Histogram(h) = &duration.points[0].value else {
        panic!("not a histogram")
    };
    assert_eq!(
        &*h.bounds,
        SECONDS_BUCKETS.as_slice(),
        "seconds get duration buckets"
    );
    assert_eq!(h.count(), 6);
    assert_eq!(
        h.bucket_counts[0], 2,
        "0.003 and 0.005: bounds are inclusive"
    );
    assert_eq!(h.bucket_counts[2], 2, "0.02 <= 0.025");
    assert_eq!(h.bucket_counts[11], 1, "3.0 <= 5.0");
    assert_eq!(h.bucket_counts[14], 1, "30.0 overflows");
    assert!((h.sum - 33.048).abs() < 1e-9);
    assert_eq!((h.min, h.max), (Some(0.003), Some(30.0)));
}

#[test]
fn label_order_doesnt_split_a_series() {
    let provider = provider();
    metrics::with_local_recorder(&provider.recorder(), || {
        metrics::counter!("c", "a" => "1", "b" => "2").increment(1);
        metrics::counter!("c", "b" => "2", "a" => "1").increment(1);
    });
    let families = provider.collect();
    let c = family(&families, "c");
    assert_eq!(c.points.len(), 1);
    assert_eq!(c.points[0].value, PointValue::Counter(2));
}

#[test]
fn series_past_the_cardinality_limit_go_to_the_overflow_series() {
    let provider = MeterProvider::builder().with_cardinality_limit(3).build();
    metrics::with_local_recorder(&provider.recorder(), || {
        for user in ["ana", "bo", "cy", "di", "ed"] {
            metrics::counter!("logins", "user" => user).increment(1);
        }
        // An existing series keeps working past the limit.
        metrics::counter!("logins", "user" => "ana").increment(1);
    });
    let families = provider.collect();
    let logins = family(&families, "logins");
    assert_eq!(logins.points.len(), 3, "two series plus overflow");
    let overflow = logins
        .points
        .iter()
        .find(|p| p.attributes == vec![(OVERFLOW_ATTRIBUTE.to_string(), "true".to_string())])
        .expect("overflow series");
    assert_eq!(overflow.value, PointValue::Counter(3));
    let ana = logins
        .points
        .iter()
        .find(|p| p.attributes[0].1 == "ana")
        .unwrap();
    assert_eq!(ana.value, PointValue::Counter(2));
}

#[test]
fn a_name_reused_for_another_kind_is_ignored_not_a_panic() {
    let provider = provider();
    metrics::with_local_recorder(&provider.recorder(), || {
        metrics::counter!("thing").increment(1);
        metrics::histogram!("thing").record(5.0);
        metrics::gauge!("thing").set(1.0);
    });
    let families = provider.collect();
    let thing = family(&families, "thing");
    assert_eq!(thing.kind, MetricKind::Counter);
    assert_eq!(thing.points[0].value, PointValue::Counter(1));
}

#[test]
fn custom_buckets_apply_by_name_and_others_get_the_defaults() {
    let provider = MeterProvider::builder()
        .with_histogram_buckets("payload.size", &[1024.0, 64.0, 4096.0])
        .build();
    metrics::with_local_recorder(&provider.recorder(), || {
        metrics::histogram!("payload.size").record(100.0);
        metrics::histogram!("retries").record(2.0);
    });
    let families = provider.collect();
    let PointValue::Histogram(size) = &family(&families, "payload.size").points[0].value else {
        panic!()
    };
    assert_eq!(&*size.bounds, &[64.0, 1024.0, 4096.0]);
    let PointValue::Histogram(retries) = &family(&families, "retries").points[0].value else {
        panic!()
    };
    assert_eq!(retries.bounds[0], 0.0);
    assert_eq!(retries.bounds.len(), 15);
}

#[test]
fn exemplars_link_measurements_to_sampled_spans_only() {
    let tracer = |sampler| {
        TracerProvider::builder()
            .with_resource(Resource::builder().without_env().build())
            .with_sampler(sampler)
            .build()
    };
    let record = |tracer: &TracerProvider, filter| {
        let meter = MeterProvider::builder()
            .with_exemplar_filter(filter)
            .build();
        let subscriber = tracing_subscriber::registry().with(tracer.layer());
        let span_cx = tracing::subscriber::with_default(subscriber, || {
            metrics::with_local_recorder(&meter.recorder(), || {
                let span = tracing::info_span!("request");
                let _entered = span.enter();
                metrics::histogram!("latency").record(0.2);
                metrics::counter!("requests").increment(1);
                span_context(&span)
            })
        });
        (meter.collect(), span_cx.expect("span context"))
    };

    let (families, cx) = record(&tracer(Sampler::AlwaysOn), ExemplarFilter::TraceBased);
    let PointValue::Histogram(h) = &family(&families, "latency").points[0].value else {
        panic!()
    };
    let bucket = h.bounds.partition_point(|&b| b < 0.2);
    let exemplar = h.exemplars[bucket]
        .as_ref()
        .expect("an exemplar in 0.2's bucket");
    assert_eq!(exemplar.value, 0.2);
    assert_eq!(exemplar.trace_id, Some(cx.trace_id));
    assert_eq!(exemplar.span_id, Some(cx.span_id));
    let counter = &family(&families, "requests").points[0];
    assert_eq!(
        counter.exemplar.as_ref().and_then(|e| e.trace_id),
        Some(cx.trace_id)
    );

    let (families, _) = record(&tracer(Sampler::AlwaysOff), ExemplarFilter::TraceBased);
    let PointValue::Histogram(h) = &family(&families, "latency").points[0].value else {
        panic!()
    };
    assert!(h.exemplars.iter().all(Option::is_none), "unsampled span");

    let (families, _) = record(&tracer(Sampler::AlwaysOn), ExemplarFilter::AlwaysOff);
    assert!(family(&families, "requests").points[0].exemplar.is_none());
}
