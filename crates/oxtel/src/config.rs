//! What the standard environment variables ask for.

/// An exporter the environment can choose for a signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exporter {
    /// OTLP over HTTP (`otlp`).
    Otlp,
    /// Readable lines on standard output (`console`).
    Console,
    /// A Prometheus `/metrics` endpoint (`prometheus`); metrics only.
    Prometheus,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Signal {
    Traces,
    Metrics,
    Logs,
}

impl Signal {
    fn name(self) -> &'static str {
        match self {
            Signal::Traces => "TRACES",
            Signal::Metrics => "METRICS",
            Signal::Logs => "LOGS",
        }
    }
}

/// The configuration read from `OTEL_*` variables, with a warning for each
/// value oxtel doesn't support.
#[derive(Debug, PartialEq)]
pub(crate) struct EnvConfig {
    pub(crate) disabled: bool,
    pub(crate) traces: Vec<Exporter>,
    pub(crate) metrics: Vec<Exporter>,
    pub(crate) logs: Vec<Exporter>,
    pub(crate) trace_context: bool,
    pub(crate) prometheus_addr: String,
    pub(crate) warnings: Vec<String>,
}

impl EnvConfig {
    pub(crate) fn from_env() -> Self {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Read through `get`, which returns a variable's value if it's set.
    pub(crate) fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
        let get = |name: &str| get(name).filter(|v| !v.trim().is_empty());
        let mut warnings = Vec::new();
        let disabled =
            get("OTEL_SDK_DISABLED").is_some_and(|v| v.trim().eq_ignore_ascii_case("true"));
        let mut exporters = |signal: Signal| {
            let var = format!("OTEL_{}_EXPORTER", signal.name());
            let raw = get(&var).unwrap_or_else(|| "otlp".to_string());
            let mut chosen = Vec::new();
            for name in raw.split(',').map(|n| n.trim().to_ascii_lowercase()) {
                let exporter = match name.as_str() {
                    "" | "none" => continue,
                    "otlp" => Exporter::Otlp,
                    // `logging` is the deprecated name for `console`.
                    "console" | "logging" => Exporter::Console,
                    "prometheus" if signal == Signal::Metrics => Exporter::Prometheus,
                    other => {
                        warnings.push(format!("{var}={other} isn't supported; ignored"));
                        continue;
                    }
                };
                if !chosen.contains(&exporter) {
                    chosen.push(exporter);
                }
            }
            if chosen.contains(&Exporter::Otlp) {
                let specific = format!("OTEL_EXPORTER_OTLP_{}_PROTOCOL", signal.name());
                let (var, protocol) = match get(&specific) {
                    Some(p) => (specific, Some(p)),
                    None => (
                        "OTEL_EXPORTER_OTLP_PROTOCOL".to_string(),
                        get("OTEL_EXPORTER_OTLP_PROTOCOL"),
                    ),
                };
                if let Some(protocol) = protocol.map(|p| p.trim().to_ascii_lowercase())
                    && protocol != "http/protobuf"
                {
                    let warning =
                        format!("{var}={protocol} isn't supported yet; using http/protobuf");
                    if !warnings.contains(&warning) {
                        warnings.push(warning);
                    }
                }
            }
            chosen
        };
        let traces = exporters(Signal::Traces);
        let metrics = exporters(Signal::Metrics);
        let logs = exporters(Signal::Logs);

        let mut trace_context = false;
        let propagators = get("OTEL_PROPAGATORS").unwrap_or_else(|| "tracecontext,baggage".into());
        for name in propagators
            .split(',')
            .map(|n| n.trim().to_ascii_lowercase())
        {
            match name.as_str() {
                "tracecontext" => trace_context = true,
                // Baggage is read and written with oxtel_sdk::propagation.
                "baggage" | "none" | "" => {}
                other => {
                    warnings.push(format!("OTEL_PROPAGATORS={other} isn't supported; ignored"))
                }
            }
        }

        let host = get("OTEL_EXPORTER_PROMETHEUS_HOST").unwrap_or_else(|| "localhost".into());
        let host = host.trim();
        let port = match get("OTEL_EXPORTER_PROMETHEUS_PORT") {
            None => 9464,
            Some(p) => p.trim().parse::<u16>().unwrap_or_else(|_| {
                warnings.push(format!(
                    "OTEL_EXPORTER_PROMETHEUS_PORT={} isn't a port; using 9464",
                    p.trim()
                ));
                9464
            }),
        };
        let prometheus_addr = if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        };

        EnvConfig {
            disabled,
            traces,
            metrics,
            logs,
            trace_context,
            prometheus_addr,
            warnings,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(vars: &[(&str, &str)]) -> EnvConfig {
        EnvConfig::from_lookup(|name| {
            vars.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.to_string())
        })
    }

    #[test]
    fn defaults_follow_the_specification() {
        assert_eq!(
            config(&[]),
            EnvConfig {
                disabled: false,
                traces: vec![Exporter::Otlp],
                metrics: vec![Exporter::Otlp],
                logs: vec![Exporter::Otlp],
                trace_context: true,
                prometheus_addr: "localhost:9464".into(),
                warnings: vec![],
            }
        );
    }

    #[test]
    fn exporter_lists_are_parsed_and_unknown_ones_reported() {
        let c = config(&[
            ("OTEL_TRACES_EXPORTER", " Console, otlp ,zipkin,console"),
            ("OTEL_METRICS_EXPORTER", "prometheus,logging"),
            ("OTEL_LOGS_EXPORTER", "none"),
        ]);
        assert_eq!(c.traces, vec![Exporter::Console, Exporter::Otlp]);
        assert_eq!(c.metrics, vec![Exporter::Prometheus, Exporter::Console]);
        assert!(c.logs.is_empty());
        assert_eq!(
            c.warnings,
            vec!["OTEL_TRACES_EXPORTER=zipkin isn't supported; ignored"]
        );
        let c = config(&[("OTEL_LOGS_EXPORTER", "prometheus")]);
        assert!(c.logs.is_empty(), "prometheus is for metrics only");
        assert_eq!(c.warnings.len(), 1);
    }

    #[test]
    fn unsupported_protocols_fall_back_to_http_protobuf() {
        let c = config(&[
            ("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc"),
            ("OTEL_EXPORTER_OTLP_LOGS_PROTOCOL", "http/protobuf"),
        ]);
        assert_eq!(
            c.warnings,
            vec!["OTEL_EXPORTER_OTLP_PROTOCOL=grpc isn't supported yet; using http/protobuf"],
            "reported once, and not for logs, which name a supported protocol"
        );
        let c = config(&[
            ("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc"),
            ("OTEL_TRACES_EXPORTER", "console"),
            ("OTEL_METRICS_EXPORTER", "none"),
            ("OTEL_LOGS_EXPORTER", "none"),
        ]);
        assert!(
            c.warnings.is_empty(),
            "no OTLP, so the protocol doesn't matter"
        );
    }

    #[test]
    fn propagators_disabling_and_prometheus_address() {
        let c = config(&[
            ("OTEL_PROPAGATORS", "b3,baggage"),
            ("OTEL_SDK_DISABLED", "TRUE"),
            ("OTEL_EXPORTER_PROMETHEUS_HOST", "::1"),
            ("OTEL_EXPORTER_PROMETHEUS_PORT", "9100"),
        ]);
        assert!(!c.trace_context);
        assert!(c.disabled);
        assert_eq!(c.prometheus_addr, "[::1]:9100");
        assert_eq!(
            c.warnings,
            vec!["OTEL_PROPAGATORS=b3 isn't supported; ignored"]
        );
        let c = config(&[
            ("OTEL_SDK_DISABLED", "1"),
            ("OTEL_EXPORTER_PROMETHEUS_PORT", "x"),
        ]);
        assert!(!c.disabled, "only `true` disables");
        assert_eq!(c.prometheus_addr, "localhost:9464");
        assert_eq!(c.warnings.len(), 1);
    }
}
