//! Local log lines: text, flat JSON and ECS JSON, with trace context.

use std::io::Write;
use std::sync::{Arc, Mutex};

use oxtel_sdk::logs::{ECS_VERSION, LocalFormat, LocalLayer};
use oxtel_sdk::trace::{Sampler, TracerProvider, span_context};
use oxtel_sdk::{KeyValue, Resource, SpanContext};
use serde_json::Value as Json;
use tracing_subscriber::layer::SubscriberExt;

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Captured {
    fn lines(&self) -> Vec<String> {
        String::from_utf8(self.0.lock().unwrap().clone())
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

/// Run `f` with a trace layer (for ids) and `layer`, returning the lines.
fn capture<R>(layer: LocalLayer, f: impl FnOnce() -> R) -> (Vec<String>, R) {
    let out = Captured::default();
    let traces = TracerProvider::builder()
        .with_resource(Resource::builder().without_env().build())
        .with_sampler(Sampler::AlwaysOn)
        .build();
    let subscriber = tracing_subscriber::registry()
        .with(traces.layer())
        .with(layer.with_writer(out.clone()));
    let result = tracing::subscriber::with_default(subscriber, f);
    (out.lines(), result)
}

/// The request the AgentStateGraph server logs: fields on two spans, one
/// recorded after the span opened, and on the event.
fn request() -> SpanContext {
    let outer = tracing::info_span!("outer", shared = "outer", only.outer = 1u64);
    let _outer = outer.enter();
    let span = tracing::info_span!(
        "http",
        http.request.method = "GET",
        shared = "inner",
        asg.api_key.id = tracing::field::Empty,
    );
    let _span = span.enter();
    span.record("asg.api_key.id", "key-1");
    tracing::warn!(
        http.response.status_code = 200u64,
        http.server.request.duration = 0.25f64,
        "request"
    );
    span_context(&span).unwrap()
}

fn parse(line: &str) -> Json {
    serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {line}"))
}

#[test]
fn json_lines_are_flat_and_carry_their_span() {
    let (lines, cx) = capture(LocalLayer::new(LocalFormat::Json), request);
    let line = parse(&lines[0]);
    assert_eq!(line["message"], "request");
    assert_eq!(line["level"], "WARN");
    assert_eq!(line["target"], "local");
    assert_eq!(line["http.request.method"], "GET");
    assert_eq!(line["asg.api_key.id"], "key-1", "recorded after opening");
    assert_eq!(line["http.response.status_code"], 200);
    assert_eq!(line["http.server.request.duration"], 0.25);
    assert_eq!(line["only.outer"], 1);
    assert_eq!(line["shared"], "inner", "the inner span wins");
    assert_eq!(line["trace_id"], cx.trace_id.to_string());
    assert_eq!(line["span_id"], cx.span_id.to_string());
    assert!(line.get("fields").is_none() && line.get("span").is_none());
    assert!(
        lines[0].starts_with(r#"{"timestamp":""#),
        "fixed fields first: {}",
        lines[0]
    );
    let time = line["timestamp"].as_str().unwrap();
    assert!(time.ends_with('Z') && time.len() == 27, "{time}");
}

#[test]
fn ecs_lines_follow_the_ecs_logging_layout() {
    let resource = Resource::builder()
        .without_env()
        .with_service_name("checkout")
        .with_attribute(KeyValue::new("service.version", "1.4.2"))
        .with_attribute(KeyValue::new("deployment.environment.name", "staging"))
        .build();
    let (lines, cx) = capture(
        LocalLayer::new(LocalFormat::Ecs).with_resource(&resource),
        request,
    );
    let line = parse(&lines[0]);
    let keys: Vec<&str> = line
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    // serde_json sorts keys, so check the written order on the raw line.
    let at = |k: &str| lines[0].find(&format!("\"{k}\":")).unwrap();
    assert!(at("@timestamp") < at("log.level") && at("log.level") < at("message"));
    assert!(at("message") < at("ecs.version"));
    assert!(keys.contains(&"log.origin.file.name"));
    assert_eq!(line["log.level"], "WARN");
    assert_eq!(line["message"], "request");
    assert_eq!(line["ecs.version"], ECS_VERSION);
    assert_eq!(line["log.logger"], "local");
    assert_eq!(line["log.origin.file.name"], file!());
    assert!(line["log.origin.file.line"].is_u64());
    assert_eq!(line["trace.id"], cx.trace_id.to_string());
    assert_eq!(line["span.id"], cx.span_id.to_string());
    assert_eq!(line["service.name"], "checkout");
    assert_eq!(line["service.version"], "1.4.2");
    assert_eq!(line["service.environment"], "staging");
    assert_eq!(line["http.request.method"], "GET");
}

#[test]
fn text_lines_read_like_tracings_own() {
    let (lines, cx) = capture(LocalLayer::new(LocalFormat::Text), request);
    let line = &lines[0];
    let (time, rest) = line.split_once(' ').unwrap();
    assert!(time.ends_with('Z'));
    assert_eq!(
        rest,
        format!(
            " WARN outer{{shared=outer only.outer=1}}: http{{http.request.method=GET shared=inner asg.api_key.id=key-1}}: local: request http.response.status_code=200 http.server.request.duration=0.25 trace_id={} span_id={}",
            cx.trace_id, cx.span_id
        )
    );
}

#[test]
fn lines_outside_spans_have_no_ids() {
    let (lines, ()) = capture(LocalLayer::new(LocalFormat::Json), || {
        tracing::info!(n = 1, "plain");
    });
    let line = parse(&lines[0]);
    assert!(line.get("trace_id").is_none() && line.get("span_id").is_none());
    let (lines, ()) = capture(LocalLayer::new(LocalFormat::Text), || {
        tracing::info!(n = 1, "plain");
    });
    assert!(lines[0].ends_with(" INFO local: plain n=1"), "{}", lines[0]);
}

#[test]
fn awkward_values_stay_valid_json() {
    let (lines, ()) = capture(LocalLayer::new(LocalFormat::Json), || {
        tracing::info!(
            level = "a field named like a fixed key",
            ratio = f64::NAN,
            big = u64::MAX,
            quote = "she said \"hi\"\n\ttab",
            "line one\nline \u{1} two é"
        );
    });
    let line = parse(&lines[0]);
    assert_eq!(line["level"], "INFO", "the fixed key keeps its meaning");
    assert_eq!(line["fields.level"], "a field named like a fixed key");
    assert_eq!(line["ratio"], "NaN");
    assert_eq!(line["big"], u64::MAX.to_string());
    assert_eq!(line["quote"], "she said \"hi\"\n\ttab");
    assert_eq!(line["message"], "line one\nline \u{1} two é");
    assert_eq!(
        lines.len(),
        1,
        "a newline in a value doesn't split the line"
    );
}

#[test]
fn formats_are_named_text_json_and_ecs() {
    assert_eq!(LocalFormat::from_name(" JSON "), Some(LocalFormat::Json));
    assert_eq!(LocalFormat::from_name("ecs"), Some(LocalFormat::Ecs));
    assert_eq!(LocalFormat::from_name("text"), Some(LocalFormat::Text));
    assert_eq!(LocalFormat::from_name("pretty"), None);
}

#[test]
fn instrumentation_spans_read_by_their_name_and_otel_fields_stay_out() {
    let event = || {
        let user = tracing::info_span!("checkout", cart.items = 3u64);
        let _user = user.enter();
        let http = tracing::info_span!(
            "HTTP server request",
            otel.name = "GET /orders/{id}",
            otel.kind = "server",
            otel.status_code = tracing::field::Empty,
            http.route = "/orders/{id}",
        );
        let _http = http.enter();
        http.record("otel.status_code", "error");
        tracing::warn!(order.id = 7u64, "looking up order");
    };

    let (lines, ()) = capture(LocalLayer::new(LocalFormat::Text), event);
    let text = &lines[0];
    assert!(
        text.contains(" WARN checkout{cart.items=3}: GET /orders/{id}: local: looking up order order.id=7 trace_id="),
        "{text}"
    );

    let (lines, ()) = capture(LocalLayer::new(LocalFormat::Json), event);
    let json = parse(&lines[0]);
    assert_eq!(json["http.route"], "/orders/{id}", "data fields stay");
    assert_eq!(json["cart.items"], 3);
    for control in ["otel.name", "otel.kind", "otel.status_code"] {
        assert!(json.get(control).is_none(), "{control} in {json}");
    }
}
