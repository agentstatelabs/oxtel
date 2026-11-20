//! The configuration project's own examples, and the errors a bad file
//! gets.

use oxtel_config::{
    AttributeValue, Config, Error, ExporterConfig, OtlpFileConfig, OtlpGrpcConfig, OtlpHttpConfig,
    ProcessorConfig, ReaderConfig, SamplerConfig, TlsConfig, parse_with,
};

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn read(name: &str, env: &[(&str, &str)]) -> (Config, Vec<String>) {
    let lookup = |k: &str| {
        env.iter()
            .find(|(name, _)| *name == k)
            .map(|(_, v)| v.to_string())
    };
    let parsed = parse_with(&fixture(name), &lookup).unwrap_or_else(|e| panic!("{name}: {e}"));
    (parsed.config, parsed.warnings)
}

fn otlp(p: &ProcessorConfig) -> &OtlpHttpConfig {
    match p {
        ProcessorConfig::Batch {
            exporter: ExporterConfig::OtlpHttp(o),
            ..
        } => o,
        other => panic!("not a batch OTLP processor: {other:?}"),
    }
}

#[test]
fn the_sdk_example_reads_without_surprises() {
    let (c, warnings) = read("otel-sdk-config.yaml", &[]);
    assert_eq!(c.file_format, "1.2");
    assert!(!c.disabled);
    assert_eq!(
        c.resource.attributes,
        vec![(
            "service.name".to_string(),
            AttributeValue::String("unknown_service".into())
        )]
    );
    assert_eq!(c.attribute_limits.attribute_count, Some(128));
    assert_eq!(
        c.propagators.as_deref(),
        Some(&["tracecontext".to_string(), "baggage".to_string()][..])
    );
    let t = c.tracer_provider.unwrap();
    assert_eq!(
        t.sampler,
        Some(SamplerConfig::ParentBased(Box::new(
            SamplerConfig::AlwaysOn
        )))
    );
    assert_eq!(
        (t.limits.event_count, t.limits.link_count),
        (Some(128), Some(128))
    );
    let ProcessorConfig::Batch {
        schedule_delay_ms,
        max_queue_size,
        ..
    } = &t.processors[0]
    else {
        panic!()
    };
    assert_eq!(
        (*schedule_delay_ms, *max_queue_size),
        (Some(5000), Some(2048))
    );
    let o = otlp(&t.processors[0]);
    assert_eq!(
        o.endpoint.as_deref(),
        Some("http://localhost:4318/v1/traces")
    );
    assert!(o.gzip);
    assert_eq!(o.timeout_ms, Some(10_000));
    let m = c.meter_provider.unwrap();
    assert!(matches!(
        &m.readers[0],
        ReaderConfig::Periodic {
            interval_ms: Some(60_000),
            ..
        }
    ));
    assert_eq!(m.exemplar_filter.as_deref(), Some("trace_based"));
    assert_eq!(c.logger_provider.unwrap().processors.len(), 1);
    // The only things oxtel can't honour: limits it doesn't have yet.
    assert!(
        warnings.iter().all(|w| w.contains("attribute_count_limit")),
        "{warnings:#?}"
    );
}

#[test]
fn the_migration_example_takes_its_values_from_the_environment() {
    let (c, warnings) = read("otel-sdk-migration-config.yaml", &[]);
    assert!(!c.disabled, "the default applies");
    assert_eq!(
        c.resource.attributes[0].1,
        AttributeValue::String("unknown_service".into())
    );
    assert_eq!(c.resource.attributes_list, None, "unset becomes null");
    let t = c.tracer_provider.unwrap();
    assert_eq!(
        otlp(&t.processors[0]).endpoint.as_deref(),
        Some("http://localhost:4318/v1/traces")
    );
    assert!(
        warnings.iter().all(|w| w.contains("attribute_count_limit")),
        "{warnings:#?}"
    );

    let (c, _) = read(
        "otel-sdk-migration-config.yaml",
        &[
            ("OTEL_SDK_DISABLED", "true"),
            (
                "OTEL_EXPORTER_OTLP_ENDPOINT",
                "https://collector.example:4318",
            ),
            ("OTEL_EXPORTER_OTLP_TRACES_COMPRESSION", "none"),
            ("OTEL_BSP_MAX_QUEUE_SIZE", "64"),
            ("OTEL_PROPAGATORS", "tracecontext"),
            ("OTEL_SERVICE_NAME", "checkout"),
        ],
    );
    assert!(
        c.disabled,
        "`${{OTEL_SDK_DISABLED:-false}}` is typed as a boolean"
    );
    assert_eq!(
        c.resource.attributes[0].1,
        AttributeValue::String("checkout".into())
    );
    assert_eq!(
        c.propagators.as_deref(),
        Some(&["tracecontext".to_string()][..])
    );
    let t = c.tracer_provider.unwrap();
    let o = otlp(&t.processors[0]);
    assert_eq!(
        o.endpoint.as_deref(),
        Some("https://collector.example:4318/v1/traces")
    );
    assert!(!o.gzip);
    assert!(matches!(
        t.processors[0],
        ProcessorConfig::Batch {
            max_queue_size: Some(64),
            ..
        }
    ));
}

#[test]
fn the_getting_started_example_names_its_detectors() {
    let (c, warnings) = read("otel-getting-started.yaml", &[]);
    assert_eq!(
        c.resource.detectors.as_deref(),
        Some(&["service", "host", "process", "container"].map(String::from)[..])
    );
    assert!(warnings.is_empty(), "{warnings:#?}");
}

fn error(text: &str) -> Error {
    parse_with(text, &|_| None).unwrap_err()
}

#[test]
fn bad_files_are_errors_that_say_where() {
    assert_eq!(error("disabled: true\n"), Error::FileFormat(None));
    assert_eq!(
        error("file_format: \"2.0\"\n"),
        Error::FileFormat(Some("2.0".into()))
    );
    assert_eq!(
        error(
            "file_format: \"1.2\"\ntracer_provider:\n  processors:\n    - batch:\n        max_queue_size: lots\n        exporter:\n          console:\n"
        ),
        Error::Invalid {
            path: "tracer_provider.processors[0].batch.max_queue_size".into(),
            expected: "a non-negative integer".into(),
        }
    );
    assert!(matches!(
        error("file_format: \"1.2\"\ndisabled: ${OOPS\n"),
        Error::Parse { line: 2, .. }
    ));
    let e = error(
        "file_format: \"1.2\"\ntracer_provider:\n  processors:\n    - batch:\n        exporter:\n          otlp_http:\n            compression: zstd\n",
    );
    assert!(e.to_string().contains("compression"), "{e}");
}

#[test]
fn unsupported_parts_are_warnings_not_errors() {
    let text = r#"
file_format: "1.2"
tracer_provider:
  processors:
    - batch:
        exporter:
          zipkin:
            endpoint: http://localhost:9411/api/v2/spans
    - simple:
        exporter:
          console:
  sampler:
    jaeger_remote:
      endpoint: http://localhost:14250
meter_provider:
  views:
    - selector: {instrument_name: x}
  readers:
    - pull:
        exporter:
          prometheus/development:
            port: 9100
            without_scope_info: true
unknown_top_level: 1
"#;
    let parsed = parse_with(text, &|_| None).unwrap();
    let t = parsed.config.tracer_provider.unwrap();
    assert_eq!(
        t.processors,
        vec![ProcessorConfig::Simple {
            exporter: ExporterConfig::Console
        }]
    );
    assert_eq!(t.sampler, None);
    assert_eq!(
        parsed.config.meter_provider.unwrap().readers,
        vec![ReaderConfig::Prometheus {
            host: None,
            port: Some(9100),
            without_scope_info: true,
            without_target_info: false,
        }]
    );
    let w = parsed.warnings.join("\n");
    for expected in ["zipkin", "jaeger_remote", "unknown_top_level"] {
        assert!(w.contains(expected), "no warning about {expected}:\n{w}");
    }
}

#[test]
fn otlp_file_writes_json_lines_to_stdout_or_a_file() {
    let text = r#"
file_format: "1.2"
tracer_provider:
  processors:
    - batch:
        exporter:
          otlp_file/development:
            output_stream: file:///var/log/traces.jsonl
meter_provider:
  readers:
    - periodic:
        exporter:
          otlp_file/development:
"#;
    let parsed = parse_with(text, &|_| None).unwrap();
    assert!(parsed.warnings.is_empty(), "{:#?}", parsed.warnings);
    assert!(matches!(
        &parsed.config.tracer_provider.unwrap().processors[0],
        ProcessorConfig::Batch {
            exporter: ExporterConfig::OtlpFile(OtlpFileConfig { output_stream: Some(s) }),
            ..
        } if s == "file:///var/log/traces.jsonl"
    ));
    assert!(matches!(
        &parsed.config.meter_provider.unwrap().readers[0],
        ReaderConfig::Periodic {
            exporter: ExporterConfig::OtlpFile(OtlpFileConfig {
                output_stream: None
            }),
            ..
        }
    ));

    let bad = "file_format: \"1.2\"\ntracer_provider:\n  processors:\n    - simple:\n        exporter:\n          otlp_file/development:\n            output_stream: /var/log/traces.jsonl\n";
    let error = parse_with(bad, &|_| None).unwrap_err().to_string();
    assert!(
        error.contains("output_stream") && error.contains("file://"),
        "{error}"
    );
}

#[test]
fn otlp_grpc_and_tls_files_are_read() {
    let text = r#"
file_format: "1.2"
tracer_provider:
  processors:
    - batch:
        exporter:
          otlp_grpc:
            endpoint: https://collector:4317
            compression: gzip
            headers_list: api-key=k
            tls:
              ca_file: /etc/otel/ca.pem
              cert_file: /etc/otel/client.pem
              key_file: /etc/otel/client.key
logger_provider:
  processors:
    - batch:
        exporter:
          otlp_http:
            endpoint: https://collector:4318/v1/logs
            tls:
              ca_file: /etc/otel/ca.pem
"#;
    let parsed = parse_with(text, &|_| None).unwrap();
    assert!(parsed.warnings.is_empty(), "{:#?}", parsed.warnings);
    let files = TlsConfig {
        ca_file: Some("/etc/otel/ca.pem".into()),
        cert_file: Some("/etc/otel/client.pem".into()),
        key_file: Some("/etc/otel/client.key".into()),
    };
    assert_eq!(
        parsed.config.tracer_provider.unwrap().processors[0],
        ProcessorConfig::Batch {
            exporter: ExporterConfig::OtlpGrpc(OtlpGrpcConfig {
                endpoint: Some("https://collector:4317".into()),
                headers: vec![],
                headers_list: Some("api-key=k".into()),
                gzip: true,
                timeout_ms: None,
                tls: files,
            }),
            schedule_delay_ms: None,
            export_timeout_ms: None,
            max_queue_size: None,
            max_export_batch_size: None,
        }
    );
    assert_eq!(
        otlp(&parsed.config.logger_provider.unwrap().processors[0])
            .tls
            .ca_file
            .as_deref(),
        Some("/etc/otel/ca.pem")
    );
}

#[test]
fn views_are_read() {
    use oxtel_config::{AggregationConfig, ViewConfig};
    let text = r#"
file_format: "1.2"
meter_provider:
  views:
    - selector:
        instrument_name: http.*
        instrument_type: histogram
        unit: s
        meter_name: oxtel_tower
      stream:
        name: requests
        description: Requests.
        aggregation:
          explicit_bucket_histogram:
            boundaries: [0.1, 1, 10]
            record_min_max: false
        attribute_keys:
          excluded: [url.path]
        aggregation_cardinality_limit: 100
    - selector: {instrument_name: cache.*}
      stream: {aggregation: {drop: }}
    - selector: {instrument_name: latency, meter_version: "1.0"}
      stream:
        aggregation:
          base2_exponential_bucket_histogram: {max_scale: 5}
        attribute_keys: {included: [route]}
"#;
    let parsed = parse_with(text, &|_| None).unwrap();
    assert_eq!(parsed.warnings.len(), 1, "{:#?}", parsed.warnings);
    assert!(parsed.warnings[0].contains("meter_version"));
    let views = parsed.config.meter_provider.unwrap().views;
    assert_eq!(
        views[0],
        ViewConfig {
            instrument_name: Some("http.*".into()),
            instrument_type: Some("histogram".into()),
            unit: Some("s".into()),
            meter_name: Some("oxtel_tower".into()),
            name: Some("requests".into()),
            description: Some("Requests.".into()),
            aggregation: Some(AggregationConfig::ExplicitBucketHistogram {
                boundaries: Some(vec![0.1, 1.0, 10.0]),
                record_min_max: Some(false),
            }),
            attributes_included: None,
            attributes_excluded: Some(vec!["url.path".into()]),
            cardinality_limit: Some(100),
        }
    );
    assert_eq!(views[1].aggregation, Some(AggregationConfig::Drop));
    assert_eq!(
        views[2].aggregation,
        Some(AggregationConfig::Base2ExponentialBucketHistogram {
            max_scale: Some(5),
            max_size: None,
            record_min_max: None,
        })
    );
    assert_eq!(views[2].attributes_included, Some(vec!["route".into()]));

    let bad = "file_format: \"1.2\"\nmeter_provider:\n  views:\n    - stream: {aggregation: {median: }}\n";
    let error = parse_with(bad, &|_| None).unwrap_err().to_string();
    assert!(error.contains("median"), "{error}");
}

#[test]
fn oxtel_s_own_example_reads_without_warnings() {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/otel-config.yaml"
    ))
    .unwrap();
    let parsed = parse_with(&text, &|name| (name == "TENANT").then(|| "acme".into())).unwrap();
    assert!(parsed.warnings.is_empty(), "{:#?}", parsed.warnings);
    let t = parsed.config.tracer_provider.unwrap();
    assert_eq!(
        t.sampler,
        Some(SamplerConfig::ParentBased(Box::new(
            SamplerConfig::TraceIdRatio(0.25)
        )))
    );
    assert_eq!(
        otlp(&t.processors[0]).headers,
        vec![("x-tenant".into(), "acme".into())]
    );
    assert_eq!(parsed.config.meter_provider.unwrap().readers.len(), 2);
}
