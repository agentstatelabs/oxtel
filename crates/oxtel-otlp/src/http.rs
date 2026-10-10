//! OTLP over HTTP with protobuf bodies (`http/protobuf`).

use std::io::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use oxtel_sdk::Resource;
use oxtel_sdk::export::ExportError;
use oxtel_sdk::logs::{LogExporter, LogRecord};
use oxtel_sdk::metrics::{MetricExporter, MetricFamily};
use oxtel_sdk::sdk_metrics::ExporterMetrics;
use oxtel_sdk::trace::{SpanData, SpanExporter};
use prost::Message;

use crate::proto::collector::logs::v1::ExportLogsServiceResponse;
use crate::proto::collector::metrics::v1::ExportMetricsServiceResponse;
use crate::proto::collector::trace::v1::ExportTraceServiceResponse;
use crate::transform::{logs_request, metrics_request, traces_request};

const DEFAULT_ENDPOINT: &str = "http://localhost:4318";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// A kind of telemetry OTLP carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Signal {
    /// Spans, sent to `/v1/traces`.
    Traces,
    /// Metrics, sent to `/v1/metrics`.
    Metrics,
    /// Log records, sent to `/v1/logs`.
    Logs,
}

impl Signal {
    const ALL: [Signal; 3] = [Signal::Traces, Signal::Metrics, Signal::Logs];

    /// The signal's name in OTLP paths: `traces`, `metrics` or `logs`.
    pub fn name(self) -> &'static str {
        match self {
            Signal::Traces => "traces",
            Signal::Metrics => "metrics",
            Signal::Logs => "logs",
        }
    }

    /// oxtel's own metrics for this signal's exporter: its component type,
    /// and the count it records.
    fn sdk_metrics(self) -> (&'static str, &'static str) {
        match self {
            Signal::Traces => ("otlp_http_span_exporter", "otel.sdk.exporter.span.exported"),
            Signal::Metrics => (
                "otlp_http_metric_exporter",
                "otel.sdk.exporter.metric_data_point.exported",
            ),
            Signal::Logs => ("otlp_http_log_exporter", "otel.sdk.exporter.log.exported"),
        }
    }

    fn env(self) -> &'static str {
        match self {
            Signal::Traces => "TRACES",
            Signal::Metrics => "METRICS",
            Signal::Logs => "LOGS",
        }
    }
}

/// Exports to an OTLP/HTTP receiver, such as an OpenTelemetry Collector:
/// spans to `{endpoint}/v1/traces`, metrics to `{endpoint}/v1/metrics` and
/// log records to `{endpoint}/v1/logs`.
/// Clones share one connection pool, so one exporter can serve every
/// pipeline.
///
/// Retryable failures (429, 502, 503, 504 and connection errors) are
/// retried with exponential backoff, honouring `Retry-After`, until the
/// export timeout. Other failures aren't retried.
#[derive(Clone)]
pub struct OtlpHttpExporter {
    inner: Arc<Inner>,
}

struct Inner {
    agent: ureq::Agent,
    targets: [Target; 3],
    initial_backoff: Duration,
}

/// Where one signal goes, and how.
struct Target {
    url: String,
    headers: Vec<(String, String)>,
    gzip: bool,
    timeout: Duration,
    rejected: AtomicU64,
    metrics: ExporterMetrics,
}

impl OtlpHttpExporter {
    /// A builder, configured from the standard environment variables.
    pub fn builder() -> OtlpHttpExporterBuilder {
        OtlpHttpExporterBuilder::from_env()
    }

    /// The URL `signal` is sent to.
    pub fn url(&self, signal: Signal) -> &str {
        &self.inner.targets[signal as usize].url
    }

    /// Items of `signal` (spans, data points, log records) that the
    /// receiver accepted the request for but rejected, as reported in
    /// partial-success responses.
    pub fn rejected(&self, signal: Signal) -> u64 {
        self.inner.targets[signal as usize]
            .rejected
            .load(Ordering::Relaxed)
    }

    /// Send `message`, carrying `items` of `signal`, retrying as the
    /// specification allows. `rejected_in` reads the rejected count from a
    /// success response.
    fn send(
        &self,
        signal: Signal,
        items: usize,
        message: &impl Message,
        rejected_in: impl Fn(&[u8]) -> Option<i64>,
    ) -> Result<(), ExportError> {
        let target = &self.inner.targets[signal as usize];
        let started = Instant::now();
        let mut status = None;
        let mut rejected = 0;
        let result = self.deliver(target, message, &rejected_in, &mut status, &mut rejected);
        target
            .metrics
            .record(items, rejected, &result, started.elapsed(), status);
        result
    }

    /// The retry loop of [`send`](Self::send). Leaves the last HTTP status
    /// and the rejected count in `status` and `rejected`.
    fn deliver(
        &self,
        target: &Target,
        message: &impl Message,
        rejected_in: &impl Fn(&[u8]) -> Option<i64>,
        status: &mut Option<u16>,
        rejected: &mut usize,
    ) -> Result<(), ExportError> {
        let mut body = message.encode_to_vec();
        if target.gzip {
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            encoder
                .write_all(&body)
                .and_then(|()| encoder.finish())
                .map(|compressed| body = compressed)
                .map_err(|e| ExportError::Transport(format!("gzip: {e}")))?;
        }
        let deadline = Instant::now() + target.timeout;
        let mut backoff = self.inner.initial_backoff;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let (error, wait) = match self.post(target, &body, remaining) {
                Ok(mut response) => {
                    let code = response.status().as_u16();
                    *status = Some(code);
                    if (200..300).contains(&code) {
                        let reply = response.body_mut().read_to_vec().unwrap_or_default();
                        if let Some(n) = rejected_in(&reply)
                            && n > 0
                        {
                            target.rejected.fetch_add(n as u64, Ordering::Relaxed);
                            *rejected = usize::try_from(n).unwrap_or(usize::MAX);
                        }
                        return Ok(());
                    }
                    let error = ExportError::Rejected {
                        status: code,
                        message: response
                            .body_mut()
                            .read_to_string()
                            .unwrap_or_default()
                            .chars()
                            .take(512)
                            .collect(),
                    };
                    if !retryable(code) {
                        return Err(error);
                    }
                    (error, retry_after(&response))
                }
                Err(ureq::Error::Timeout(_)) => (ExportError::Timeout, None),
                Err(e) => (ExportError::Transport(e.to_string()), None),
            };
            let wait = wait.unwrap_or_else(|| jitter(backoff));
            if Instant::now() + wait >= deadline {
                return Err(error);
            }
            std::thread::sleep(wait);
            backoff = (backoff * 2).min(Duration::from_secs(5));
        }
    }

    fn post(
        &self,
        target: &Target,
        body: &[u8],
        timeout: Duration,
    ) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
        let mut request = self
            .inner
            .agent
            .post(&target.url)
            .config()
            .timeout_global(Some(timeout.max(Duration::from_millis(1))))
            .build()
            .header("Content-Type", "application/x-protobuf");
        if target.gzip {
            request = request.header("Content-Encoding", "gzip");
        }
        for (name, value) in &target.headers {
            request = request.header(name, value);
        }
        request.send(body)
    }
}

#[derive(Clone, Debug)]
struct TargetConfig {
    url: String,
    headers: Vec<(String, String)>,
    gzip: bool,
    timeout: Duration,
}

/// Builds an [`OtlpHttpExporter`]. Settings set on the builder override the
/// environment, for every signal unless they name one.
#[derive(Clone, Debug)]
pub struct OtlpHttpExporterBuilder {
    targets: [TargetConfig; 3],
    initial_backoff: Duration,
}

impl OtlpHttpExporterBuilder {
    /// Settings from the environment, for each signal (`TRACES`, `METRICS`,
    /// `LOGS`):
    /// - endpoint: `OTEL_EXPORTER_OTLP_{SIGNAL}_ENDPOINT`, used as is, or
    ///   `OTEL_EXPORTER_OTLP_ENDPOINT` with `/v1/{signal}` appended, or
    ///   `http://localhost:4318/v1/{signal}`;
    /// - headers: `OTEL_EXPORTER_OTLP_HEADERS`, then
    ///   `OTEL_EXPORTER_OTLP_{SIGNAL}_HEADERS`;
    /// - compression: `OTEL_EXPORTER_OTLP_[{SIGNAL}_]COMPRESSION` (`gzip`);
    /// - timeout: `OTEL_EXPORTER_OTLP_[{SIGNAL}_]TIMEOUT`, in milliseconds.
    pub fn from_env() -> Self {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        let target = |signal: Signal| {
            let specific =
                |setting: &str| var(&format!("OTEL_EXPORTER_OTLP_{}_{setting}", signal.env()));
            let url = resolve_url(
                specific("ENDPOINT").as_deref(),
                var("OTEL_EXPORTER_OTLP_ENDPOINT").as_deref(),
                signal.name(),
            );
            let mut headers: Vec<(String, String)> = Vec::new();
            for raw in [var("OTEL_EXPORTER_OTLP_HEADERS"), specific("HEADERS")]
                .into_iter()
                .flatten()
            {
                for (k, v) in parse_headers(&raw) {
                    headers.retain(|(existing, _)| !existing.eq_ignore_ascii_case(&k));
                    headers.push((k, v));
                }
            }
            let compression =
                specific("COMPRESSION").or_else(|| var("OTEL_EXPORTER_OTLP_COMPRESSION"));
            let timeout = specific("TIMEOUT")
                .or_else(|| var("OTEL_EXPORTER_OTLP_TIMEOUT"))
                .and_then(|ms| ms.trim().parse::<u64>().ok())
                .map_or(DEFAULT_TIMEOUT, Duration::from_millis);
            TargetConfig {
                url,
                headers,
                gzip: compression.is_some_and(|c| c.trim().eq_ignore_ascii_case("gzip")),
                timeout,
            }
        };
        OtlpHttpExporterBuilder {
            targets: Signal::ALL.map(target),
            initial_backoff: Duration::from_millis(500),
        }
    }

    /// Send every signal to this receiver, at `{endpoint}/v1/{signal}`,
    /// such as `https://collector.example.com:4318`.
    pub fn with_endpoint(mut self, endpoint: &str) -> Self {
        for signal in Signal::ALL {
            self.targets[signal as usize].url = resolve_url(None, Some(endpoint), signal.name());
        }
        self
    }

    /// Send `signal` to this URL exactly, such as
    /// `https://collector.example.com:4318/v1/traces`.
    pub fn with_url(mut self, signal: Signal, url: impl Into<String>) -> Self {
        self.targets[signal as usize].url = url.into();
        self
    }

    /// Add a header to every request, such as an API key.
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        let (name, value) = (name.into(), value.into());
        for target in &mut self.targets {
            target.headers.push((name.clone(), value.clone()));
        }
        self
    }

    /// Gzip request bodies.
    pub fn with_gzip(mut self, gzip: bool) -> Self {
        for target in &mut self.targets {
            target.gzip = gzip;
        }
        self
    }

    /// Give up on an export, retries included, after this long.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        for target in &mut self.targets {
            target.timeout = timeout;
        }
        self
    }

    /// The first retry's wait; each later one doubles, up to 5 seconds.
    pub fn with_initial_backoff(mut self, backoff: Duration) -> Self {
        self.initial_backoff = backoff;
        self
    }

    /// The exporter.
    pub fn build(self) -> OtlpHttpExporter {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .user_agent(concat!("oxtel/", env!("CARGO_PKG_VERSION")))
            .build();
        OtlpHttpExporter {
            inner: Arc::new(Inner {
                agent: ureq::Agent::new_with_config(config),
                targets: Signal::ALL.map(|signal| {
                    let t = &self.targets[signal as usize];
                    let (kind, exported) = signal.sdk_metrics();
                    let server = server(&t.url);
                    Target {
                        url: t.url.clone(),
                        headers: t.headers.clone(),
                        gzip: t.gzip,
                        timeout: t.timeout,
                        rejected: AtomicU64::new(0),
                        metrics: ExporterMetrics::new(
                            kind,
                            exported,
                            server.as_ref().map(|(host, port)| (host.as_str(), *port)),
                        ),
                    }
                }),
                initial_backoff: self.initial_backoff,
            }),
        }
    }
}

/// The URL for `signal`: the signal-specific endpoint as is, or the general
/// endpoint with `/v1/{signal}` appended.
pub fn resolve_url(signal_endpoint: Option<&str>, endpoint: Option<&str>, signal: &str) -> String {
    match (signal_endpoint, endpoint) {
        (Some(url), _) => url.trim().to_string(),
        (None, Some(base)) => format!("{}/v1/{signal}", base.trim().trim_end_matches('/')),
        (None, None) => format!("{DEFAULT_ENDPOINT}/v1/{signal}"),
    }
}

/// The host and port of an `http(s)://host[:port]/...` URL, for oxtel's own
/// metrics. User information in the URL is left out.
fn server(url: &str) -> Option<(String, u16)> {
    let (scheme, rest) = url.trim().split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let default = match scheme.to_ascii_lowercase().as_str() {
        "https" => 443,
        "http" => 80,
        _ => return None,
    };
    let (host, port) = match authority.strip_prefix('[') {
        Some(v6) => {
            let (host, after) = v6.split_once(']')?;
            (host, after.strip_prefix(':'))
        }
        None => match authority.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        },
    };
    let port = port.map_or(Some(default), |p| p.parse().ok())?;
    (!host.is_empty()).then(|| (host.to_string(), port))
}

/// `key1=value1,key2=value2`, values percent-decoded. Malformed pairs are
/// skipped.
pub fn parse_headers(raw: &str) -> Vec<(String, String)> {
    raw.split(',')
        .filter_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            let k = k.trim();
            if k.is_empty() {
                return None;
            }
            Some((k.to_string(), percent_decode(v.trim())?))
        })
        .collect()
}

fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            out.push(u8::from_str_radix(s.get(i + 1..i + 3)?, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn retryable(status: u16) -> bool {
    matches!(status, 429 | 502 | 503 | 504)
}

fn retry_after(response: &ureq::http::Response<ureq::Body>) -> Option<Duration> {
    let value = response.headers().get("retry-after")?.to_str().ok()?;
    value.trim().parse::<u64>().ok().map(Duration::from_secs)
}

impl SpanExporter for OtlpHttpExporter {
    fn export(&self, batch: &[SpanData], resource: &Resource) -> Result<(), ExportError> {
        if batch.is_empty() {
            return Ok(());
        }
        self.send(
            Signal::Traces,
            batch.len(),
            &traces_request(batch, resource),
            |reply| {
                Some(
                    ExportTraceServiceResponse::decode(reply)
                        .ok()?
                        .partial_success?
                        .rejected_spans,
                )
            },
        )
    }
}

impl LogExporter for OtlpHttpExporter {
    fn export(&self, batch: &[LogRecord], resource: &Resource) -> Result<(), ExportError> {
        if batch.is_empty() {
            return Ok(());
        }
        self.send(
            Signal::Logs,
            batch.len(),
            &logs_request(batch, resource),
            |reply| {
                Some(
                    ExportLogsServiceResponse::decode(reply)
                        .ok()?
                        .partial_success?
                        .rejected_log_records,
                )
            },
        )
    }
}

impl MetricExporter for OtlpHttpExporter {
    fn export(&self, metrics: &[MetricFamily], resource: &Resource) -> Result<(), ExportError> {
        if metrics.iter().all(|m| m.points.is_empty()) {
            return Ok(());
        }
        let points = metrics.iter().map(|m| m.points.len()).sum();
        self.send(
            Signal::Metrics,
            points,
            &metrics_request(metrics, resource),
            |reply| {
                Some(
                    ExportMetricsServiceResponse::decode(reply)
                        .ok()?
                        .partial_success?
                        .rejected_data_points,
                )
            },
        )
    }
}

/// `d` scaled by a random factor in [0.8, 1.2), so that many exporters
/// retrying together spread out.
fn jitter(d: Duration) -> Duration {
    d.mul_f64(0.8 + 0.4 * fastrand::f64())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_follow_the_specification() {
        assert_eq!(
            resolve_url(None, None, "traces"),
            "http://localhost:4318/v1/traces"
        );
        assert_eq!(
            resolve_url(None, Some("https://c.example:4318/"), "traces"),
            "https://c.example:4318/v1/traces"
        );
        assert_eq!(
            resolve_url(
                Some("https://c.example/custom"),
                Some("https://ignored"),
                "traces"
            ),
            "https://c.example/custom"
        );
    }

    #[test]
    fn servers_are_read_from_urls_without_credentials() {
        let host = |url: &str| server(url).map(|(h, p)| format!("{h}:{p}"));
        assert_eq!(
            host("http://c.example:4318/v1/traces").as_deref(),
            Some("c.example:4318")
        );
        assert_eq!(
            host("https://c.example/v1/logs").as_deref(),
            Some("c.example:443")
        );
        assert_eq!(host("http://c.example").as_deref(), Some("c.example:80"));
        assert_eq!(
            host("http://user:secret@c.example:9/x").as_deref(),
            Some("c.example:9")
        );
        assert_eq!(
            host("http://[::1]:4318/v1/metrics").as_deref(),
            Some("::1:4318")
        );
        assert_eq!(host("http://[::1]/").as_deref(), Some("::1:80"));
        assert_eq!(host("unix:///run/otel.sock"), None);
        assert_eq!(host("http://c.example:port/"), None);
    }

    #[test]
    fn headers_are_percent_decoded() {
        assert_eq!(
            parse_headers("api-key=secret%20value, x-tenant = a ,broken"),
            vec![
                ("api-key".to_string(), "secret value".to_string()),
                ("x-tenant".to_string(), "a".to_string()),
            ]
        );
    }
}
