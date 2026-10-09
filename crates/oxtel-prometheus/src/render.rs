//! The OpenMetrics 1.0 and classic Prometheus text formats.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};

use oxtel_sdk::metrics::{Exemplar, MetricFamily, MetricKind, PointValue};
use oxtel_sdk::trace::Scope;
use oxtel_sdk::{Resource, Value};

use crate::names::{family_name, label_name, sample_name, unit_suffix};

/// An exposition format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// OpenMetrics 1.0: exemplars, units and `# EOF`.
    OpenMetrics,
    /// Prometheus text format 0.0.4.
    Text,
}

impl Format {
    /// OpenMetrics when the scrape's `Accept` header asks for it, as
    /// Prometheus does; otherwise the classic text format.
    pub fn negotiate(accept: Option<&str>) -> Self {
        if accept.is_some_and(|a| a.contains("application/openmetrics-text")) {
            Format::OpenMetrics
        } else {
            Format::Text
        }
    }

    /// The `Content-Type` of a response in this format.
    pub fn content_type(self) -> &'static str {
        match self {
            Format::OpenMetrics => "application/openmetrics-text; version=1.0.0; charset=utf-8",
            Format::Text => "text/plain; version=0.0.4; charset=utf-8",
        }
    }
}

/// What the exposition adds beyond the metrics themselves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    /// Label every series with `otel_scope_name` (and
    /// `otel_scope_version`, when known), as the compatibility
    /// specification asks. Default: true.
    pub scope_labels: bool,
    /// Expose the resource as `target_info`. Default: true.
    pub target_info: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            scope_labels: true,
            target_info: true,
        }
    }
}

/// The exposition of `families` in `format`. Two metrics whose names
/// translate to the same Prometheus name would make an invalid exposition;
/// the first, in name order, is kept.
pub fn render(
    families: &[MetricFamily],
    resource: &Resource,
    format: Format,
    options: &Options,
) -> String {
    let mut out = String::new();
    let mut emitted: HashSet<String> = HashSet::new();
    if options.target_info {
        target_info(&mut out, resource, format);
        emitted.insert("target".into());
        emitted.insert("target_info".into());
    }
    let mut named: Vec<(String, &MetricFamily)> = families
        .iter()
        .filter(|f| !f.points.is_empty())
        .map(|f| (family_name(&f.name, f.unit.as_deref(), f.kind), f))
        .collect();
    named.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, family) in named {
        if !emitted.insert(name.clone()) {
            continue;
        }
        write_family(&mut out, &name, family, format, options);
    }
    if format == Format::OpenMetrics {
        out.push_str("# EOF\n");
    }
    out
}

fn type_word(kind: MetricKind) -> &'static str {
    match kind {
        MetricKind::Counter => "counter",
        MetricKind::Gauge => "gauge",
        MetricKind::Histogram => "histogram",
    }
}

fn write_family(
    out: &mut String,
    name: &str,
    family: &MetricFamily,
    format: Format,
    options: &Options,
) {
    let kind = family.kind;
    match format {
        Format::OpenMetrics => {
            let _ = writeln!(out, "# TYPE {name} {}", type_word(kind));
            if let Some(unit) = family.unit.as_deref().and_then(unit_suffix)
                && name.ends_with(&format!("_{unit}"))
            {
                let _ = writeln!(out, "# UNIT {name} {unit}");
            }
            if let Some(help) = &family.description {
                let _ = writeln!(out, "# HELP {name} {}", escape_help(help, true));
            }
        }
        Format::Text => {
            let typed = sample_name(name, kind);
            if let Some(help) = &family.description {
                let _ = writeln!(out, "# HELP {typed} {}", escape_help(help, false));
            }
            let _ = writeln!(out, "# TYPE {typed} {}", type_word(kind));
        }
    }
    for point in &family.points {
        let labels = labels(&point.attributes, &family.scope, options);
        match &point.value {
            PointValue::Counter(v) => sample(
                out,
                &sample_name(name, kind),
                &labels,
                None,
                &v.to_string(),
                point.exemplar.as_ref(),
                format,
            ),
            PointValue::Gauge(v) => sample(out, name, &labels, None, &number(*v), None, format),
            PointValue::Histogram(h) => {
                let bucket = format!("{name}_bucket");
                let mut cumulative = 0u64;
                for (i, count) in h.bucket_counts.iter().enumerate() {
                    cumulative += count;
                    let le = h
                        .bounds
                        .get(i)
                        .map_or_else(|| "+Inf".to_string(), |b| number(*b));
                    sample(
                        out,
                        &bucket,
                        &labels,
                        Some(("le", &le)),
                        &cumulative.to_string(),
                        h.exemplars.get(i).and_then(Option::as_ref),
                        format,
                    );
                }
                sample(
                    out,
                    &format!("{name}_count"),
                    &labels,
                    None,
                    &cumulative.to_string(),
                    None,
                    format,
                );
                sample(
                    out,
                    &format!("{name}_sum"),
                    &labels,
                    None,
                    &number(h.sum),
                    None,
                    format,
                );
            }
        }
    }
}

/// Attribute labels with Prometheus names. Keys that sanitise to the same
/// name have their values joined with `;`, in key order, as the
/// specification asks.
fn labels(
    attributes: &[(String, String)],
    scope: &Scope,
    options: &Options,
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::with_capacity(attributes.len() + 2);
    for (key, value) in attributes {
        let name = label_name(key);
        match out.iter_mut().find(|(n, _)| *n == name) {
            Some((_, existing)) => {
                existing.push(';');
                existing.push_str(value);
            }
            None => out.push((name, value.clone())),
        }
    }
    if options.scope_labels {
        out.push(("otel_scope_name".into(), scope.name.to_string()));
        if let Some(version) = &scope.version {
            out.push(("otel_scope_version".into(), version.to_string()));
        }
    }
    out
}

fn sample(
    out: &mut String,
    name: &str,
    labels: &[(String, String)],
    extra: Option<(&str, &str)>,
    value: &str,
    exemplar: Option<&Exemplar>,
    format: Format,
) {
    out.push_str(name);
    let pairs = labels
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .chain(extra);
    write_labels(out, pairs);
    out.push(' ');
    out.push_str(value);
    if format == Format::OpenMetrics
        && let Some(e) = exemplar
    {
        out.push_str(" # ");
        let trace_id = e.trace_id.map(|t| t.to_string());
        let span_id = e.span_id.map(|s| s.to_string());
        let ids = [
            ("trace_id", trace_id.as_deref()),
            ("span_id", span_id.as_deref()),
        ];
        let present: Vec<(&str, &str)> = ids
            .into_iter()
            .filter_map(|(k, v)| v.map(|v| (k, v)))
            .collect();
        if present.is_empty() {
            out.push_str("{}");
        } else {
            write_labels(out, present.into_iter());
        }
        let _ = write!(out, " {} {}", number(e.value), timestamp(e.time));
    }
    out.push('\n');
}

fn write_labels<'a>(out: &mut String, pairs: impl Iterator<Item = (&'a str, &'a str)>) {
    let mut first = true;
    for (k, v) in pairs {
        out.push(if first { '{' } else { ',' });
        first = false;
        out.push_str(k);
        out.push_str("=\"");
        escape_label_value(out, v);
        out.push('"');
    }
    if !first {
        out.push('}');
    }
}

fn escape_label_value(out: &mut String, v: &str) {
    for c in v.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
}

fn escape_help(help: &str, openmetrics: bool) -> String {
    let mut out = String::with_capacity(help.len());
    for c in help.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '"' if openmetrics => out.push_str("\\\""),
            c => out.push(c),
        }
    }
    out
}

/// A float as Prometheus writes it.
fn number(v: f64) -> String {
    if v.is_nan() {
        "NaN".into()
    } else if v == f64::INFINITY {
        "+Inf".into()
    } else if v == f64::NEG_INFINITY {
        "-Inf".into()
    } else {
        v.to_string()
    }
}

/// Seconds since the Unix epoch, to the millisecond.
fn timestamp(t: SystemTime) -> String {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    format!("{}.{:03}", d.as_secs(), d.subsec_millis())
}

fn target_info(out: &mut String, resource: &Resource, format: Format) {
    let labels: Vec<(String, String)> = resource
        .attributes()
        .iter()
        .map(|kv| (label_name(&kv.key), value_text(&kv.value)))
        .collect();
    match format {
        Format::OpenMetrics => {
            out.push_str("# TYPE target info\n# HELP target Target metadata\n");
        }
        Format::Text => {
            out.push_str("# HELP target_info Target metadata\n# TYPE target_info gauge\n");
        }
    }
    out.push_str("target_info");
    write_labels(out, labels.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    out.push_str(" 1\n");
}

fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::I64(i) => i.to_string(),
        Value::F64(f) => number(*f),
        other => format!("{other:?}"),
    }
}
