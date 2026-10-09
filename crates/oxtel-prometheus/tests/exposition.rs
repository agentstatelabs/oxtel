//! The OpenMetrics and classic text expositions.

use oxtel_prometheus::{Format, Options, PrometheusExporter, render};
use oxtel_sdk::Resource;
use oxtel_sdk::metrics::{ExemplarFilter, MeterProvider};
use oxtel_sdk::trace::{Sampler, TracerProvider, span_context};
use tracing_subscriber::layer::SubscriberExt;

fn resource() -> Resource {
    Resource::builder()
        .without_env()
        .with_service_name("checkout")
        .build()
}

/// Metrics recorded inside one sampled span, and that span's ids.
fn recorded() -> (MeterProvider, String, String) {
    let tracer = TracerProvider::builder()
        .with_resource(resource())
        .with_sampler(Sampler::AlwaysOn)
        .build();
    let meters = MeterProvider::builder()
        .with_exemplar_filter(ExemplarFilter::TraceBased)
        .build();
    let subscriber = tracing_subscriber::registry().with(tracer.layer());
    let cx = tracing::subscriber::with_default(subscriber, || {
        metrics::with_local_recorder(&meters.recorder(), || {
            metrics::describe_histogram!(
                "http.server.request.duration",
                metrics::Unit::Seconds,
                "Duration of HTTP server requests."
            );
            let span = tracing::info_span!("request");
            let _entered = span.enter();
            let h = metrics::histogram!("http.server.request.duration", "http.route" => "/items");
            h.record(0.012);
            h.record(0.2);
            metrics::counter!("jobs.done", "queue" => "email").increment(5);
            metrics::gauge!("queue.depth").set(3.0);
            span_context(&span).unwrap()
        })
    });
    (meters, cx.trace_id.to_string(), cx.span_id.to_string())
}

fn line<'a>(text: &'a str, prefix: &str) -> &'a str {
    text.lines()
        .find(|l| l.starts_with(prefix))
        .unwrap_or_else(|| panic!("no line starting {prefix:?} in:\n{text}"))
}

#[test]
fn openmetrics_has_types_units_exemplars_and_eof() {
    let (meters, trace_id, span_id) = recorded();
    let text = render(
        &meters.collect(),
        &resource(),
        Format::OpenMetrics,
        &Options::default(),
    );

    assert!(text.ends_with("# EOF\n"), "{text}");
    assert!(line(&text, "target_info{").contains(r#"service_name="checkout""#));
    assert!(text.contains("# TYPE target info\n"));

    let h = "http_server_request_duration_seconds";
    assert!(text.contains(&format!(
        "# TYPE {h} histogram\n# UNIT {h} seconds\n# HELP {h} Duration of HTTP server requests.\n"
    )));
    let scope = r#"otel_scope_name="exposition""#;
    // 0.012 lands in le=0.025, 0.2 in le=0.25; buckets are cumulative.
    assert_eq!(
        line(
            &text,
            &format!(r#"{h}_bucket{{http_route="/items",{scope},le="0.01"}}"#)
        ),
        format!(r#"{h}_bucket{{http_route="/items",{scope},le="0.01"}} 0"#)
    );
    let b025 = line(
        &text,
        &format!(r#"{h}_bucket{{http_route="/items",{scope},le="0.025"}}"#),
    );
    assert!(
        b025.starts_with(&format!(
            r#"{h}_bucket{{http_route="/items",{scope},le="0.025"}} 1 # {{trace_id="{trace_id}",span_id="{span_id}"}} 0.012 "#
        )),
        "{b025}"
    );
    assert!(
        line(
            &text,
            &format!(r#"{h}_bucket{{http_route="/items",{scope},le="0.25"}}"#)
        )
        .contains(" 2 # {")
    );
    assert!(text.contains(&format!(
        r#"{h}_bucket{{http_route="/items",{scope},le="+Inf"}} 2"#
    )));
    assert!(text.contains(&format!(r#"{h}_count{{http_route="/items",{scope}}} 2"#)));
    assert!(text.contains(&format!(r#"{h}_sum{{http_route="/items",{scope}}} 0.212"#)));

    assert!(text.contains("# TYPE jobs_done counter\n"));
    assert!(line(&text, "jobs_done_total{").starts_with(&format!(
        r#"jobs_done_total{{queue="email",{scope}}} 5 # {{trace_id="{trace_id}""#
    )));
    assert!(text.contains(&format!("queue_depth{{{scope}}} 3\n")));
}

#[test]
fn classic_text_has_no_exemplars_units_or_eof() {
    let (meters, _, _) = recorded();
    let text = render(
        &meters.collect(),
        &resource(),
        Format::Text,
        &Options::default(),
    );
    assert!(
        !text.contains("# EOF") && !text.contains("# UNIT") && !text.contains(" # {"),
        "{text}"
    );
    assert!(text.contains("# TYPE jobs_done_total counter\n"));
    assert!(text.contains("# TYPE target_info gauge\n"));
    assert!(text.contains("# TYPE http_server_request_duration_seconds histogram\n"));
}

#[test]
fn options_turn_off_scope_labels_and_target_info() {
    let (meters, _, _) = recorded();
    let text = render(
        &meters.collect(),
        &resource(),
        Format::Text,
        &Options {
            scope_labels: false,
            target_info: false,
        },
    );
    assert!(
        !text.contains("otel_scope_name") && !text.contains("target_info"),
        "{text}"
    );
    assert!(text.contains("queue_depth 3\n"));
}

#[test]
fn colliding_label_names_merge_and_values_are_escaped() {
    let meters = MeterProvider::builder().build();
    metrics::with_local_recorder(&meters.recorder(), || {
        metrics::describe_gauge!("g", "line one\nline \\two\\");
        metrics::gauge!("g", "a.b" => "1", "a_b" => "2", "q" => "say \"hi\"\n").set(1.0);
    });
    let text = render(
        &meters.collect(),
        &resource(),
        Format::Text,
        &Options {
            scope_labels: false,
            target_info: false,
        },
    );
    assert!(
        text.contains("# HELP g line one\\nline \\\\two\\\\\n"),
        "{text}"
    );
    assert!(
        text.contains(r#"g{a_b="1;2",q="say \"hi\"\n"} 1"#),
        "{text}"
    );
}

#[test]
fn the_exporter_negotiates_the_format() {
    let (meters, _, _) = recorded();
    let exporter = PrometheusExporter::new(meters, resource());
    let prometheus_accept =
        "application/openmetrics-text;version=1.0.0;q=0.5,text/plain;version=0.0.4;q=0.3,*/*;q=0.2";
    let (content_type, body) = exporter.render(Some(prometheus_accept));
    assert_eq!(
        content_type,
        "application/openmetrics-text; version=1.0.0; charset=utf-8"
    );
    assert!(body.ends_with("# EOF\n"));
    let (content_type, body) = exporter.render(None);
    assert_eq!(content_type, "text/plain; version=0.0.4; charset=utf-8");
    assert!(!body.contains("# EOF"));
}
