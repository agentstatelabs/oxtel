//! Assembling the pipelines from the environment and the program's own
//! choices.

use std::fmt;
use std::io::Write;
use std::time::Duration;

use oxtel_prometheus::{PrometheusExporter, PrometheusServer};
use oxtel_sdk::console::ConsoleExporter;
use oxtel_sdk::logs::{LocalFormat, LocalLayer, LoggerProvider, LoggerProviderBuilder};
use oxtel_sdk::metrics::{MeterProvider, MeterProviderBuilder, MetricsRecorder};
use oxtel_sdk::trace::{Sampler, TracerProvider, TracerProviderBuilder};
use oxtel_sdk::{KeyValue, Resource};
use tracing::Dispatch;
use tracing_subscriber::filter::Targets;
use tracing_subscriber::layer::{Layer, SubscriberExt};
use tracing_subscriber::registry::Registry;

use crate::config::{EnvConfig, Exporter};
use crate::filter::LogFilter;
use crate::guard::Guard;

#[cfg(feature = "otlp")]
type Otlp = oxtel_otlp::OtlpHttpExporter;

/// Stands in for the OTLP exporter when the `otlp` feature is off. Never
/// constructed.
#[cfg(not(feature = "otlp"))]
#[derive(Clone)]
enum Otlp {}

#[cfg(not(feature = "otlp"))]
mod no_otlp {
    use oxtel_sdk::Resource;
    use oxtel_sdk::export::ExportError;
    use oxtel_sdk::logs::{LogExporter, LogRecord};
    use oxtel_sdk::metrics::{MetricExporter, MetricFamily};
    use oxtel_sdk::trace::{SpanData, SpanExporter};

    impl SpanExporter for super::Otlp {
        fn export(&self, _: &[SpanData], _: &Resource) -> Result<(), ExportError> {
            match *self {}
        }
    }
    impl LogExporter for super::Otlp {
        fn export(&self, _: &[LogRecord], _: &Resource) -> Result<(), ExportError> {
            match *self {}
        }
    }
    impl MetricExporter for super::Otlp {
        fn export(&self, _: &[MetricFamily], _: &Resource) -> Result<(), ExportError> {
            match *self {}
        }
    }
}

type BoxedLayer = Box<dyn Layer<Registry> + Send + Sync>;
type Hook<B> = Box<dyn FnOnce(B) -> B>;

/// Configures oxtel. Start with [`oxtel::builder`](crate::builder).
///
/// The standard `OTEL_*` variables choose the exporters, the resource, the
/// sampler, batching and limits. What's set here adds to them, or overrides
/// them where noted.
#[must_use = "a builder does nothing until `install` or `build`"]
pub struct Builder {
    default_service_name: Option<String>,
    attributes: Vec<KeyValue>,
    resource: Option<Resource>,
    sampler: Option<Sampler>,
    traces_filter: Option<String>,
    logs_filter: Option<String>,
    layers: Vec<BoxedLayer>,
    trace_hooks: Vec<Hook<TracerProviderBuilder>>,
    log_hooks: Vec<Hook<LoggerProviderBuilder>>,
    metric_hooks: Vec<Hook<MeterProviderBuilder>>,
    env_exporters: bool,
    pull_metrics: bool,
    sdk_metrics: bool,
    shutdown_timeout: Duration,
    local_format: Option<LocalFormat>,
    local_filter: Option<String>,
    local_writer: Option<Box<dyn Write + Send>>,
}

impl Default for Builder {
    fn default() -> Self {
        Builder {
            default_service_name: None,
            attributes: Vec::new(),
            resource: None,
            sampler: None,
            traces_filter: None,
            logs_filter: None,
            layers: Vec::new(),
            trace_hooks: Vec::new(),
            log_hooks: Vec::new(),
            metric_hooks: Vec::new(),
            env_exporters: true,
            pull_metrics: false,
            sdk_metrics: false,
            shutdown_timeout: Duration::from_secs(5),
            local_format: None,
            local_filter: None,
            local_writer: None,
        }
    }
}

impl Builder {
    /// The service name, unless `OTEL_SERVICE_NAME` or
    /// `OTEL_RESOURCE_ATTRIBUTES` names one.
    pub fn with_default_service_name(mut self, name: impl Into<String>) -> Self {
        self.default_service_name = Some(name.into());
        self
    }

    /// A resource attribute, overriding the environment's.
    pub fn with_resource_attribute(mut self, attribute: KeyValue) -> Self {
        self.attributes.push(attribute);
        self
    }

    /// Use exactly this resource, ignoring the environment's and the two
    /// settings above.
    pub fn with_resource(mut self, resource: Resource) -> Self {
        self.resource = Some(resource);
        self
    }

    /// The sampler, overriding `OTEL_TRACES_SAMPLER`.
    pub fn with_sampler(mut self, sampler: Sampler) -> Self {
        self.sampler = Some(sampler);
        self
    }

    /// Which spans (and span events) to record, as directives like
    /// `info,my_app=debug,hyper=warn`. Default: `RUST_LOG`, or `info`.
    pub fn with_traces_filter(mut self, directives: impl Into<String>) -> Self {
        self.traces_filter = Some(directives.into());
        self
    }

    /// Which events to export as log records, in the same syntax. Default:
    /// `RUST_LOG`, or `info`.
    pub fn with_logs_filter(mut self, directives: impl Into<String>) -> Self {
        self.logs_filter = Some(directives.into());
        self
    }

    /// Add a `tracing` layer of your own, such as a formatter for local
    /// output. oxtel's filters don't apply to it; give it its own.
    pub fn with_layer(mut self, layer: impl Layer<Registry> + Send + Sync) -> Self {
        self.layers.push(layer.boxed());
        self
    }

    /// Customise the trace pipeline: add exporters or processors, set
    /// limits. Applied after the environment's exporters are added.
    pub fn with_traces(
        mut self,
        hook: impl FnOnce(TracerProviderBuilder) -> TracerProviderBuilder + 'static,
    ) -> Self {
        self.trace_hooks.push(Box::new(hook));
        self
    }

    /// Customise the log pipeline. With a hook, the pipeline exists even if
    /// `OTEL_LOGS_EXPORTER` is `none`.
    pub fn with_logs(
        mut self,
        hook: impl FnOnce(LoggerProviderBuilder) -> LoggerProviderBuilder + 'static,
    ) -> Self {
        self.log_hooks.push(Box::new(hook));
        self
    }

    /// Customise the metrics pipeline: add exporters, set buckets or the
    /// cardinality limit. With a hook, the pipeline exists even if
    /// `OTEL_METRICS_EXPORTER` is `none`.
    pub fn with_metrics(
        mut self,
        hook: impl FnOnce(MeterProviderBuilder) -> MeterProviderBuilder + 'static,
    ) -> Self {
        self.metric_hooks.push(Box::new(hook));
        self
    }

    /// Ignore `OTEL_TRACES_EXPORTER`, `OTEL_METRICS_EXPORTER` and
    /// `OTEL_LOGS_EXPORTER`: export only where the hooks say.
    pub fn without_env_exporters(mut self) -> Self {
        self.env_exporters = false;
        self
    }

    /// Record metrics even when no exporter wants them, to serve
    /// `/metrics` yourself from [`Guard::meter_provider`].
    pub fn with_pull_metrics(mut self) -> Self {
        self.pull_metrics = true;
        self
    }

    /// Record oxtel's own `otel.sdk.*` metrics (see
    /// [`oxtel_sdk::sdk_metrics`]). Default: off.
    pub fn with_sdk_metrics(mut self, enabled: bool) -> Self {
        self.sdk_metrics = enabled;
        self
    }

    /// Also write each event locally, to standard error, in `format`: text
    /// for people, or flat or ECS JSON for log shippers. `OXTEL_LOG_FORMAT`
    /// (`text`, `json`, `ecs` or `none`) overrides it. Local output keeps
    /// working when `OTEL_SDK_DISABLED` turns the rest off.
    pub fn with_local_logs(mut self, format: LocalFormat) -> Self {
        self.local_format = Some(format);
        self
    }

    /// Which events to write locally. Default: the log filter.
    pub fn with_local_logs_filter(mut self, directives: impl Into<String>) -> Self {
        self.local_filter = Some(directives.into());
        self
    }

    /// Write local output to `out` instead of standard error.
    pub fn with_local_logs_writer(mut self, out: impl Write + Send + 'static) -> Self {
        self.local_writer = Some(Box::new(out));
        self
    }

    /// How long dropping the guard waits for the pipelines to finish
    /// exporting. Default: 5 s.
    pub fn with_shutdown_timeout(mut self, timeout: Duration) -> Self {
        self.shutdown_timeout = timeout;
        self
    }

    /// Build the pipelines and install them: the global `tracing`
    /// subscriber, the global `metrics` recorder and, with the `log`
    /// feature, the bridge from the `log` crate. Keep the guard alive.
    pub fn install(self) -> Result<Guard, Error> {
        self.build().install()
    }

    /// Build the pipelines without installing anything, to install them
    /// yourself or use them in a test.
    // Without the `otlp` feature, the OTLP arms below can't be reached.
    #[cfg_attr(not(feature = "otlp"), allow(unreachable_code))]
    pub fn build(self) -> Telemetry {
        let mut env = EnvConfig::from_env();
        if !self.env_exporters {
            env.traces.clear();
            env.metrics.clear();
            env.logs.clear();
        }
        let mut warnings = std::mem::take(&mut env.warnings);
        oxtel_sdk::sdk_metrics::set_enabled(self.sdk_metrics);
        let resource = self.resource.unwrap_or_else(|| {
            let mut b = Resource::builder();
            if let Some(name) = self.default_service_name {
                b = b.with_default_service_name(name);
            }
            for kv in self.attributes {
                b = b.with_attribute(kv);
            }
            b.build()
        });
        let local_format = match env.local_format {
            Some(choice) => choice,
            None => self.local_format,
        };
        let local = |events: Targets, spans: Targets, writer: Option<Box<dyn Write + Send>>| {
            local_format.map(|format| {
                let mut layer = LocalLayer::new(format).with_resource(&resource);
                if let Some(out) = writer {
                    layer = layer.with_writer(out);
                }
                layer.with_filter(LogFilter { events, spans }).boxed()
            })
        };
        let local_directives = directives(
            "local logs",
            self.local_filter.or_else(|| self.logs_filter.clone()),
            &mut warnings,
        );
        if env.disabled {
            let mut layers: Vec<BoxedLayer> = Vec::new();
            layers.extend(local(
                local_directives.clone(),
                local_directives,
                self.local_writer,
            ));
            layers.extend(self.layers);
            return Telemetry {
                dispatch: Dispatch::new(tracing_subscriber::registry().with(layers)),
                recorder: None,
                guard: Guard {
                    traces: None,
                    logs: None,
                    metrics: None,
                    prometheus: None,
                    timeout: self.shutdown_timeout,
                },
                warnings,
                trace_context: false,
            };
        }

        let wants_otlp = [&env.traces, &env.metrics, &env.logs]
            .iter()
            .any(|e| e.contains(&Exporter::Otlp));
        #[cfg(feature = "otlp")]
        let otlp: Option<Otlp> =
            wants_otlp.then(|| oxtel_otlp::OtlpHttpExporter::builder().build());
        #[cfg(not(feature = "otlp"))]
        let otlp: Option<Otlp> = {
            if wants_otlp {
                warnings.push("OTLP export needs oxtel's `otlp` feature; ignored".into());
            }
            None
        };
        let console = ConsoleExporter::stdout();
        let traces_directives = directives("traces", self.traces_filter, &mut warnings);
        let logs_directives = directives("logs", self.logs_filter, &mut warnings);

        let logs = (!env.logs.is_empty() || !self.log_hooks.is_empty()).then(|| {
            let mut b = LoggerProvider::builder().with_resource(resource.clone());
            for exporter in &env.logs {
                match (exporter, &otlp) {
                    (Exporter::Otlp, Some(o)) => b = b.with_batch_exporter(o.clone()),
                    (Exporter::Console, _) => b = b.with_simple_exporter(console.clone()),
                    _ => {}
                }
            }
            for hook in self.log_hooks {
                b = hook(b);
            }
            b.build()
        });

        // Events become log records when there's a log pipeline, and span
        // events when there isn't, so they aren't lost.
        let mut b = TracerProvider::builder()
            .with_resource(resource.clone())
            .with_span_events(logs.is_none());
        if let Some(sampler) = self.sampler {
            b = b.with_sampler(sampler);
        }
        for exporter in &env.traces {
            match (exporter, &otlp) {
                (Exporter::Otlp, Some(o)) => b = b.with_batch_exporter(o.clone()),
                (Exporter::Console, _) => b = b.with_simple_exporter(console.clone()),
                _ => {}
            }
        }
        for hook in self.trace_hooks {
            b = hook(b);
        }
        let traces = b.build();

        let mut prometheus = None;
        let metrics = (!env.metrics.is_empty()
            || !self.metric_hooks.is_empty()
            || self.pull_metrics)
            .then(|| {
                let mut b = MeterProvider::builder().with_resource(resource.clone());
                for exporter in &env.metrics {
                    match (exporter, &otlp) {
                        (Exporter::Otlp, Some(o)) => b = b.with_periodic_exporter(o.clone()),
                        (Exporter::Console, _) => b = b.with_periodic_exporter(console.clone()),
                        _ => {}
                    }
                }
                for hook in self.metric_hooks {
                    b = hook(b);
                }
                let meters = b.build();
                if env.metrics.contains(&Exporter::Prometheus) {
                    let exporter = PrometheusExporter::new(meters.clone(), resource.clone());
                    match PrometheusServer::serve(env.prometheus_addr.as_str(), exporter) {
                        Ok(server) => prometheus = Some(server),
                        Err(e) => warnings.push(format!(
                            "couldn't serve Prometheus metrics on {}: {e}",
                            env.prometheus_addr
                        )),
                    }
                }
                meters
            });

        let mut layers: Vec<BoxedLayer> = vec![
            traces
                .layer()
                .with_filter(traces_directives.clone())
                .boxed(),
        ];
        if let Some(logs) = &logs {
            layers.push(
                logs.layer()
                    .with_filter(LogFilter {
                        events: logs_directives,
                        spans: traces_directives.clone(),
                    })
                    .boxed(),
            );
        }
        layers.extend(local(
            local_directives,
            traces_directives,
            self.local_writer,
        ));
        layers.extend(self.layers);
        Telemetry {
            dispatch: Dispatch::new(tracing_subscriber::registry().with(layers)),
            recorder: metrics.as_ref().map(MeterProvider::recorder),
            guard: Guard {
                traces: Some(traces),
                logs,
                metrics,
                prometheus,
                timeout: self.shutdown_timeout,
            },
            warnings,
            trace_context: env.trace_context,
        }
    }
}

/// `explicit` directives, else `RUST_LOG`, else `info`. Directives that
/// don't parse are reported and replaced by `info`.
fn directives(signal: &str, explicit: Option<String>, warnings: &mut Vec<String>) -> Targets {
    let source = explicit
        .or_else(|| std::env::var("RUST_LOG").ok())
        .filter(|d| !d.trim().is_empty())
        .unwrap_or_else(|| "info".into());
    source.parse().unwrap_or_else(|e| {
        warnings.push(format!(
            "the {signal} filter {source:?} doesn't parse ({e}); using `info`"
        ));
        "info".parse().expect("`info` parses")
    })
}

/// Pipelines built but not yet installed. See [`Builder::build`].
pub struct Telemetry {
    dispatch: Dispatch,
    recorder: Option<MetricsRecorder>,
    guard: Guard,
    warnings: Vec<String>,
    trace_context: bool,
}

impl Telemetry {
    /// The `tracing` dispatcher carrying oxtel's layers, and yours.
    pub fn dispatch(&self) -> &Dispatch {
        &self.dispatch
    }

    /// The `metrics` recorder, if a metrics pipeline exists.
    pub fn recorder(&self) -> Option<&MetricsRecorder> {
        self.recorder.as_ref()
    }

    /// The guard.
    pub fn guard(&self) -> &Guard {
        &self.guard
    }

    /// Configuration oxtel couldn't honour, such as an unsupported
    /// exporter. [`install`](Telemetry::install) logs these as warnings.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Keep only the guard, dropping the dispatcher and recorder.
    pub fn into_guard(self) -> Guard {
        self.guard
    }

    /// Install the dispatcher and recorder globally, and the bridge from the
    /// `log` crate with the `log` feature; then log the warnings.
    pub fn install(self) -> Result<Guard, Error> {
        tracing::dispatcher::set_global_default(self.dispatch)
            .map_err(|_| Error::SubscriberAlreadySet)?;
        if let Some(recorder) = self.recorder {
            metrics::set_global_recorder(recorder).map_err(|_| Error::RecorderAlreadySet)?;
        }
        #[cfg(feature = "log")]
        {
            let level = match tracing_core::LevelFilter::current().into_level() {
                Some(tracing::Level::TRACE) => tracing_log::log::LevelFilter::Trace,
                Some(tracing::Level::DEBUG) => tracing_log::log::LevelFilter::Debug,
                Some(tracing::Level::INFO) => tracing_log::log::LevelFilter::Info,
                Some(tracing::Level::WARN) => tracing_log::log::LevelFilter::Warn,
                Some(tracing::Level::ERROR) => tracing_log::log::LevelFilter::Error,
                None => tracing_log::log::LevelFilter::Off,
            };
            if tracing_log::LogTracer::builder()
                .with_max_level(level)
                .init()
                .is_err()
            {
                tracing::warn!(
                    target: "oxtel",
                    "another `log` logger is installed; records from the `log` crate won't reach oxtel"
                );
            }
        }
        crate::propagation::set_trace_context(self.trace_context);
        for warning in &self.warnings {
            tracing::warn!(target: "oxtel", "{warning}");
        }
        Ok(self.guard)
    }
}

/// Why oxtel couldn't be installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// A global `tracing` subscriber was already installed.
    SubscriberAlreadySet,
    /// A global `metrics` recorder was already installed.
    RecorderAlreadySet,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Error::SubscriberAlreadySet => "a global tracing subscriber is already installed",
            Error::RecorderAlreadySet => "a global metrics recorder is already installed",
        })
    }
}

impl std::error::Error for Error {}
