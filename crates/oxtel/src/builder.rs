//! Assembling the pipelines from the environment and the program's own
//! choices.

use std::fmt;
use std::io::{IsTerminal, Write};
use std::time::Duration;

use oxtel_prometheus::{PrometheusExporter, PrometheusServer};
use oxtel_sdk::logs::{LocalFormat, LocalLayer, LoggerProvider, LoggerProviderBuilder};
use oxtel_sdk::metrics::{MeterProvider, MeterProviderBuilder, MetricsRecorder};
use oxtel_sdk::resource::Detector;
use oxtel_sdk::trace::{Sampler, TracerProvider, TracerProviderBuilder};
use oxtel_sdk::{KeyValue, Resource};
use tracing::Dispatch;
use tracing_subscriber::filter::Targets;
use tracing_subscriber::layer::{Layer, SubscriberExt};
use tracing_subscriber::registry::Registry;

use crate::config::{EnvConfig, Local, LocalEnv, LogOutput};
use crate::filter::LogFilter;
use crate::guard::Guard;
use crate::plan::{Plan, Proc, Programmed, Reader};
use crate::summary::{Dest, Kind, Summary};

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
    detectors: Vec<Box<dyn Detector>>,
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
    local: Local,
    local_output: LogOutput,
    local_filter: Option<String>,
    local_writer: Option<Box<dyn Write + Send>>,
}

impl Default for Builder {
    fn default() -> Self {
        Builder {
            default_service_name: None,
            attributes: Vec::new(),
            detectors: Vec::new(),
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
            local: Local::Auto,
            local_output: LogOutput::Stderr,
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

    /// Detect more resource attributes, such as a cloud provider's, after
    /// the built-in detectors (which `OTEL_EXPERIMENTAL_RESOURCE_DETECTORS`
    /// chooses).
    pub fn with_resource_detector(mut self, detector: impl Detector + 'static) -> Self {
        self.detectors.push(Box::new(detector));
        self
    }

    /// Use exactly this resource, ignoring the environment, the detectors
    /// and the settings above.
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

    /// Write local log lines in `format`: text for people, or flat or ECS
    /// JSON for log shippers. Without this, lines are text when they go to
    /// a terminal and flat JSON otherwise. `OXTEL_LOG_FORMAT` (`auto`,
    /// `text`, `json`, `ecs` or `none`) overrides it. Local output keeps
    /// working when `OTEL_SDK_DISABLED` turns the rest off.
    pub fn with_local_logs(mut self, format: LocalFormat) -> Self {
        self.local = Local::Format(format);
        self
    }

    /// Don't write log lines locally, unless `OXTEL_LOG_FORMAT` asks for
    /// them.
    pub fn without_local_logs(mut self) -> Self {
        self.local = Local::Off;
        self
    }

    /// Write local log lines to `output`. Default: standard error.
    /// `OXTEL_LOG_OUTPUT` (`stderr` or `stdout`) overrides it.
    pub fn with_local_logs_output(mut self, output: LogOutput) -> Self {
        self.local_output = output;
        self
    }

    /// Which events to write locally. Default: the log filter.
    pub fn with_local_logs_filter(mut self, directives: impl Into<String>) -> Self {
        self.local_filter = Some(directives.into());
        self
    }

    /// Write local log lines to `out` instead, as flat JSON unless a format
    /// is chosen. `OXTEL_LOG_OUTPUT` overrides it.
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
        self.build()?.install()
    }

    /// Build the pipelines without installing anything, to install them
    /// yourself or use them in a test.
    ///
    /// With `OTEL_CONFIG_FILE` set (and the `config` feature), the
    /// pipelines come from that declarative configuration file, and the
    /// other `OTEL_*` variables are used only where the file substitutes
    /// them. A file that can't be read is an error.
    pub fn build(self) -> Result<Telemetry, Error> {
        oxtel_sdk::sdk_metrics::set_enabled(self.sdk_metrics);
        let programmed = Programmed {
            default_service_name: self.default_service_name,
            attributes: self.attributes,
            detectors: self.detectors,
            resource: self.resource,
            sampler: self.sampler,
        };
        let config_file = std::env::var("OTEL_CONFIG_FILE")
            .ok()
            .filter(|p| !p.trim().is_empty());
        let plan = match config_file.as_deref() {
            #[cfg(feature = "config")]
            Some(path) => {
                let parsed = oxtel_config::from_file(path.trim())
                    .map_err(|e| Error::Config(e.to_string()))?;
                let (local, local_warnings) = EnvConfig::local_from_env();
                let mut plan = Plan::from_config(parsed, programmed, local);
                plan.warnings.extend(local_warnings);
                plan
            }
            #[cfg(not(feature = "config"))]
            Some(_) => {
                let mut plan =
                    Plan::from_env(EnvConfig::from_env(), self.env_exporters, programmed);
                plan.warnings.push(
                    "OTEL_CONFIG_FILE needs oxtel's `config` feature; using the environment".into(),
                );
                plan
            }
            None => Plan::from_env(EnvConfig::from_env(), self.env_exporters, programmed),
        };
        let Plan {
            disabled,
            resource,
            trace_context,
            sampler,
            span_limits,
            log_limits,
            span_processors,
            log_processors,
            readers,
            exemplar_filter,
            cardinality_limit,
            local: env_local,
            mut warnings,
        } = plan;

        let local_out = local_output(env_local, self.local, self.local_output, self.local_writer);
        let mut summary = Summary {
            #[cfg(feature = "config")]
            config_file: config_file.map(|p| p.trim().to_string()),
            local: local_out.as_ref().map(|(format, _, place)| {
                let format = match format {
                    LocalFormat::Text => "text",
                    LocalFormat::Json => "JSON",
                    LocalFormat::Ecs => "ECS JSON",
                };
                format!("{place}, as {format}")
            }),
            disabled,
            ..Summary::default()
        };
        let local = |events: Targets, spans: Targets| {
            local_out.map(|(format, out, _)| {
                LocalLayer::new(format)
                    .with_resource(&resource)
                    .with_writer(out)
                    .with_filter(LogFilter { events, spans })
                    .boxed()
            })
        };
        let local_directives = directives(
            "local logs",
            self.local_filter.or_else(|| self.logs_filter.clone()),
            &mut warnings,
        );
        if disabled {
            let mut layers: Vec<BoxedLayer> = Vec::new();
            layers.extend(local(local_directives.clone(), local_directives));
            layers.extend(self.layers);
            return Ok(Telemetry {
                dispatch: Dispatch::new(tracing_subscriber::registry().with(layers)),
                recorder: None,
                guard: Guard {
                    traces: None,
                    logs: None,
                    metrics: None,
                    prometheus: None,
                    timeout: self.shutdown_timeout,
                    summary: summary.line(),
                },
                warnings,
                trace_context: false,
            });
        }

        let traces_directives = directives("traces", self.traces_filter, &mut warnings);
        let logs_directives = directives("logs", self.logs_filter, &mut warnings);

        let dests = |procs: &[Proc], kind, hooks: bool| {
            let mut dests: Vec<Dest> = procs.iter().map(|p| Dest::of_proc(p, kind)).collect();
            if hooks {
                dests.push(Dest::Program);
            }
            dests
        };
        summary.traces = dests(&span_processors, Kind::Traces, !self.trace_hooks.is_empty());
        summary.logs = dests(&log_processors, Kind::Logs, !self.log_hooks.is_empty());
        summary.metrics = readers
            .iter()
            .filter_map(|r| match r {
                Reader::Periodic(sink, _) => Some(Dest::of_sink(sink, Kind::Metrics)),
                Reader::Prometheus { .. } => None,
            })
            .collect();
        if !self.metric_hooks.is_empty() || self.pull_metrics {
            summary.metrics.push(Dest::Program);
        }

        let logs = (!log_processors.is_empty() || !self.log_hooks.is_empty()).then(|| {
            let mut b = LoggerProvider::builder().with_resource(resource.clone());
            if let Some(limits) = log_limits {
                b = b.with_limits(limits);
            }
            for p in log_processors {
                b = match p {
                    Proc::Batch(sink, config) => b.with_batch_exporter_config(sink, config),
                    Proc::Simple(sink) => b.with_simple_exporter(sink),
                };
            }
            for hook in self.log_hooks {
                b = hook(b);
            }
            b.build()
        });

        // Events become log records when there's a log pipeline, and span
        // events when there isn't, so they aren't lost. With no span
        // exporter either, there's nowhere for span events to go.
        let spans_go_somewhere = !span_processors.is_empty() || !self.trace_hooks.is_empty();
        let mut b = TracerProvider::builder()
            .with_resource(resource.clone())
            .with_span_events(logs.is_none() && spans_go_somewhere);
        if let Some(sampler) = sampler {
            b = b.with_sampler(sampler);
        }
        if let Some(limits) = span_limits {
            b = b.with_span_limits(limits);
        }
        for p in span_processors {
            b = match p {
                Proc::Batch(sink, config) => b.with_batch_exporter_config(sink, config),
                Proc::Simple(sink) => b.with_simple_exporter(sink),
            };
        }
        for hook in self.trace_hooks {
            b = hook(b);
        }
        let traces = b.build();

        let mut prometheus = None;
        let metrics = (!readers.is_empty() || !self.metric_hooks.is_empty() || self.pull_metrics)
            .then(|| {
                let mut b = MeterProvider::builder().with_resource(resource.clone());
                if let Some(filter) = exemplar_filter {
                    b = b.with_exemplar_filter(filter);
                }
                if let Some(limit) = cardinality_limit {
                    b = b.with_cardinality_limit(limit);
                }
                let mut pull = Vec::new();
                for reader in readers {
                    match reader {
                        Reader::Periodic(sink, config) => {
                            b = b.with_periodic_exporter_config(sink, config);
                        }
                        Reader::Prometheus { addr, options } => pull.push((addr, options)),
                    }
                }
                for hook in self.metric_hooks {
                    b = hook(b);
                }
                let meters = b.build();
                if let Some((addr, options)) = pull.into_iter().next() {
                    let exporter = PrometheusExporter::new(meters.clone(), resource.clone())
                        .with_options(options);
                    match PrometheusServer::serve(addr.as_str(), exporter) {
                        Ok(server) => prometheus = Some(server),
                        Err(e) => warnings
                            .push(format!("couldn't serve Prometheus metrics on {addr}: {e}")),
                    }
                }
                meters
            });
        if let Some(server) = &prometheus {
            summary.metrics.push(Dest::Prometheus(server.local_addr()));
        }
        let grpc_port = [&summary.traces, &summary.metrics, &summary.logs]
            .into_iter()
            .flatten()
            .find(|d| d.uses_the_grpc_port());
        if let Some(Dest::Exporter(destination)) = grpc_port {
            warnings.push(format!(
                "{destination} uses port 4317, where collectors take OTLP over gRPC, which oxtel \
                 doesn't speak yet; OTLP over HTTP is usually on port 4318"
            ));
        }

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
        layers.extend(local(local_directives, traces_directives));
        layers.extend(self.layers);
        Ok(Telemetry {
            dispatch: Dispatch::new(tracing_subscriber::registry().with(layers)),
            recorder: metrics.as_ref().map(MeterProvider::recorder),
            guard: Guard {
                traces: Some(traces),
                logs,
                metrics,
                prometheus,
                timeout: self.shutdown_timeout,
                summary: summary.line(),
            },
            warnings,
            trace_context,
        })
    }
}

/// The format and destination of local log lines, or `None` for none.
/// The environment's choices win over the program's; `auto` means text
/// for a terminal and flat JSON for anything else.
fn local_output(
    env: LocalEnv,
    programmed: Local,
    output: LogOutput,
    writer: Option<Box<dyn Write + Send>>,
) -> Option<(LocalFormat, Box<dyn Write + Send>, &'static str)> {
    type Out = (Box<dyn Write + Send>, bool, &'static str);
    let stream = |output| -> Out {
        match output {
            LogOutput::Stderr => (
                Box::new(std::io::stderr()),
                std::io::stderr().is_terminal(),
                "standard error",
            ),
            LogOutput::Stdout => (
                Box::new(std::io::stdout()),
                std::io::stdout().is_terminal(),
                "standard output",
            ),
        }
    };
    let (out, terminal, place) = match (env.output, writer) {
        (Some(output), _) => stream(output),
        (None, Some(writer)) => (writer, false, "the program's writer"),
        (None, None) => stream(output),
    };
    let format = match env.format.unwrap_or(programmed) {
        Local::Off => return None,
        Local::Format(format) => format,
        Local::Auto if terminal => LocalFormat::Text,
        Local::Auto => LocalFormat::Json,
    };
    Some((format, out, place))
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

    /// What was set up, in one line of plain words: where log lines go, and
    /// where traces, metrics and logs are sent. [`install`](Telemetry::install)
    /// logs it, at `info` with the target `oxtel`.
    pub fn summary(&self) -> &str {
        self.guard.summary()
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
    /// `log` crate with the `log` feature; then log the
    /// [summary](Telemetry::summary) and the warnings.
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
        tracing::info!(target: "oxtel", "{}", self.guard.summary());
        for warning in &self.warnings {
            tracing::warn!(target: "oxtel", "{warning}");
        }
        Ok(self.guard)
    }
}

/// Why oxtel couldn't be set up.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// A global `tracing` subscriber was already installed.
    SubscriberAlreadySet,
    /// A global `metrics` recorder was already installed.
    RecorderAlreadySet,
    /// The configuration file `OTEL_CONFIG_FILE` names couldn't be read.
    Config(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::SubscriberAlreadySet => {
                f.write_str("a global tracing subscriber is already installed")
            }
            Error::RecorderAlreadySet => {
                f.write_str("a global metrics recorder is already installed")
            }
            Error::Config(e) => f.write_str(e),
        }
    }
}

impl std::error::Error for Error {}
