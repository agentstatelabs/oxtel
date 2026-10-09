//! OpenTelemetry names to Prometheus names, following the specification's
//! [Prometheus compatibility](https://opentelemetry.io/docs/specs/otel/compatibility/prometheus_and_openmetrics/)
//! rules.

use oxtel_sdk::metrics::MetricKind;

/// The Prometheus metric family name for an OpenTelemetry metric, unit
/// suffix included and `_total` not: `http.server.request.duration` in `s`
/// becomes `http_server_request_duration_seconds`. Counter samples add
/// `_total` (see [`sample_name`]).
pub fn family_name(name: &str, unit: Option<&str>, kind: MetricKind) -> String {
    let mut out = sanitize(name, true);
    let base = out.strip_suffix("_total").map(str::to_string);
    if kind == MetricKind::Counter
        && let Some(base) = base
    {
        out = base;
    }
    let suffix = match unit.map(str::trim) {
        Some("1") if kind == MetricKind::Gauge => Some("ratio".to_string()),
        Some(unit) => unit_suffix(unit),
        None => None,
    };
    if let Some(suffix) = suffix
        && !suffix.is_empty()
        && !out.ends_with(&format!("_{suffix}"))
    {
        out.push('_');
        out.push_str(&suffix);
    }
    out
}

/// The sample name: the family name, plus `_total` for counters.
pub fn sample_name(family: &str, kind: MetricKind) -> String {
    if kind == MetricKind::Counter {
        format!("{family}_total")
    } else {
        family.to_string()
    }
}

/// A UCUM unit as Prometheus spells it in a name, such as `seconds` for
/// `s` or `bytes_per_second` for `By/s`. Annotations in braces are
/// dropped; `None` when nothing is left.
pub fn unit_suffix(unit: &str) -> Option<String> {
    let without_annotations = strip_braces(unit);
    let (main, per) = match without_annotations.split_once('/') {
        Some((main, per)) => (main, Some(per)),
        None => (without_annotations.as_str(), None),
    };
    let main = main.trim();
    let main = if main == "1" { "" } else { main };
    let mut out = String::new();
    if !main.is_empty() {
        out.push_str(&unit_word(main, false));
    }
    if let Some(per) = per.map(str::trim).filter(|p| !p.is_empty()) {
        if !out.is_empty() {
            out.push('_');
        }
        out.push_str("per_");
        out.push_str(&unit_word(per, true));
    }
    let out = sanitize(&out, false);
    (!out.is_empty()).then_some(out)
}

fn strip_braces(unit: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for c in unit.chars() {
        match c {
            '{' => depth += 1,
            '}' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

fn unit_word(unit: &str, singular: bool) -> String {
    let word = match unit {
        "d" => ("days", "day"),
        "h" => ("hours", "hour"),
        "min" => ("minutes", "minute"),
        "s" => ("seconds", "second"),
        "ms" => ("milliseconds", "millisecond"),
        "us" => ("microseconds", "microsecond"),
        "ns" => ("nanoseconds", "nanosecond"),
        "w" => ("weeks", "week"),
        "mo" => ("months", "month"),
        "y" => ("years", "year"),
        "By" => ("bytes", "byte"),
        "KiBy" => ("kibibytes", "kibibyte"),
        "MiBy" => ("mebibytes", "mebibyte"),
        "GiBy" => ("gibibytes", "gibibyte"),
        "TiBy" => ("tibibytes", "tibibyte"),
        "KBy" => ("kilobytes", "kilobyte"),
        "MBy" => ("megabytes", "megabyte"),
        "GBy" => ("gigabytes", "gigabyte"),
        "TBy" => ("terabytes", "terabyte"),
        "m" => ("meters", "meter"),
        "V" => ("volts", "volt"),
        "A" => ("amperes", "ampere"),
        "J" => ("joules", "joule"),
        "W" => ("watts", "watt"),
        "g" => ("grams", "gram"),
        "Cel" => ("celsius", "celsius"),
        "Hz" => ("hertz", "hertz"),
        "%" => ("percent", "percent"),
        other => return other.to_string(),
    };
    if singular { word.1 } else { word.0 }.to_string()
}

/// Replace characters Prometheus doesn't allow with `_`, collapse runs of
/// `_`, and keep the name from starting with a digit. Metric names may
/// contain `:`; label names may not.
fn sanitize(name: &str, metric: bool) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        let ok = c.is_ascii_alphanumeric() || c == '_' || (metric && c == ':');
        let c = if ok { c } else { '_' };
        if c == '_' && out.ends_with('_') {
            continue;
        }
        out.push(c);
    }
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

/// A Prometheus label name for an attribute key: invalid characters become
/// `_`, and a name that would start with a digit or `__` (reserved by
/// Prometheus) gets a `key_` or `key` prefix.
pub fn label_name(key: &str) -> String {
    let mut out: String = key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert_str(0, "key_");
    } else if out.starts_with("__") {
        out.insert_str(0, "key");
    }
    if out.is_empty() {
        out.push_str("key_empty");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use MetricKind::*;

    #[test]
    fn names_get_units_and_totals() {
        assert_eq!(
            family_name("http.server.request.duration", Some("s"), Histogram),
            "http_server_request_duration_seconds"
        );
        assert_eq!(
            family_name("http.server.active_requests", Some("{request}"), Gauge),
            "http_server_active_requests"
        );
        assert_eq!(family_name("jobs.done", None, Counter), "jobs_done");
        assert_eq!(sample_name("jobs_done", Counter), "jobs_done_total");
        assert_eq!(family_name("jobs_done_total", None, Counter), "jobs_done");
        assert_eq!(
            family_name("system.memory.usage", Some("By"), Gauge),
            "system_memory_usage_bytes"
        );
        assert_eq!(
            family_name("cpu.utilization", Some("1"), Gauge),
            "cpu_utilization_ratio"
        );
        assert_eq!(family_name("errors", Some("1"), Counter), "errors");
        assert_eq!(
            family_name("net.io.rate", Some("By/s"), Gauge),
            "net_io_rate_bytes_per_second"
        );
        assert_eq!(
            family_name("req.rate", Some("{request}/s"), Gauge),
            "req_rate_per_second"
        );
        assert_eq!(
            family_name("latency_seconds", Some("s"), Histogram),
            "latency_seconds",
            "the unit isn't added twice"
        );
        assert_eq!(
            family_name("9lives..cat-count", None, Gauge),
            "_9lives_cat_count"
        );
        assert_eq!(family_name("job:rate", None, Gauge), "job:rate");
    }

    #[test]
    fn label_names_are_sanitized() {
        assert_eq!(label_name("http.request.method"), "http_request_method");
        assert_eq!(label_name("1st"), "key_1st");
        assert_eq!(label_name("__name__"), "key__name__");
        assert_eq!(label_name("a:b"), "a_b");
    }
}
