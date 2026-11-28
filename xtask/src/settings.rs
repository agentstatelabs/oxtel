//! Every setting oxtel reads from the environment, the one place they're
//! described. `gen-settings` writes the site's settings page from this
//! table, and `check-settings` checks it against the code:
//!
//! - every `OTEL_*` or `OXTEL_*` name, or name template, that a crate's
//!   code (outside its tests) has as a string literal is described here;
//! - every setting here is still read where it says, by the literals it
//!   names;
//! - the page is what this table makes.
//!
//! So a setting can't be added, renamed or dropped in the code without
//! this table, and the page, following.

use crate::WEIGHT;
use std::collections::BTreeMap;
use std::path::Path;

/// One environment variable.
pub struct Setting {
    pub name: &'static str,
    /// Also read for one signal, as `OTEL_EXPORTER_OTLP_TRACES_…`,
    /// `…_METRICS_…` and `…_LOGS_…`, which takes precedence.
    pub per_signal: bool,
    /// Markdown.
    pub default: &'static str,
    /// What it does, in Markdown.
    pub about: &'static str,
    /// Where the code reads it: each file, with the string literals it
    /// reads it by. A name built at run time is read by its parts, such as
    /// a template `"OTEL_{}_EXPORTER"` or a suffix `"CERTIFICATE"`.
    pub read: &'static [(&'static str, &'static [&'static str])],
}

pub struct Group {
    pub title: &'static str,
    pub intro: &'static str,
    pub settings: &'static [Setting],
}

const CONFIG: &str = "crates/oxtel/src/config.rs";
const BUILDER: &str = "crates/oxtel/src/builder.rs";
const PLAN: &str = "crates/oxtel/src/plan.rs";
const OTLP: &str = "crates/oxtel-otlp/src/http.rs";
const EXPORT: &str = "crates/oxtel-sdk/src/export.rs";
const RESOURCE: &str = "crates/oxtel-sdk/src/resource/mod.rs";
const DETECT: &str = "crates/oxtel-sdk/src/resource/detect.rs";
const SAMPLER: &str = "crates/oxtel-sdk/src/trace/sampler.rs";
const TRACE_LAYER: &str = "crates/oxtel-sdk/src/trace/layer.rs";
const LOG_LAYER: &str = "crates/oxtel-sdk/src/logs/layer.rs";
const READER: &str = "crates/oxtel-sdk/src/metrics/reader.rs";
const STORE: &str = "crates/oxtel-sdk/src/metrics/store.rs";

/// How OTLP's per-signal settings are read: `OTEL_EXPORTER_OTLP_{SIGNAL}_`
/// and the setting's suffix.
const OTLP_SIGNAL: &str = "OTEL_EXPORTER_OTLP_{}_{setting}";
/// A TLS file's general variable: `OTEL_EXPORTER_OTLP_` and the suffix.
const OTLP_FILE: &str = "OTEL_EXPORTER_OTLP_{setting}";

pub const GROUPS: &[Group] = &[
    Group {
        title: "Turning things on and off",
        intro: "Nothing is sent anywhere until an endpoint is set or an exporter is named. Local log lines are on from the start.",
        settings: &[
            Setting {
                name: "OTEL_SDK_DISABLED",
                per_signal: false,
                default: "`false`",
                about: "`true` turns off traces, metrics and logs, and leaves only local log lines.",
                read: &[(CONFIG, &["OTEL_SDK_DISABLED"])],
            },
            Setting {
                name: "OTEL_TRACES_EXPORTER",
                per_signal: false,
                default: "`otlp` once an endpoint is set, otherwise `none`",
                about: "Where spans go: `otlp`, `console` (readable lines on standard output) or `none`. Several can be listed, separated by commas.",
                read: &[(CONFIG, &["OTEL_{}_EXPORTER"])],
            },
            Setting {
                name: "OTEL_METRICS_EXPORTER",
                per_signal: false,
                default: "`otlp` once an endpoint is set, otherwise `none`",
                about: "Where metrics go: `otlp`, `prometheus` (a `/metrics` endpoint, at the host and port below), `console` or `none`. Several can be listed, separated by commas.",
                read: &[(CONFIG, &["OTEL_{}_EXPORTER"])],
            },
            Setting {
                name: "OTEL_LOGS_EXPORTER",
                per_signal: false,
                default: "`otlp` once an endpoint is set, otherwise `none`",
                about: "Where log records go: `otlp`, `console` or `none`. Local log lines are separate: see `OXTEL_LOG_FORMAT`.",
                read: &[(CONFIG, &["OTEL_{}_EXPORTER"])],
            },
            Setting {
                name: "OTEL_CONFIG_FILE",
                per_signal: false,
                default: "unset",
                about: "A declarative configuration file (schema 1.x) to build every pipeline from, instead of these variables, which the file can still substitute with `${NAME}`. Needs oxtel's `config` feature, which is on by default. A file that can't be read is an error.",
                read: &[(BUILDER, &["OTEL_CONFIG_FILE"])],
            },
        ],
    },
    Group {
        title: "Local log lines",
        intro: "What oxtel writes to the program's own output, separately from what it exports.",
        settings: &[
            Setting {
                name: "RUST_LOG",
                per_signal: false,
                default: "`info`",
                about: "How much is recorded, as directives such as `info` or `info,hyper=warn`. It applies to spans and exported log records too, unless the program sets their filters in code.",
                read: &[(BUILDER, &["RUST_LOG"])],
            },
            Setting {
                name: "OXTEL_LOG_FORMAT",
                per_signal: false,
                default: "`auto`",
                about: "How local log lines look: `auto` (text on a terminal, JSON otherwise), `text`, `json` (flat, with OpenTelemetry field names), `ecs` (Elastic Common Schema) or `none`.",
                read: &[(CONFIG, &["OXTEL_LOG_FORMAT"])],
            },
            Setting {
                name: "OXTEL_LOG_OUTPUT",
                per_signal: false,
                default: "`stderr`",
                about: "Where local log lines go: `stderr` or `stdout`. Standard output is left alone by default, since it carries a command-line tool's results and an MCP stdio server's protocol.",
                read: &[(CONFIG, &["OXTEL_LOG_OUTPUT"])],
            },
            Setting {
                name: "OXTEL_MAPPINGS",
                per_signal: false,
                default: "`all`",
                about: "`none` turns off the spans and metrics oxtel makes from what libraries already log, such as sqlx's queries.",
                read: &[(BUILDER, &["OXTEL_MAPPINGS"])],
            },
        ],
    },
    Group {
        title: "Your service",
        intro: "What every span, metric and log record says about where it came from.",
        settings: &[
            Setting {
                name: "OTEL_SERVICE_NAME",
                per_signal: false,
                default: "`unknown_service:` and the executable's name",
                about: "The service's name, as backends show it. It takes precedence over a `service.name` in `OTEL_RESOURCE_ATTRIBUTES`.",
                read: &[
                    (RESOURCE, &["OTEL_SERVICE_NAME"]),
                    (PLAN, &["OTEL_SERVICE_NAME"]),
                ],
            },
            Setting {
                name: "OTEL_RESOURCE_ATTRIBUTES",
                per_signal: false,
                default: "unset",
                about: "More about the service, as `key=value` pairs separated by commas, such as `deployment.environment.name=prod`. They take precedence over what's detected.",
                read: &[(RESOURCE, &["OTEL_RESOURCE_ATTRIBUTES"])],
            },
            Setting {
                name: "OTEL_EXPERIMENTAL_RESOURCE_DETECTORS",
                per_signal: false,
                default: "all of them",
                about: "Which detectors run at startup, separated by commas: `service`, `host`, `os`, `process`, `container` and `k8s`; or `none`.",
                read: &[(DETECT, &["OTEL_EXPERIMENTAL_RESOURCE_DETECTORS"])],
            },
            Setting {
                name: "K8S_NODE_NAME",
                per_signal: false,
                default: "unset",
                about: "The Kubernetes node, for `k8s.node.name`. Pass it into the pod through the downward API, from `spec.nodeName`. The namespace and pod are found without it.",
                read: &[(DETECT, &["K8S_NODE_NAME"])],
            },
        ],
    },
    Group {
        title: "Sending with OTLP",
        intro: "Where and how OTLP exporters send. Each of these is also read for one signal, with `TRACES_`, `METRICS_` or `LOGS_` after `OTEL_EXPORTER_OTLP_`, and that one takes precedence.",
        settings: &[
            Setting {
                name: "OTEL_EXPORTER_OTLP_ENDPOINT",
                per_signal: true,
                default: "unset: nothing is sent. With an exporter named, `http://localhost:4318`, or `http://localhost:4317` for gRPC",
                about: "Where to send. Over HTTP, oxtel adds `/v1/traces`, `/v1/metrics` or `/v1/logs`; a signal's own endpoint is used as it is, so it needs the full path. Over gRPC, it's used as it is.",
                read: &[
                    (
                        CONFIG,
                        &[
                            "OTEL_EXPORTER_OTLP_ENDPOINT",
                            "OTEL_EXPORTER_OTLP_{}_ENDPOINT",
                        ],
                    ),
                    (
                        OTLP,
                        &["OTEL_EXPORTER_OTLP_ENDPOINT", OTLP_SIGNAL, "ENDPOINT"],
                    ),
                ],
            },
            Setting {
                name: "OTEL_EXPORTER_OTLP_PROTOCOL",
                per_signal: true,
                default: "`http/protobuf`",
                about: "`http/protobuf`, `http/json`, or `grpc` with oxtel's `grpc` feature. Without the feature, `grpc` falls back to `http/protobuf` with a warning.",
                read: &[
                    (
                        CONFIG,
                        &[
                            "OTEL_EXPORTER_OTLP_PROTOCOL",
                            "OTEL_EXPORTER_OTLP_{}_PROTOCOL",
                        ],
                    ),
                    (
                        OTLP,
                        &["OTEL_EXPORTER_OTLP_PROTOCOL", OTLP_SIGNAL, "PROTOCOL"],
                    ),
                ],
            },
            Setting {
                name: "OTEL_EXPORTER_OTLP_HEADERS",
                per_signal: true,
                default: "unset",
                about: "Headers to send, as `key=value` pairs separated by commas, such as `Authorization=Bearer <token>`. Values may be percent-encoded. Over gRPC they're sent as metadata. A signal's own headers are added to these, replacing any of the same name.",
                read: &[(
                    OTLP,
                    &["OTEL_EXPORTER_OTLP_HEADERS", OTLP_SIGNAL, "HEADERS"],
                )],
            },
            Setting {
                name: "OTEL_EXPORTER_OTLP_COMPRESSION",
                per_signal: true,
                default: "none",
                about: "`gzip` compresses what's sent.",
                read: &[(
                    OTLP,
                    &["OTEL_EXPORTER_OTLP_COMPRESSION", OTLP_SIGNAL, "COMPRESSION"],
                )],
            },
            Setting {
                name: "OTEL_EXPORTER_OTLP_TIMEOUT",
                per_signal: true,
                default: "`10000`",
                about: "How long each export may take, in milliseconds.",
                read: &[(
                    OTLP,
                    &["OTEL_EXPORTER_OTLP_TIMEOUT", OTLP_SIGNAL, "TIMEOUT"],
                )],
            },
            Setting {
                name: "OTEL_EXPORTER_OTLP_CERTIFICATE",
                per_signal: true,
                default: "the bundled root certificates",
                about: "A PEM file of the certificates to verify the receiver with, in place of the bundled roots.",
                read: &[(OTLP, &[OTLP_SIGNAL, OTLP_FILE, "CERTIFICATE"])],
            },
            Setting {
                name: "OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE",
                per_signal: true,
                default: "unset",
                about: "A PEM file with a client certificate to present, for mutual TLS. It needs the key below.",
                read: &[(OTLP, &[OTLP_SIGNAL, OTLP_FILE, "CLIENT_CERTIFICATE"])],
            },
            Setting {
                name: "OTEL_EXPORTER_OTLP_CLIENT_KEY",
                per_signal: true,
                default: "unset",
                about: "A PEM file with the client certificate's private key.",
                read: &[(OTLP, &[OTLP_SIGNAL, OTLP_FILE, "CLIENT_KEY"])],
            },
            Setting {
                name: "OTEL_EXPORTER_OTLP_METRICS_TEMPORALITY_PREFERENCE",
                per_signal: false,
                default: "`cumulative`",
                about: "`delta` (or `lowmemory`, the same for oxtel) makes OTLP's counters and histograms report the change since the previous export. Other exporters keep their totals.",
                read: &[
                    (
                        CONFIG,
                        &["OTEL_EXPORTER_OTLP_METRICS_TEMPORALITY_PREFERENCE"],
                    ),
                    (OTLP, &["OTEL_EXPORTER_OTLP_METRICS_TEMPORALITY_PREFERENCE"]),
                ],
            },
            Setting {
                name: "OTEL_EXPORTER_OTLP_METRICS_DEFAULT_HISTOGRAM_AGGREGATION",
                per_signal: false,
                default: "`explicit_bucket_histogram`",
                about: "`base2_exponential_bucket_histogram` makes OTLP's histograms exponential. Prometheus scrapes are unchanged.",
                read: &[
                    (
                        CONFIG,
                        &["OTEL_EXPORTER_OTLP_METRICS_DEFAULT_HISTOGRAM_AGGREGATION"],
                    ),
                    (
                        OTLP,
                        &["OTEL_EXPORTER_OTLP_METRICS_DEFAULT_HISTOGRAM_AGGREGATION"],
                    ),
                ],
            },
        ],
    },
    Group {
        title: "Prometheus",
        intro: "The built-in `/metrics` endpoint, served with `OTEL_METRICS_EXPORTER=prometheus`.",
        settings: &[
            Setting {
                name: "OTEL_EXPORTER_PROMETHEUS_HOST",
                per_signal: false,
                default: "`localhost`",
                about: "The address to listen on. `0.0.0.0` listens on every interface, as a scraper outside a container needs.",
                read: &[(CONFIG, &["OTEL_EXPORTER_PROMETHEUS_HOST"])],
            },
            Setting {
                name: "OTEL_EXPORTER_PROMETHEUS_PORT",
                per_signal: false,
                default: "`9464`",
                about: "The port to listen on.",
                read: &[(CONFIG, &["OTEL_EXPORTER_PROMETHEUS_PORT"])],
            },
        ],
    },
    Group {
        title: "Sampling and propagation",
        intro: "Which traces are kept, and how a trace crosses from one service to the next.",
        settings: &[
            Setting {
                name: "OTEL_TRACES_SAMPLER",
                per_signal: false,
                default: "`parentbased_always_on`",
                about: "`always_on`, `always_off`, `traceidratio`, `parentbased_always_on`, `parentbased_always_off` or `parentbased_traceidratio`. The `parentbased_` ones follow the caller's decision when there is one.",
                read: &[(SAMPLER, &["OTEL_TRACES_SAMPLER"])],
            },
            Setting {
                name: "OTEL_TRACES_SAMPLER_ARG",
                per_signal: false,
                default: "`1.0`",
                about: "For the ratio samplers, the share of traces to keep, from 0 to 1. A ratio that isn't one means 1.0.",
                read: &[(SAMPLER, &["OTEL_TRACES_SAMPLER_ARG"])],
            },
            Setting {
                name: "OTEL_PROPAGATORS",
                per_signal: false,
                default: "`tracecontext,baggage`",
                about: "How trace context travels between services: `tracecontext` (W3C), `baggage` or `none`. Others are warned about and ignored.",
                read: &[(CONFIG, &["OTEL_PROPAGATORS"])],
            },
        ],
    },
    Group {
        title: "Batching and export intervals",
        intro: "When spans, log records and metrics are sent. Times are in milliseconds.",
        settings: &[
            Setting {
                name: "OTEL_BSP_SCHEDULE_DELAY",
                per_signal: false,
                default: "`5000`",
                about: "How often a batch of spans is sent.",
                read: &[(EXPORT, &["OTEL_BSP", "SCHEDULE_DELAY"])],
            },
            Setting {
                name: "OTEL_BSP_MAX_QUEUE_SIZE",
                per_signal: false,
                default: "`2048`",
                about: "How many spans wait to be sent. Beyond it, new spans are dropped and counted, rather than slowing the program.",
                read: &[(EXPORT, &["OTEL_BSP", "MAX_QUEUE_SIZE"])],
            },
            Setting {
                name: "OTEL_BSP_MAX_EXPORT_BATCH_SIZE",
                per_signal: false,
                default: "`512`",
                about: "The most spans sent at once. It can't exceed the queue.",
                read: &[(EXPORT, &["OTEL_BSP", "MAX_EXPORT_BATCH_SIZE"])],
            },
            Setting {
                name: "OTEL_BSP_EXPORT_TIMEOUT",
                per_signal: false,
                default: "`30000`",
                about: "How long sending a batch of spans may take.",
                read: &[(EXPORT, &["OTEL_BSP", "EXPORT_TIMEOUT"])],
            },
            Setting {
                name: "OTEL_BLRP_SCHEDULE_DELAY",
                per_signal: false,
                default: "`1000`",
                about: "How often a batch of log records is sent.",
                read: &[(EXPORT, &["OTEL_BLRP", "SCHEDULE_DELAY"])],
            },
            Setting {
                name: "OTEL_BLRP_MAX_QUEUE_SIZE",
                per_signal: false,
                default: "`2048`",
                about: "How many log records wait to be sent. Beyond it, new ones are dropped and counted.",
                read: &[(EXPORT, &["OTEL_BLRP", "MAX_QUEUE_SIZE"])],
            },
            Setting {
                name: "OTEL_BLRP_MAX_EXPORT_BATCH_SIZE",
                per_signal: false,
                default: "`512`",
                about: "The most log records sent at once.",
                read: &[(EXPORT, &["OTEL_BLRP", "MAX_EXPORT_BATCH_SIZE"])],
            },
            Setting {
                name: "OTEL_BLRP_EXPORT_TIMEOUT",
                per_signal: false,
                default: "`30000`",
                about: "How long sending a batch of log records may take.",
                read: &[(EXPORT, &["OTEL_BLRP", "EXPORT_TIMEOUT"])],
            },
            Setting {
                name: "OTEL_METRIC_EXPORT_INTERVAL",
                per_signal: false,
                default: "`60000`",
                about: "How often metrics are pushed. Prometheus scrapes whenever it likes.",
                read: &[(READER, &["OTEL_METRIC_EXPORT_INTERVAL"])],
            },
            Setting {
                name: "OTEL_METRIC_EXPORT_TIMEOUT",
                per_signal: false,
                default: "`30000`",
                about: "How long pushing metrics may take.",
                read: &[(READER, &["OTEL_METRIC_EXPORT_TIMEOUT"])],
            },
            Setting {
                name: "OTEL_METRICS_EXEMPLAR_FILTER",
                per_signal: false,
                default: "`trace_based`",
                about: "Which measurements keep the trace and span ids of the span they were made in: `trace_based` (those in a sampled span), `always_on` or `always_off`.",
                read: &[(STORE, &["OTEL_METRICS_EXEMPLAR_FILTER"])],
            },
        ],
    },
    Group {
        title: "Limits",
        intro: "Caps that keep one runaway span or record from using unbounded memory. What's over a limit is dropped.",
        settings: &[
            Setting {
                name: "OTEL_ATTRIBUTE_COUNT_LIMIT",
                per_signal: false,
                default: "`128`",
                about: "The most attributes on a span or log record, unless the more specific limits below say otherwise.",
                read: &[
                    (TRACE_LAYER, &["OTEL_ATTRIBUTE_COUNT_LIMIT"]),
                    (LOG_LAYER, &["OTEL_ATTRIBUTE_COUNT_LIMIT"]),
                ],
            },
            Setting {
                name: "OTEL_SPAN_ATTRIBUTE_COUNT_LIMIT",
                per_signal: false,
                default: "`128`",
                about: "The most attributes on a span.",
                read: &[(TRACE_LAYER, &["OTEL_SPAN_ATTRIBUTE_COUNT_LIMIT"])],
            },
            Setting {
                name: "OTEL_SPAN_EVENT_COUNT_LIMIT",
                per_signal: false,
                default: "`128`",
                about: "The most events on a span.",
                read: &[(TRACE_LAYER, &["OTEL_SPAN_EVENT_COUNT_LIMIT"])],
            },
            Setting {
                name: "OTEL_SPAN_LINK_COUNT_LIMIT",
                per_signal: false,
                default: "`128`",
                about: "The most links on a span.",
                read: &[(TRACE_LAYER, &["OTEL_SPAN_LINK_COUNT_LIMIT"])],
            },
            Setting {
                name: "OTEL_LOGRECORD_ATTRIBUTE_COUNT_LIMIT",
                per_signal: false,
                default: "`128`",
                about: "The most attributes on a log record.",
                read: &[(LOG_LAYER, &["OTEL_LOGRECORD_ATTRIBUTE_COUNT_LIMIT"])],
            },
        ],
    },
];

/// Where the generated page goes.
pub const PAGE: &str = "site/src/content/docs/reference/settings.md";

/// The settings page, as Markdown.
pub fn page() -> String {
    let mut out = String::from(
        "---\n\
         title: Every setting\n\
         description: Every environment variable oxtel reads, with its default and what it does.\n\
         editUrl: https://github.com/agentstatelabs/oxtel/edit/main/xtask/src/settings.rs\n\
         ---\n\n\
         <!-- Generated by `cargo run -p xtask -- gen-settings` from xtask/src/settings.rs. Edit that, not this. -->\n\n\
         oxtel reads the standard OpenTelemetry variables, and a few of its own that start with `OXTEL_`, so the people who run a program can change what it records and where it goes without a rebuild. A program can also make most of these choices in code, with `oxtel::builder()`; its documentation says which ones the environment overrides.\n\n\
         oxtel follows the specification's variables and defaults with one deliberate exception: the specification sends to `localhost:4318` even when nothing is set, and oxtel sends nothing until an endpoint is set or an exporter is named.\n",
    );
    for group in GROUPS {
        out.push_str(&format!(
            "\n## {}\n\n{}\n\n| Variable | Default | What it does |\n|---|---|---|\n",
            group.title, group.intro
        ));
        for s in group.settings {
            let mut about = s.about.to_string();
            if s.per_signal {
                let signal =
                    s.name
                        .replacen("OTEL_EXPORTER_OTLP_", "OTEL_EXPORTER_OTLP_TRACES_", 1);
                about.push_str(&format!(" Also per signal, such as `{signal}`."));
            }
            out.push_str(&format!(
                "| {WEIGHT}`{}`</span> | {} | {} |\n",
                s.name, s.default, about
            ));
        }
    }
    out.push_str(
        "\n## How this page is kept true\n\n\
         It's generated from a table in the repository's `xtask`, which CI checks against the code on every change: every `OTEL_` or `OXTEL_` name the code reads has to be in the table, and every setting in the table has to be read where the table says. A setting can't be added, renamed or dropped without this page following.\n",
    );
    out
}

/// The string literals in a crate's code, outside its tests, that name a
/// setting or a template for one: `"OTEL_…"`, `"OXTEL_…"` or `"RUST_LOG"`.
fn read_names(source: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in code_lines(source) {
        for literal in literals(line) {
            // A name or a template for one, not a message that mentions one.
            let name_like = literal
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "_{}".contains(c));
            let is_setting = literal.starts_with("OTEL_")
                || literal.starts_with("OXTEL_")
                || literal == "RUST_LOG";
            if is_setting && name_like {
                names.push(literal);
            }
        }
    }
    names
}

/// A source file's lines outside its tests module and comments.
fn code_lines(source: &str) -> impl Iterator<Item = &str> {
    let code = match source.find("#[cfg(test)]\nmod tests") {
        Some(at) => &source[..at],
        None => source,
    };
    code.lines()
        .filter(|line| !line.trim_start().starts_with("//"))
}

/// The string literals on a line.
fn literals(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c != '"' {
            continue;
        }
        let mut literal = String::new();
        let mut escaped = false;
        for c in chars.by_ref() {
            match (escaped, c) {
                (false, '\\') => escaped = true,
                (false, '"') => break,
                _ => {
                    literal.push(c);
                    escaped = false;
                }
            }
        }
        out.push(literal);
    }
    out
}

/// Checks the table against the code and the page against the table.
pub fn check(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut problems = Vec::new();

    // Each source file's string literals, and the setting names among
    // them, outside its tests.
    let mut literals_in: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut names_in: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for path in walk(&root.join("crates"))? {
        if path.extension().is_some_and(|e| e == "rs")
            && path.components().any(|c| c.as_os_str() == "src")
        {
            let shown = path.strip_prefix(root)?.display().to_string();
            let source = std::fs::read_to_string(&path)?;
            literals_in.insert(
                shown.clone(),
                code_lines(&source).flat_map(literals).collect(),
            );
            names_in.insert(shown, read_names(&source));
        }
    }

    // Every setting is still read where the table says.
    let mut documented = std::collections::BTreeSet::new();
    let mut seen = std::collections::BTreeSet::new();
    for group in GROUPS {
        for s in group.settings {
            if !seen.insert(s.name) {
                problems.push(format!("{} is in the table twice", s.name));
            }
            documented.insert(s.name.to_string());
            for (file, tokens) in s.read {
                let Some(literals) = literals_in.get(*file) else {
                    problems.push(format!("{}: {file} isn't a crate source file", s.name));
                    continue;
                };
                for token in *tokens {
                    documented.insert(token.to_string());
                    if !literals.iter().any(|l| l == token) {
                        problems.push(format!(
                            "{}: {file} no longer has the string literal {token:?} it's read by",
                            s.name
                        ));
                    }
                }
            }
        }
    }

    // Everything the code reads is in the table.
    for (file, names) in &names_in {
        for name in names {
            if !documented.contains(name) {
                problems.push(format!(
                    "{file} reads {name:?}, which xtask/src/settings.rs doesn't describe"
                ));
            }
        }
    }

    // The page is what the table makes.
    let page_path = root.join(PAGE);
    match std::fs::read_to_string(&page_path) {
        Ok(text) if text == page() => {}
        _ => problems.push(format!(
            "{PAGE} is out of date; `cargo run -p xtask -- gen-settings` rewrites it"
        )),
    }

    if problems.is_empty() {
        let count: usize = GROUPS.iter().map(|g| g.settings.len()).sum();
        println!("{count} settings match the code, and {PAGE} matches them");
        Ok(())
    } else {
        Err(problems.join("\n").into())
    }
}

fn walk(dir: &Path) -> std::io::Result<Vec<std::path::PathBuf>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            out.extend(walk(&path)?);
        } else {
            out.push(path);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_found_outside_comments_and_tests() {
        let source = r#"
// "OTEL_IN_A_COMMENT"
let a = var("OTEL_SERVICE_NAME");
let b = var(&format!("OTEL_{}_EXPORTER", s));
warn!("OTEL_PROPAGATORS={other} isn't supported");
let c = std::env::var("RUST_LOG");
#[cfg(test)]
mod tests {
    let d = var("OTEL_ONLY_IN_TESTS");
}
"#;
        assert_eq!(
            read_names(source),
            ["OTEL_SERVICE_NAME", "OTEL_{}_EXPORTER", "RUST_LOG"]
        );
    }

    #[test]
    fn literals_handle_escapes() {
        assert_eq!(literals(r#"f("a\"b", "c")"#), [r#"a"b"#, "c"]);
    }
}
