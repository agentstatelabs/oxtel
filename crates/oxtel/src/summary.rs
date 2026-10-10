//! The line [`Telemetry::install`](crate::Telemetry::install) logs to say,
//! in plain words, what was set up: where log lines go, and where each kind
//! of telemetry is sent.
//!
//! ```text
//! log lines: standard error, as JSON; traces, metrics and logs: not exported (set OTEL_EXPORTER_OTLP_ENDPOINT to send them to a collector)
//! ```

use std::net::SocketAddr;

use crate::plan::{Proc, Sink};

/// The kinds of telemetry, in the order the line names them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Traces,
    Metrics,
    Logs,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Traces => "traces",
            Kind::Metrics => "metrics",
            Kind::Logs => "logs",
        }
    }

    #[cfg(feature = "otlp")]
    fn otlp(self) -> oxtel_otlp::Signal {
        match self {
            Kind::Traces => oxtel_otlp::Signal::Traces,
            Kind::Metrics => oxtel_otlp::Signal::Metrics,
            Kind::Logs => oxtel_otlp::Signal::Logs,
        }
    }
}

/// Somewhere telemetry goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Dest {
    /// OTLP to `url`, in JSON if `json`.
    #[cfg_attr(not(feature = "otlp"), allow(dead_code))]
    Otlp {
        url: String,
        json: bool,
    },
    Stdout,
    Prometheus(SocketAddr),
    /// Whatever the program's own hooks set up.
    Program,
}

impl Dest {
    pub(crate) fn of_sink(sink: &Sink, kind: Kind) -> Dest {
        match sink {
            #[cfg(feature = "otlp")]
            Sink::Otlp(o) => Dest::Otlp {
                url: o.url(kind.otlp()).to_string(),
                json: o.protocol(kind.otlp()) == oxtel_otlp::Protocol::HttpJson,
            },
            #[cfg(not(feature = "otlp"))]
            Sink::Otlp(o) => match *o {},
            Sink::Console(_) => {
                let _ = kind;
                Dest::Stdout
            }
        }
    }

    pub(crate) fn of_proc(proc: &Proc, kind: Kind) -> Dest {
        match proc {
            Proc::Batch(sink, _) | Proc::Simple(sink) => Dest::of_sink(sink, kind),
        }
    }

    fn describe(&self) -> String {
        match self {
            Dest::Otlp { url, json: false } => format!("OTLP to {}", shown_url(url)),
            Dest::Otlp { url, json: true } => format!("OTLP/JSON to {}", shown_url(url)),
            Dest::Stdout => "standard output".into(),
            Dest::Prometheus(addr) => {
                format!("a Prometheus scrape endpoint at http://{addr}/metrics")
            }
            Dest::Program => "the program's own setup".into(),
        }
    }
}

/// What was set up, for the line.
#[derive(Debug, Default)]
pub(crate) struct Summary {
    /// The configuration file in use, if any.
    pub(crate) config_file: Option<String>,
    /// Where local log lines go and how they look, such as
    /// `standard error, as JSON`; `None` when they're off.
    pub(crate) local: Option<String>,
    /// Everything but local output is off.
    pub(crate) disabled: bool,
    pub(crate) traces: Vec<Dest>,
    pub(crate) metrics: Vec<Dest>,
    pub(crate) logs: Vec<Dest>,
}

impl Summary {
    /// The line.
    pub(crate) fn line(&self) -> String {
        let mut parts = Vec::new();
        if let Some(path) = &self.config_file {
            parts.push(format!("configured by {path}"));
        }
        parts.push(match &self.local {
            Some(local) => format!("log lines: {local}"),
            None => "log lines: off".into(),
        });
        if self.disabled {
            let why = if self.config_file.is_some() {
                "the configuration file disables it"
            } else {
                "OTEL_SDK_DISABLED is true"
            };
            parts.push(format!("everything else: off ({why})"));
            return parts.join("; ");
        }
        let kinds = [
            (Kind::Traces, &self.traces),
            (Kind::Metrics, &self.metrics),
            (Kind::Logs, &self.logs),
        ];
        let mut unexported = Vec::new();
        for (kind, dests) in kinds {
            if dests.is_empty() {
                unexported.push(kind.name());
            } else {
                let dests: Vec<String> = dests.iter().map(Dest::describe).collect();
                parts.push(format!("{}: {}", kind.name(), dests.join(" and ")));
            }
        }
        if !unexported.is_empty() {
            let mut part = format!("{}: not exported", list(&unexported));
            if unexported.len() == kinds.len() && self.config_file.is_none() {
                part.push_str(" (set OTEL_EXPORTER_OTLP_ENDPOINT to send them to a collector)");
            }
            parts.push(part);
        }
        parts.join("; ")
    }
}

/// `a`, `a and b`, `a, b and c`.
fn list(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => one.to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// `url` without credentials, a query or a fragment, which can carry
/// secrets.
fn shown_url(url: &str) -> String {
    let url = url.split(['?', '#']).next().unwrap_or(url);
    match url.split_once("://") {
        Some((scheme, rest)) => {
            let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
            let host = authority
                .rsplit_once('@')
                .map_or(authority, |(_, host)| host);
            format!("{scheme}://{host}{path}")
        }
        None => url.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn otlp(path: &str) -> Dest {
        Dest::Otlp {
            url: format!("http://localhost:4318{path}"),
            json: false,
        }
    }

    #[test]
    fn with_nothing_configured_it_says_how_to_export() {
        let summary = Summary {
            local: Some("standard error, as JSON".into()),
            ..Summary::default()
        };
        assert_eq!(
            summary.line(),
            "log lines: standard error, as JSON; traces, metrics and logs: not exported \
             (set OTEL_EXPORTER_OTLP_ENDPOINT to send them to a collector)"
        );
    }

    #[test]
    fn each_kind_names_where_it_goes() {
        let summary = Summary {
            local: Some("standard error, as text".into()),
            traces: vec![otlp("/v1/traces"), Dest::Stdout],
            metrics: vec![Dest::Prometheus("127.0.0.1:9464".parse().unwrap())],
            ..Summary::default()
        };
        assert_eq!(
            summary.line(),
            "log lines: standard error, as text; \
             traces: OTLP to http://localhost:4318/v1/traces and standard output; \
             metrics: a Prometheus scrape endpoint at http://127.0.0.1:9464/metrics; \
             logs: not exported"
        );
    }

    #[test]
    fn a_configuration_file_or_a_disabled_sdk_is_named() {
        let summary = Summary {
            config_file: Some("otel.yaml".into()),
            logs: vec![Dest::Otlp {
                url: "https://collector/v1/logs".into(),
                json: true,
            }],
            ..Summary::default()
        };
        assert_eq!(
            summary.line(),
            "configured by otel.yaml; log lines: off; logs: OTLP/JSON to https://collector/v1/logs; \
             traces and metrics: not exported"
        );
        let summary = Summary {
            local: Some("standard output, as ECS JSON".into()),
            disabled: true,
            traces: vec![Dest::Program],
            ..Summary::default()
        };
        assert_eq!(
            summary.line(),
            "log lines: standard output, as ECS JSON; everything else: off (OTEL_SDK_DISABLED is true)"
        );
    }

    #[test]
    fn urls_are_shown_without_secrets() {
        assert_eq!(
            shown_url("https://user:token@collector:4318/v1/traces?api-key=x#f"),
            "https://collector:4318/v1/traces"
        );
        assert_eq!(shown_url("http://collector:4318"), "http://collector:4318");
        assert_eq!(shown_url("collector:4318"), "collector:4318");
    }
}
