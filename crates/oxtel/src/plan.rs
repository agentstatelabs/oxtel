//! What to build: from the standard environment variables, or from a
//! declarative configuration file. Building it is the same either way.

use oxtel_prometheus::Options as PrometheusOptions;
use oxtel_sdk::console::ConsoleExporter;
use oxtel_sdk::export::{BatchConfig, ExportError};
use oxtel_sdk::logs::{LogExporter, LogLimits, LogRecord};
use oxtel_sdk::metrics::{ExemplarFilter, MetricExporter, MetricFamily, PeriodicReaderConfig};
use oxtel_sdk::resource::{Detector, ResourceBuilder};
use oxtel_sdk::trace::{Sampler, SpanData, SpanExporter, SpanLimits};
use oxtel_sdk::{KeyValue, Resource};

use crate::config::{EnvConfig, Exporter, LocalEnv};

#[cfg(feature = "otlp")]
pub(crate) type Otlp = oxtel_otlp::OtlpHttpExporter;

/// Stands in for the OTLP exporter when the `otlp` feature is off. Never
/// constructed.
#[cfg(not(feature = "otlp"))]
#[derive(Clone)]
pub(crate) enum Otlp {}

/// Where a processor or reader sends its data.
#[derive(Clone)]
pub(crate) enum Sink {
    #[cfg_attr(not(feature = "otlp"), allow(dead_code))]
    Otlp(Otlp),
    Console(ConsoleExporter),
}

impl SpanExporter for Sink {
    fn export(&self, batch: &[SpanData], resource: &Resource) -> Result<(), ExportError> {
        match self {
            #[cfg(feature = "otlp")]
            Sink::Otlp(o) => SpanExporter::export(o, batch, resource),
            #[cfg(not(feature = "otlp"))]
            Sink::Otlp(o) => match *o {},
            Sink::Console(c) => SpanExporter::export(c, batch, resource),
        }
    }
}

impl LogExporter for Sink {
    fn export(&self, batch: &[LogRecord], resource: &Resource) -> Result<(), ExportError> {
        match self {
            #[cfg(feature = "otlp")]
            Sink::Otlp(o) => LogExporter::export(o, batch, resource),
            #[cfg(not(feature = "otlp"))]
            Sink::Otlp(o) => match *o {},
            Sink::Console(c) => LogExporter::export(c, batch, resource),
        }
    }
}

impl MetricExporter for Sink {
    fn export(&self, metrics: &[MetricFamily], resource: &Resource) -> Result<(), ExportError> {
        match self {
            #[cfg(feature = "otlp")]
            Sink::Otlp(o) => MetricExporter::export(o, metrics, resource),
            #[cfg(not(feature = "otlp"))]
            Sink::Otlp(o) => match *o {},
            Sink::Console(c) => MetricExporter::export(c, metrics, resource),
        }
    }
}

/// A span or log record processor to add.
pub(crate) enum Proc {
    Batch(Sink, BatchConfig),
    Simple(Sink),
}

/// A metric reader to add.
pub(crate) enum Reader {
    Periodic(Sink, PeriodicReaderConfig),
    Prometheus {
        addr: String,
        options: PrometheusOptions,
    },
}

/// Everything the builder needs from the configuration source. `None`
/// leaves a setting to the provider, which reads the environment; a plan
/// from a file sets everything.
pub(crate) struct Plan {
    pub(crate) disabled: bool,
    pub(crate) resource: Resource,
    pub(crate) trace_context: bool,
    pub(crate) sampler: Option<Sampler>,
    pub(crate) span_limits: Option<SpanLimits>,
    pub(crate) log_limits: Option<LogLimits>,
    pub(crate) span_processors: Vec<Proc>,
    pub(crate) log_processors: Vec<Proc>,
    pub(crate) readers: Vec<Reader>,
    pub(crate) exemplar_filter: Option<ExemplarFilter>,
    pub(crate) cardinality_limit: Option<usize>,
    /// `OXTEL_LOG_FORMAT` and `OXTEL_LOG_OUTPUT`, where set.
    pub(crate) local: LocalEnv,
    pub(crate) warnings: Vec<String>,
}

/// What the program set on the builder about the resource and sampler.
pub(crate) struct Programmed {
    pub(crate) default_service_name: Option<String>,
    pub(crate) attributes: Vec<KeyValue>,
    pub(crate) detectors: Vec<Box<dyn Detector>>,
    pub(crate) resource: Option<Resource>,
    pub(crate) sampler: Option<Sampler>,
}

impl Programmed {
    /// The resource: the program's own, or `base` with the program's
    /// default service name, detectors and attributes (which win).
    fn resource(self, base: ResourceBuilder, extra: Vec<KeyValue>) -> Resource {
        if let Some(r) = self.resource {
            return r;
        }
        let mut b = base;
        if let Some(name) = self.default_service_name {
            b = b.with_default_service_name(name);
        }
        for detector in self.detectors {
            b = b.with_boxed_detector(detector);
        }
        for kv in extra.into_iter().chain(self.attributes) {
            b = b.with_attribute(kv);
        }
        b.build()
    }
}

#[cfg(feature = "otlp")]
fn otlp_from_env() -> Option<Otlp> {
    Some(oxtel_otlp::OtlpHttpExporter::builder().build())
}

#[cfg(not(feature = "otlp"))]
fn otlp_from_env() -> Option<Otlp> {
    None
}

impl Plan {
    /// The plan the `OTEL_*` variables describe.
    pub(crate) fn from_env(
        mut env: EnvConfig,
        env_exporters: bool,
        programmed: Programmed,
    ) -> Plan {
        if !env_exporters {
            env.traces.clear();
            env.metrics.clear();
            env.logs.clear();
        }
        let mut warnings = std::mem::take(&mut env.warnings);
        let wants_otlp = [&env.traces, &env.metrics, &env.logs]
            .iter()
            .any(|e| e.contains(&Exporter::Otlp));
        let otlp = if wants_otlp { otlp_from_env() } else { None };
        if wants_otlp && otlp.is_none() {
            warnings.push("OTLP export needs oxtel's `otlp` feature; ignored".into());
        }
        let console = Sink::Console(ConsoleExporter::stdout());
        let sink = |e: &Exporter| match e {
            Exporter::Otlp => otlp.clone().map(Sink::Otlp),
            Exporter::Console => Some(console.clone()),
            Exporter::Prometheus => None,
        };
        let procs = |exporters: &[Exporter], batch: fn() -> BatchConfig| {
            exporters
                .iter()
                .filter_map(|e| match e {
                    Exporter::Console => Some(Proc::Simple(console.clone())),
                    other => sink(other).map(|s| Proc::Batch(s, batch())),
                })
                .collect()
        };
        let span_processors = procs(&env.traces, BatchConfig::from_env);
        let log_processors = procs(&env.logs, BatchConfig::logs_from_env);
        let readers = env
            .metrics
            .iter()
            .filter_map(|e| match e {
                Exporter::Prometheus => Some(Reader::Prometheus {
                    addr: env.prometheus_addr.clone(),
                    options: PrometheusOptions::default(),
                }),
                other => sink(other).map(|s| Reader::Periodic(s, PeriodicReaderConfig::from_env())),
            })
            .collect();
        let sampler = programmed.sampler.clone();
        Plan {
            disabled: env.disabled,
            resource: programmed.resource(Resource::builder(), Vec::new()),
            trace_context: env.trace_context,
            sampler,
            span_limits: None,
            log_limits: None,
            span_processors,
            log_processors,
            readers,
            exemplar_filter: None,
            cardinality_limit: None,
            local: env.local,
            warnings,
        }
    }

    /// The plan a declarative configuration file describes. Every setting
    /// comes from the file (or the specification's default), never from
    /// other `OTEL_*` variables.
    #[cfg(feature = "config")]
    pub(crate) fn from_config(
        parsed: oxtel_config::Parsed,
        programmed: Programmed,
        local: LocalEnv,
    ) -> Plan {
        use oxtel_config::{ProcessorConfig, ReaderConfig};

        let oxtel_config::Parsed {
            config,
            mut warnings,
        } = parsed;
        let resource = config_resource(&config.resource, programmed, &mut warnings);

        let trace_context = match &config.propagators {
            None => true,
            Some(names) => {
                for name in names {
                    if !matches!(name.as_str(), "tracecontext" | "baggage" | "none") {
                        warnings.push(format!("the `{name}` propagator isn't supported; ignored"));
                    }
                }
                names.iter().any(|n| n == "tracecontext")
            }
        };

        let defaults = config.attribute_limits;
        let tp = config.tracer_provider.unwrap_or_default();
        let lp = config.logger_provider.unwrap_or_default();
        let span_limits = SpanLimits {
            max_attributes: tp
                .limits
                .attribute_count
                .or(defaults.attribute_count)
                .unwrap_or(128),
            max_events: tp.limits.event_count.unwrap_or(128),
            max_links: tp.limits.link_count.unwrap_or(128),
        };
        let log_limits = LogLimits {
            max_attributes: lp
                .limits
                .attribute_count
                .or(defaults.attribute_count)
                .unwrap_or(128),
        };

        let mut sink = |e: &oxtel_config::ExporterConfig, signal: &'static str| match e {
            oxtel_config::ExporterConfig::Console => Some(Sink::Console(ConsoleExporter::stdout())),
            oxtel_config::ExporterConfig::OtlpHttp(o) => otlp_from_config(o, signal, &mut warnings),
        };
        let mut procs = |list: &[ProcessorConfig], signal, defaults: BatchConfig| -> Vec<Proc> {
            list.iter()
                .filter_map(|p| match p {
                    ProcessorConfig::Simple { exporter } => {
                        sink(exporter, signal).map(Proc::Simple)
                    }
                    ProcessorConfig::Batch {
                        exporter,
                        schedule_delay_ms,
                        export_timeout_ms,
                        max_queue_size,
                        max_export_batch_size,
                    } => {
                        let max_queue_size =
                            max_queue_size.unwrap_or(defaults.max_queue_size).max(1);
                        let config = BatchConfig {
                            max_queue_size,
                            scheduled_delay: schedule_delay_ms
                                .map_or(defaults.scheduled_delay, std::time::Duration::from_millis),
                            max_export_batch_size: max_export_batch_size
                                .unwrap_or(defaults.max_export_batch_size)
                                .clamp(1, max_queue_size),
                            export_timeout: export_timeout_ms
                                .map_or(defaults.export_timeout, std::time::Duration::from_millis),
                        };
                        sink(exporter, signal).map(|s| Proc::Batch(s, config))
                    }
                })
                .collect()
        };
        let span_processors = procs(&tp.processors, "traces", BatchConfig::default());
        let log_processors = procs(&lp.processors, "logs", BatchConfig::logs());

        let mp = config.meter_provider.unwrap_or_default();
        let mut readers = Vec::new();
        for reader in &mp.readers {
            match reader {
                ReaderConfig::Periodic {
                    exporter,
                    interval_ms,
                    timeout_ms,
                } => {
                    let d = PeriodicReaderConfig::default();
                    let config = PeriodicReaderConfig {
                        interval: interval_ms
                            .filter(|&ms| ms > 0)
                            .map_or(d.interval, std::time::Duration::from_millis),
                        timeout: timeout_ms
                            .filter(|&ms| ms > 0)
                            .map_or(d.timeout, std::time::Duration::from_millis),
                    };
                    let exporter = match exporter {
                        oxtel_config::ExporterConfig::Console => {
                            Some(Sink::Console(ConsoleExporter::stdout()))
                        }
                        oxtel_config::ExporterConfig::OtlpHttp(o) => {
                            otlp_from_config(o, "metrics", &mut warnings)
                        }
                    };
                    if let Some(s) = exporter {
                        readers.push(Reader::Periodic(s, config));
                    }
                }
                ReaderConfig::Prometheus {
                    host,
                    port,
                    without_scope_info,
                    without_target_info,
                } => {
                    let host = host.clone().unwrap_or_else(|| "localhost".into());
                    let port = port.unwrap_or(9464);
                    let addr = if host.contains(':') && !host.starts_with('[') {
                        format!("[{host}]:{port}")
                    } else {
                        format!("{host}:{port}")
                    };
                    readers.push(Reader::Prometheus {
                        addr,
                        options: PrometheusOptions {
                            scope_labels: !without_scope_info,
                            target_info: !without_target_info,
                        },
                    });
                }
            }
        }
        let exemplar_filter = match mp.exemplar_filter.as_deref() {
            None | Some("trace_based") => ExemplarFilter::TraceBased,
            Some("always_on") => ExemplarFilter::AlwaysOn,
            Some("always_off") => ExemplarFilter::AlwaysOff,
            Some(other) => {
                warnings.push(format!(
                    "the `{other}` exemplar filter isn't supported; using trace_based"
                ));
                ExemplarFilter::TraceBased
            }
        };

        Plan {
            disabled: config.disabled,
            resource,
            trace_context,
            sampler: Some(config_sampler(tp.sampler)),
            span_limits: Some(span_limits),
            log_limits: Some(log_limits),
            span_processors,
            log_processors,
            readers,
            exemplar_filter: Some(exemplar_filter),
            cardinality_limit: mp.cardinality_limit,
            local,
            warnings,
        }
    }
}

/// The sampler a configuration names: parent-based always-on by default.
#[cfg(feature = "config")]
fn config_sampler(sampler: Option<oxtel_config::SamplerConfig>) -> Sampler {
    use oxtel_config::SamplerConfig as S;
    fn convert(s: S) -> Sampler {
        match s {
            S::AlwaysOn => Sampler::AlwaysOn,
            S::AlwaysOff => Sampler::AlwaysOff,
            S::TraceIdRatio(r) => Sampler::TraceIdRatio(r.clamp(0.0, 1.0)),
            S::ParentBased(root) => Sampler::ParentBased(Box::new(convert(*root))),
        }
    }
    sampler.map_or_else(Sampler::default, convert)
}

/// A resource from the configuration: detected attributes, then
/// `attributes_list`, then `attributes`, then the program's own.
#[cfg(feature = "config")]
fn config_resource(
    r: &oxtel_config::ResourceConfig,
    programmed: Programmed,
    warnings: &mut Vec<String>,
) -> Resource {
    use oxtel_config::AttributeValue as A;
    use oxtel_sdk::resource::{
        ContainerDetector, HostDetector, KubernetesDetector, OsDetector, ProcessDetector,
        ServiceInstanceDetector, parse_resource_attributes,
    };
    use oxtel_sdk::value::Value;

    /// The configuration's `service` detector also reads `OTEL_SERVICE_NAME`.
    struct ServiceName;
    impl Detector for ServiceName {
        fn detect(&self) -> Vec<KeyValue> {
            std::env::var("OTEL_SERVICE_NAME")
                .ok()
                .filter(|n| !n.trim().is_empty())
                .map(|n| vec![KeyValue::new("service.name", n.trim().to_string())])
                .unwrap_or_default()
        }
    }

    let mut b = Resource::builder().without_env();
    for name in r.detectors.iter().flatten() {
        b = match name.as_str() {
            "service" => b
                .with_detector(ServiceInstanceDetector)
                .with_detector(ServiceName),
            "host" => b.with_detector(HostDetector),
            "os" => b.with_detector(OsDetector),
            "process" => b.with_detector(ProcessDetector),
            "container" => b.with_detector(ContainerDetector),
            "k8s" | "kubernetes" => b.with_detector(KubernetesDetector),
            other => {
                warnings.push(format!(
                    "the `{other}` resource detector isn't supported; ignored"
                ));
                b
            }
        };
    }
    if let Some(url) = &r.schema_url {
        b = b.with_schema_url(url.clone());
    }
    let mut extra: Vec<KeyValue> = r
        .attributes_list
        .as_deref()
        .map(parse_resource_attributes)
        .unwrap_or_default();
    for (name, value) in &r.attributes {
        let value = match value {
            A::String(s) => Value::String(s.clone()),
            A::Bool(v) => Value::Bool(*v),
            A::Int(v) => Value::I64(*v),
            A::Double(v) => Value::F64(*v),
            A::StringArray(v) => Value::Array(v.iter().cloned().map(Value::String).collect()),
            A::BoolArray(v) => Value::Array(v.iter().copied().map(Value::Bool).collect()),
            A::IntArray(v) => Value::Array(v.iter().copied().map(Value::I64).collect()),
            A::DoubleArray(v) => Value::Array(v.iter().copied().map(Value::F64).collect()),
        };
        extra.push(KeyValue::new(name.clone(), value));
    }
    programmed.resource(b, extra)
}

/// An OTLP exporter for one signal, as the configuration describes it.
#[cfg(all(feature = "config", feature = "otlp"))]
fn otlp_from_config(
    o: &oxtel_config::OtlpHttpConfig,
    signal: &'static str,
    _warnings: &mut Vec<String>,
) -> Option<Sink> {
    use oxtel_otlp::{OtlpHttpExporterBuilder, Protocol, Signal, parse_headers};
    let which = match signal {
        "traces" => Signal::Traces,
        "metrics" => Signal::Metrics,
        _ => Signal::Logs,
    };
    let url = o
        .endpoint
        .clone()
        .unwrap_or_else(|| format!("http://localhost:4318/v1/{signal}"));
    let mut b = OtlpHttpExporterBuilder::new()
        .with_url(which, url)
        .with_gzip(o.gzip)
        .with_protocol(if o.json {
            Protocol::HttpJson
        } else {
            Protocol::HttpProtobuf
        });
    if let Some(ms) = o.timeout_ms.filter(|&ms| ms > 0) {
        b = b.with_timeout(std::time::Duration::from_millis(ms));
    }
    let mut headers = o
        .headers_list
        .as_deref()
        .map(parse_headers)
        .unwrap_or_default();
    for (name, value) in &o.headers {
        headers.retain(|(k, _)| !k.eq_ignore_ascii_case(name));
        headers.push((name.clone(), value.clone()));
    }
    for (name, value) in headers {
        b = b.with_header(name, value);
    }
    Some(Sink::Otlp(b.build()))
}

#[cfg(all(feature = "config", not(feature = "otlp")))]
fn otlp_from_config(
    _: &oxtel_config::OtlpHttpConfig,
    _: &'static str,
    warnings: &mut Vec<String>,
) -> Option<Sink> {
    warnings.push("OTLP export needs oxtel's `otlp` feature; ignored".into());
    None
}
