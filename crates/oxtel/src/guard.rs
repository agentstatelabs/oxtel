//! The guard that keeps the pipelines running and shuts them down.

use std::net::SocketAddr;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use oxtel_prometheus::PrometheusServer;
use oxtel_sdk::export::ExportError;
use oxtel_sdk::logs::LoggerProvider;
use oxtel_sdk::metrics::MeterProvider;
use oxtel_sdk::trace::TracerProvider;

/// Keeps oxtel's pipelines running. Dropping it, or calling
/// [`shutdown`](Guard::shutdown), exports what's pending and stops them.
///
/// Every pipeline shuts down at once, and the guard waits at most the
/// shutdown timeout (5 s unless the builder sets another) for all of them,
/// so an unreachable collector delays exit by that much at most.
#[must_use = "telemetry stops when the guard is dropped; keep it alive in main"]
pub struct Guard {
    pub(crate) traces: Option<TracerProvider>,
    pub(crate) logs: Option<LoggerProvider>,
    pub(crate) metrics: Option<MeterProvider>,
    pub(crate) prometheus: Option<PrometheusServer>,
    pub(crate) timeout: Duration,
}

impl Guard {
    /// The trace pipeline, unless the SDK is disabled.
    pub fn tracer_provider(&self) -> Option<&TracerProvider> {
        self.traces.as_ref()
    }

    /// The log pipeline, if any exporter wants logs.
    pub fn logger_provider(&self) -> Option<&LoggerProvider> {
        self.logs.as_ref()
    }

    /// The metrics pipeline, if any exporter wants metrics. Serve
    /// `/metrics` from it yourself with `oxtel_prometheus`.
    pub fn meter_provider(&self) -> Option<&MeterProvider> {
        self.metrics.as_ref()
    }

    /// Where the built-in Prometheus endpoint listens, if it runs.
    pub fn prometheus_addr(&self) -> Option<SocketAddr> {
        self.prometheus.as_ref().map(PrometheusServer::local_addr)
    }

    /// Export everything pending in every pipeline, and wait for it.
    /// Returns the first failure.
    pub fn force_flush(&self) -> Result<(), ExportError> {
        std::thread::scope(|s| {
            let traces = self.traces.as_ref().map(|t| s.spawn(|| t.force_flush()));
            let logs = self.logs.as_ref().map(|l| s.spawn(|| l.force_flush()));
            let metrics = self.metrics.as_ref().map(|m| s.spawn(|| m.force_flush()));
            [traces, logs, metrics]
                .into_iter()
                .flatten()
                .map(|h| h.join().unwrap_or(Err(ExportError::Shutdown)))
                .fold(Ok(()), Result::and)
        })
    }

    /// Export what's pending and stop every pipeline, waiting at most the
    /// shutdown timeout. Returns the first failure, or
    /// [`ExportError::Timeout`] if a pipeline didn't finish in time.
    pub fn shutdown(mut self) -> Result<(), ExportError> {
        self.shutdown_all()
    }

    fn shutdown_all(&mut self) -> Result<(), ExportError> {
        let (done, results) = mpsc::channel();
        let mut pending = 0;
        let mut spawn = |work: Box<dyn FnOnce() -> Result<(), ExportError> + Send>| {
            let done = done.clone();
            let spawned = std::thread::Builder::new()
                .name("oxtel-shutdown".into())
                .spawn(move || {
                    let _ = done.send(work());
                });
            if spawned.is_ok() {
                pending += 1;
            }
        };
        if let Some(t) = self.traces.take() {
            spawn(Box::new(move || t.shutdown()));
        }
        if let Some(l) = self.logs.take() {
            spawn(Box::new(move || l.shutdown()));
        }
        if let Some(m) = self.metrics.take() {
            spawn(Box::new(move || m.shutdown()));
        }
        if let Some(p) = self.prometheus.take() {
            spawn(Box::new(move || {
                drop(p);
                Ok(())
            }));
        }
        let deadline = Instant::now() + self.timeout;
        let mut result = Ok(());
        for _ in 0..pending {
            match results.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(r) => result = result.and(r),
                // The rest carry on in the background; stop waiting.
                Err(_) => return Err(ExportError::Timeout),
            }
        }
        result
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.shutdown_all();
    }
}
