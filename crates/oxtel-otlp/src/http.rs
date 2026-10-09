//! OTLP over HTTP with protobuf bodies (`http/protobuf`).

use std::io::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use oxtel_sdk::Resource;
use oxtel_sdk::trace::{ExportError, SpanData, SpanExporter};
use prost::Message;

use crate::proto::collector::trace::v1::ExportTraceServiceResponse;
use crate::transform::traces_request;

const DEFAULT_ENDPOINT: &str = "http://localhost:4318";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// Exports spans to an OTLP/HTTP receiver, such as an OpenTelemetry
/// Collector, at `{endpoint}/v1/traces`.
///
/// Retryable failures (429, 502, 503, 504 and connection errors) are
/// retried with exponential backoff, honouring `Retry-After`, until the
/// export timeout. Other failures aren't retried.
pub struct OtlpHttpExporter {
    agent: ureq::Agent,
    url: String,
    headers: Vec<(String, String)>,
    gzip: bool,
    timeout: Duration,
    initial_backoff: Duration,
    rejected: AtomicU64,
}

impl OtlpHttpExporter {
    /// A builder, configured from the standard environment variables.
    pub fn builder() -> OtlpHttpExporterBuilder {
        OtlpHttpExporterBuilder::from_env()
    }

    /// The URL spans are sent to.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Spans the receiver accepted the request for but rejected, as
    /// reported in partial-success responses.
    pub fn rejected_spans(&self) -> u64 {
        self.rejected.load(Ordering::Relaxed)
    }

    fn send(&self, body: &[u8]) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
        let mut request = self
            .agent
            .post(&self.url)
            .header("Content-Type", "application/x-protobuf");
        if self.gzip {
            request = request.header("Content-Encoding", "gzip");
        }
        for (name, value) in &self.headers {
            request = request.header(name, value);
        }
        request.send(body)
    }
}

/// Builds an [`OtlpHttpExporter`]. Settings set on the builder override the
/// environment.
#[derive(Clone, Debug)]
pub struct OtlpHttpExporterBuilder {
    url: String,
    headers: Vec<(String, String)>,
    gzip: bool,
    timeout: Duration,
    initial_backoff: Duration,
}

impl OtlpHttpExporterBuilder {
    /// Settings from the environment:
    /// - endpoint: `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`, used as is, or
    ///   `OTEL_EXPORTER_OTLP_ENDPOINT` with `/v1/traces` appended, or
    ///   `http://localhost:4318/v1/traces`;
    /// - headers: `OTEL_EXPORTER_OTLP_HEADERS`, then
    ///   `OTEL_EXPORTER_OTLP_TRACES_HEADERS`;
    /// - compression: `OTEL_EXPORTER_OTLP_[TRACES_]COMPRESSION` (`gzip`);
    /// - timeout: `OTEL_EXPORTER_OTLP_[TRACES_]TIMEOUT`, in milliseconds.
    pub fn from_env() -> Self {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        let url = resolve_url(
            var("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT").as_deref(),
            var("OTEL_EXPORTER_OTLP_ENDPOINT").as_deref(),
            "traces",
        );
        let mut headers = Vec::new();
        for name in [
            "OTEL_EXPORTER_OTLP_HEADERS",
            "OTEL_EXPORTER_OTLP_TRACES_HEADERS",
        ] {
            for (k, v) in var(name).map(|h| parse_headers(&h)).unwrap_or_default() {
                headers
                    .retain(|(existing, _): &(String, String)| !existing.eq_ignore_ascii_case(&k));
                headers.push((k, v));
            }
        }
        let compression = var("OTEL_EXPORTER_OTLP_TRACES_COMPRESSION")
            .or_else(|| var("OTEL_EXPORTER_OTLP_COMPRESSION"));
        let timeout = var("OTEL_EXPORTER_OTLP_TRACES_TIMEOUT")
            .or_else(|| var("OTEL_EXPORTER_OTLP_TIMEOUT"))
            .and_then(|ms| ms.trim().parse::<u64>().ok())
            .map_or(DEFAULT_TIMEOUT, Duration::from_millis);
        OtlpHttpExporterBuilder {
            url,
            headers,
            gzip: compression.is_some_and(|c| c.trim().eq_ignore_ascii_case("gzip")),
            timeout,
            initial_backoff: Duration::from_millis(500),
        }
    }

    /// Send to this URL exactly, such as
    /// `https://collector.example.com:4318/v1/traces`.
    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.url = url.into();
        self
    }

    /// Add a header to every request, such as an API key.
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Gzip request bodies.
    pub fn with_gzip(mut self, gzip: bool) -> Self {
        self.gzip = gzip;
        self
    }

    /// Give up on an export, retries included, after this long.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
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
            .timeout_global(Some(self.timeout))
            .user_agent(concat!("oxtel/", env!("CARGO_PKG_VERSION")))
            .build();
        OtlpHttpExporter {
            agent: ureq::Agent::new_with_config(config),
            url: self.url,
            headers: self.headers,
            gzip: self.gzip,
            timeout: self.timeout,
            initial_backoff: self.initial_backoff,
            rejected: AtomicU64::new(0),
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
        let mut body = traces_request(batch, resource).encode_to_vec();
        if self.gzip {
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            encoder
                .write_all(&body)
                .and_then(|()| encoder.finish())
                .map(|compressed| body = compressed)
                .map_err(|e| ExportError::Transport(format!("gzip: {e}")))?;
        }
        let deadline = Instant::now() + self.timeout;
        let mut backoff = self.initial_backoff;
        loop {
            let (error, wait) = match self.send(&body) {
                Ok(mut response) => {
                    let status = response.status().as_u16();
                    if (200..300).contains(&status) {
                        let reply = response.body_mut().read_to_vec().unwrap_or_default();
                        if let Ok(reply) = ExportTraceServiceResponse::decode(reply.as_slice())
                            && let Some(partial) = reply.partial_success
                            && partial.rejected_spans > 0
                        {
                            self.rejected
                                .fetch_add(partial.rejected_spans as u64, Ordering::Relaxed);
                        }
                        return Ok(());
                    }
                    let error = ExportError::Rejected {
                        status,
                        message: response
                            .body_mut()
                            .read_to_string()
                            .unwrap_or_default()
                            .chars()
                            .take(512)
                            .collect(),
                    };
                    if !retryable(status) {
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
