//! Model calls recorded into in-memory exporters, and checked against the
//! GenAI conventions.

use oxtel_genai::Modality;
use oxtel_sdk::Resource;
use oxtel_sdk::metrics::{
    GENAI_SECONDS_BUCKETS, GENAI_TOKEN_BUCKETS, MeterProvider, MetricFamily, MetricKind, Point,
    PointValue,
};
use oxtel_sdk::trace::{InMemorySpanExporter, Sampler, SpanData, SpanKind, Status, TracerProvider};
use oxtel_semconv::{Instrument, attribute, genai};
use tracing_subscriber::layer::SubscriberExt;

/// Runs `f` with spans and metrics recorded in memory.
fn record(f: impl FnOnce()) -> (Vec<SpanData>, Vec<MetricFamily>) {
    let spans = InMemorySpanExporter::default();
    let traces = TracerProvider::builder()
        .with_resource(Resource::builder().without_env().build())
        .with_sampler(Sampler::AlwaysOn)
        .with_simple_exporter(spans.clone())
        .build();
    let meters = MeterProvider::builder().build();
    let subscriber = tracing_subscriber::registry().with(traces.layer());
    tracing::subscriber::with_default(subscriber, || {
        metrics::with_local_recorder(&meters.recorder(), f)
    });
    (spans.finished_spans(), meters.collect())
}

fn attr<'a>(span: &'a SpanData, key: &str) -> Option<&'a oxtel_sdk::Value> {
    span.attributes
        .iter()
        .find(|kv| kv.key == key)
        .map(|kv| &kv.value)
}

fn family<'a>(families: &'a [MetricFamily], name: &str) -> &'a MetricFamily {
    families
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("no {name}"))
}

fn has(point: &Point, key: &str, value: &str) -> bool {
    point.attributes.contains(&(key.into(), value.into()))
}

fn label<'a>(point: &'a Point, key: &str) -> Option<&'a str> {
    point
        .attributes
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

fn histogram(point: &Point) -> &oxtel_sdk::metrics::HistogramPoint {
    match &point.value {
        PointValue::Histogram(h) => h,
        other => panic!("not a histogram: {other:?}"),
    }
}

#[test]
fn a_chat_call_records_its_span_duration_and_tokens() {
    let (spans, families) = record(|| {
        let call = oxtel_genai::chat("ollama", "llama3.2")
            .server("localhost", 11434)
            .max_tokens(512)
            .temperature(0.2)
            .top_k(40)
            .seed(7)
            .choices(1)
            .conversation_id("conv-1")
            .start();
        call.span().in_scope(|| tracing::info!("sending"));
        call.response_id("r-1")
            .response_model("llama3.2:latest")
            .input_tokens(26)
            .output_tokens(298)
            .finish();
    });

    assert_eq!(spans.len(), 1);
    let span = &spans[0];
    assert_eq!(span.name, "chat llama3.2");
    assert_eq!(span.kind, SpanKind::Client);
    assert_eq!(span.status, Status::Unset);
    for (key, value) in [
        ("gen_ai.operation.name", "chat".into()),
        ("gen_ai.provider.name", "ollama".into()),
        ("gen_ai.request.model", "llama3.2".into()),
        ("server.address", "localhost".into()),
        ("server.port", oxtel_sdk::Value::I64(11434)),
        ("gen_ai.request.max_tokens", oxtel_sdk::Value::I64(512)),
        ("gen_ai.request.temperature", oxtel_sdk::Value::F64(0.2)),
        ("gen_ai.request.top_k", oxtel_sdk::Value::I64(40)),
        ("gen_ai.request.seed", oxtel_sdk::Value::I64(7)),
        ("gen_ai.conversation.id", "conv-1".into()),
        ("gen_ai.response.id", "r-1".into()),
        ("gen_ai.response.model", "llama3.2:latest".into()),
        ("gen_ai.usage.input_tokens", oxtel_sdk::Value::I64(26)),
        ("gen_ai.usage.output_tokens", oxtel_sdk::Value::I64(298)),
    ] {
        assert_eq!(attr(span, key), Some(&value), "{key}");
    }
    assert_eq!(
        attr(span, "gen_ai.request.choice.count"),
        None,
        "one choice is the default, and isn't recorded"
    );
    assert_eq!(attr(span, "gen_ai.request.stream"), None);
    assert_eq!(attr(span, "error.type"), None);

    let duration = family(&families, "gen_ai.client.inference.duration");
    assert_eq!(duration.unit.as_deref(), Some("s"));
    let point = &duration.points[0];
    for (key, value) in [
        ("gen_ai.operation.name", "chat"),
        ("gen_ai.provider.name", "ollama"),
        ("gen_ai.request.model", "llama3.2"),
        ("gen_ai.response.model", "llama3.2:latest"),
        ("server.address", "localhost"),
        ("server.port", "11434"),
    ] {
        assert!(has(point, key, value), "{key}");
    }
    assert_eq!(&*histogram(point).bounds, GENAI_SECONDS_BUCKETS);

    let input = family(&families, "gen_ai.client.inference.operation.input_tokens");
    assert_eq!(histogram(&input.points[0]).sum, 26.0);
    assert_eq!(&*histogram(&input.points[0]).bounds, GENAI_TOKEN_BUCKETS);
    let output = family(&families, "gen_ai.client.inference.operation.output_tokens");
    assert_eq!(histogram(&output.points[0]).sum, 298.0);

    let usage = family(&families, "gen_ai.client.inference.usage.output_tokens");
    assert_eq!(usage.kind, MetricKind::Counter);
    assert_eq!(usage.points[0].value, PointValue::Counter(298));
    assert!(has(&usage.points[0], "gen_ai.token.modality", "text"));
    assert!(has(
        &usage.points[0],
        "gen_ai.response.model",
        "llama3.2:latest"
    ));

    assert!(
        families
            .iter()
            .all(|f| f.name != "gen_ai.client.operation.duration"),
        "inference has its own duration"
    );
}

#[test]
fn a_failed_call_is_an_error_and_counts_only_the_tokens_it_reports() {
    let (spans, families) = record(|| {
        oxtel_genai::chat("openai", "gpt-4o")
            .start()
            .fail("timeout");
        oxtel_genai::chat("openai", "gpt-4o")
            .start()
            .input_tokens(1200)
            .fail("context_length_exceeded");
    });

    assert!(matches!(spans[0].status, Status::Error { .. }));
    assert_eq!(attr(&spans[0], "error.type"), Some(&"timeout".into()));
    assert_eq!(attr(&spans[0], "gen_ai.usage.input_tokens"), None);
    assert_eq!(
        attr(&spans[1], "gen_ai.usage.input_tokens"),
        Some(&oxtel_sdk::Value::I64(1200))
    );

    let duration = family(&families, "gen_ai.client.inference.duration");
    assert_eq!(duration.points.len(), 2);
    assert!(
        duration
            .points
            .iter()
            .any(|p| has(p, "error.type", "timeout"))
    );

    let input = family(&families, "gen_ai.client.inference.operation.input_tokens");
    assert_eq!(input.points.len(), 1, "only the call that reported tokens");
    assert!(has(
        &input.points[0],
        "error.type",
        "context_length_exceeded"
    ));
    let usage = family(&families, "gen_ai.client.inference.usage.input_tokens");
    assert_eq!(
        label(&usage.points[0], "error.type"),
        None,
        "the usage counters have no error.type"
    );
    assert!(
        families.iter().all(|f| !f.name.ends_with("output_tokens")),
        "no output tokens were reported"
    );
}

#[test]
fn tokens_of_several_modalities_add_up() {
    let (spans, families) = record(|| {
        oxtel_genai::generate_content("gcp.gemini", "gemini-2.5-flash")
            .start()
            .input_tokens(100)
            .input_tokens_of(Modality::Image, 258)
            .output_tokens(40)
            .finish();
    });
    assert_eq!(
        attr(&spans[0], "gen_ai.usage.input_tokens"),
        Some(&oxtel_sdk::Value::I64(358))
    );
    let input = family(&families, "gen_ai.client.inference.operation.input_tokens");
    assert_eq!(histogram(&input.points[0]).sum, 358.0);
    let usage = family(&families, "gen_ai.client.inference.usage.input_tokens");
    let by = |modality| {
        usage
            .points
            .iter()
            .find(|p| has(p, "gen_ai.token.modality", modality))
            .map(|p| p.value.clone())
    };
    assert_eq!(by("text"), Some(PointValue::Counter(100)));
    assert_eq!(by("image"), Some(PointValue::Counter(258)));
    assert_eq!(by("audio"), None);
}

#[test]
fn a_streamed_call_times_its_chunks() {
    let (spans, families) = record(|| {
        let mut call = oxtel_genai::chat("anthropic", "claude-sonnet-5-5")
            .streamed()
            .start();
        for _ in 0..3 {
            call.chunk();
        }
        call.output_tokens(3).finish();
    });
    assert_eq!(
        attr(&spans[0], "gen_ai.request.stream"),
        Some(&oxtel_sdk::Value::Bool(true))
    );
    assert!(matches!(
        attr(&spans[0], "gen_ai.response.time_to_first_chunk"),
        Some(oxtel_sdk::Value::F64(s)) if *s >= 0.0
    ));
    let first = family(&families, "gen_ai.client.inference.time_to_first_chunk");
    assert_eq!(first.unit.as_deref(), Some("s"));
    assert_eq!(histogram(&first.points[0]).count(), 1);
    let per = family(&families, "gen_ai.client.inference.time_per_output_chunk");
    assert_eq!(histogram(&per.points[0]).count(), 2);
}

#[test]
fn an_embeddings_call_is_an_operation_not_inference() {
    let (spans, families) = record(|| {
        oxtel_genai::embeddings("ollama", "nomic-embed-text")
            .server("localhost", 11434)
            .start()
            .input_tokens(12)
            .dimensions(768)
            .finish();
    });
    assert_eq!(spans[0].name, "embeddings nomic-embed-text");
    assert_eq!(
        attr(&spans[0], "gen_ai.usage.input_tokens"),
        Some(&oxtel_sdk::Value::I64(12))
    );
    assert_eq!(
        attr(&spans[0], "gen_ai.embeddings.dimension.count"),
        Some(&oxtel_sdk::Value::I64(768))
    );
    let names: Vec<&str> = families.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["gen_ai.client.operation.duration"]);
    assert!(has(
        &families[0].points[0],
        "gen_ai.operation.name",
        "embeddings"
    ));
}

/// Everything recorded is named as the GenAI conventions define it, with
/// each metric's instrument and unit.
#[test]
fn spans_and_metrics_use_only_defined_names() {
    let (spans, families) = record(|| {
        let mut call = oxtel_genai::chat("openai", "gpt-4o")
            .server("api.openai.com", 443)
            .max_tokens(100)
            .temperature(1.0)
            .top_p(0.9)
            .top_k(5)
            .frequency_penalty(0.1)
            .presence_penalty(0.1)
            .seed(1)
            .choices(2)
            .streamed()
            .conversation_id("c")
            .start();
        call.chunk();
        call.chunk();
        call.response_id("r")
            .response_model("gpt-4o-2024-08-06")
            .input_tokens(10)
            .output_tokens(20)
            .fail("500");
        oxtel_genai::embeddings("openai", "text-embedding-3-small")
            .start()
            .input_tokens(3)
            .dimensions(1536)
            .finish();
    });
    assert_eq!(spans.len(), 2);
    // The names recorded, kept in tests/recorded/ for the site's reference:
    // OXTEL_REWRITE_RECORDED=1 rewrites the file when a change is meant.
    oxtel_sdk::testing::assert_recorded_names(
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/recorded/calls.txt"),
        &spans,
        &families,
        std::env::var_os("OXTEL_REWRITE_RECORDED").is_some(),
    );

    for span in &spans {
        for kv in &span.attributes {
            assert!(
                kv.key.starts_with("otel.")
                    || attribute::is_defined(&kv.key)
                    || genai::attribute::is_defined(&kv.key),
                "span {:?} has attribute {:?}, which the conventions don't define",
                span.name,
                kv.key
            );
        }
    }
    assert_eq!(families.len(), 8);
    for family in &families {
        let definition = genai::metric::by_name(&family.name)
            .unwrap_or_else(|| panic!("{} isn't a defined metric", family.name));
        let instrument = match family.kind {
            MetricKind::Counter => Instrument::Counter,
            MetricKind::Histogram => Instrument::Histogram,
            MetricKind::Gauge => panic!("{} is a gauge", family.name),
        };
        assert_eq!(instrument, definition.instrument, "{}", family.name);
        match &family.unit {
            Some(unit) => assert_eq!(unit, definition.unit, "{}", family.name),
            // `metrics` can't express an annotation such as `{token}`.
            None => assert!(
                definition.unit.starts_with('{'),
                "{} needs a unit",
                family.name
            ),
        }
        for point in &family.points {
            for (key, _) in &point.attributes {
                assert!(
                    attribute::is_defined(key) || genai::attribute::is_defined(key),
                    "{} has label {key:?}, which the conventions don't define",
                    family.name
                );
            }
        }
    }
}
