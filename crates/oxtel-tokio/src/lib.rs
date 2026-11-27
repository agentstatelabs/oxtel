//! Metrics for a tokio runtime: its workers, its tasks, the tasks waiting
//! to run, and how busy it is. Part of
//! [oxtel](https://github.com/agentstatelabs/oxtel).
//!
//! One line records them every ten seconds, for as long as the value it
//! returns is kept:
//!
//! ```
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() {
//! let _runtime_metrics = oxtel_tokio::record(&tokio::runtime::Handle::current());
//! # }
//! ```
//!
//! | Metric | Type | What it is |
//! |---|---|---|
//! | `oxtel.tokio.worker.count` | gauge, `{thread}` | Worker threads. |
//! | `oxtel.tokio.task.count` | gauge, `{task}` | Tasks spawned and not yet finished. |
//! | `oxtel.tokio.global_queue.length` | gauge, `{task}` | Tasks waiting in the global queue, where tasks spawned from outside the runtime go. A queue that keeps growing means the workers can't keep up. |
//! | `oxtel.tokio.utilization` | gauge, `1` | The share of the workers' time spent running tasks since the last recording, from 0 to 1. Near 1, the runtime is saturated. |
//!
//! Built with `--cfg tokio_unstable`, as tokio's own unstable metrics
//! need, it also records the blocking pool, where `spawn_blocking` work
//! runs:
//!
//! | Metric | Type | What it is |
//! |---|---|---|
//! | `oxtel.tokio.blocking.thread.count` | gauge, `{thread}` | Blocking threads, by `oxtel.tokio.thread.state`: `busy` or `idle`. |
//! | `oxtel.tokio.blocking.queue.length` | gauge, `{task}` | Blocking work waiting for a thread. |
//!
//! The OpenTelemetry conventions don't define tokio metrics, so these are
//! named under `oxtel.`, shaped like the conventions' own runtime metrics
//! (`dotnet.thread_pool.*` and `nodejs.eventloop.utilization`). They're
//! recorded through `metrics`, so they go wherever the program's recorder
//! sends them: with `oxtel::init()`, to OTLP or Prometheus.
//!
//! The recording happens on a thread of its own, so it goes on when the
//! runtime is too busy to run anything else, which is when it matters.
//! [`Sampler`] records on demand instead, such as just before serving a
//! scrape, and names runtimes apart when a program has several.

use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use metrics::Label;
use tokio::runtime::{Handle, RuntimeMetrics};

/// How often [`record`] records.
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(10);

/// Records the runtime's metrics every [ten seconds](DEFAULT_INTERVAL), on
/// a thread of its own, until the returned [`Recording`] is dropped.
pub fn record(handle: &Handle) -> Recording {
    Sampler::new(handle).spawn_every(DEFAULT_INTERVAL)
}

/// Records one runtime's metrics when asked. It remembers the last time it
/// recorded, to say how busy the runtime has been since.
#[derive(Debug)]
pub struct Sampler {
    metrics: RuntimeMetrics,
    labels: Vec<Label>,
    /// When it last recorded, and the workers' busy time then.
    last: (Instant, Duration),
}

impl Sampler {
    /// A sampler for the runtime `handle` belongs to. How busy the runtime
    /// is gets measured from now.
    pub fn new(handle: &Handle) -> Sampler {
        describe_metrics();
        let metrics = handle.metrics();
        let busy = busy_time(&metrics);
        Sampler {
            metrics,
            labels: Vec::new(),
            last: (Instant::now(), busy),
        }
    }

    /// Names the runtime, as the `oxtel.tokio.runtime.name` label on every
    /// metric, for a program with more than one.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.labels
            .retain(|label| label.key() != "oxtel.tokio.runtime.name");
        self.labels
            .push(Label::new("oxtel.tokio.runtime.name", name.into()));
        self
    }

    /// Records the metrics now.
    pub fn sample(&mut self) {
        let workers = self.metrics.num_workers();
        let labels = &self.labels;
        metrics::gauge!("oxtel.tokio.worker.count", labels.clone()).set(workers as f64);
        metrics::gauge!("oxtel.tokio.task.count", labels.clone())
            .set(self.metrics.num_alive_tasks() as f64);
        metrics::gauge!("oxtel.tokio.global_queue.length", labels.clone())
            .set(self.metrics.global_queue_depth() as f64);

        let now = Instant::now();
        let busy = busy_time(&self.metrics);
        let (then, busy_then) = std::mem::replace(&mut self.last, (now, busy));
        let available = (now - then).as_secs_f64() * workers as f64;
        if cfg!(target_has_atomic = "64") && available > 0.0 {
            let used = busy.saturating_sub(busy_then).as_secs_f64();
            metrics::gauge!("oxtel.tokio.utilization", labels.clone())
                .set((used / available).clamp(0.0, 1.0));
        }

        #[cfg(tokio_unstable)]
        {
            let threads = self.metrics.num_blocking_threads();
            let idle = self.metrics.num_idle_blocking_threads();
            for (state, count) in [("busy", threads.saturating_sub(idle)), ("idle", idle)] {
                let mut labels = labels.clone();
                labels.push(Label::new("oxtel.tokio.thread.state", state));
                metrics::gauge!("oxtel.tokio.blocking.thread.count", labels).set(count as f64);
            }
            metrics::gauge!("oxtel.tokio.blocking.queue.length", labels.clone())
                .set(self.metrics.blocking_queue_depth() as f64);
        }
    }

    /// Records the metrics now and every `interval` after, on a thread of
    /// its own, until the returned [`Recording`] is dropped.
    pub fn spawn_every(mut self, interval: Duration) -> Recording {
        let (stop, stopped) = mpsc::channel::<()>();
        let thread = std::thread::Builder::new()
            .name("oxtel-tokio".into())
            .spawn(move || {
                loop {
                    self.sample();
                    // Wakes at once when the Recording is dropped.
                    if stopped.recv_timeout(interval) != Err(mpsc::RecvTimeoutError::Timeout) {
                        break;
                    }
                }
            })
            .expect("a thread can be started");
        Recording {
            stop: Some(stop),
            thread: Some(thread),
        }
    }
}

/// The workers' busy time, summed. tokio measures it only where 64-bit
/// atomics exist; elsewhere it's always zero, and utilization isn't
/// recorded.
#[cfg(target_has_atomic = "64")]
fn busy_time(metrics: &RuntimeMetrics) -> Duration {
    (0..metrics.num_workers())
        .map(|worker| metrics.worker_total_busy_duration(worker))
        .sum()
}

#[cfg(not(target_has_atomic = "64"))]
fn busy_time(_: &RuntimeMetrics) -> Duration {
    Duration::ZERO
}

/// A runtime's metrics being recorded. Dropping it stops the recording.
#[derive(Debug)]
#[must_use = "recording stops when this is dropped"]
pub struct Recording {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Recording {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Gives the current `metrics` recorder the descriptions of the runtime
/// metrics. [`Sampler::new`] does this.
pub fn describe_metrics() {
    metrics::describe_gauge!("oxtel.tokio.worker.count", "Worker threads.");
    metrics::describe_gauge!(
        "oxtel.tokio.task.count",
        "Tasks spawned and not yet finished."
    );
    metrics::describe_gauge!(
        "oxtel.tokio.global_queue.length",
        "Tasks waiting in the runtime's global queue."
    );
    metrics::describe_gauge!(
        "oxtel.tokio.utilization",
        "The share of the workers' time spent running tasks, from 0 to 1."
    );
    #[cfg(tokio_unstable)]
    {
        metrics::describe_gauge!("oxtel.tokio.blocking.thread.count", "Blocking threads.");
        metrics::describe_gauge!(
            "oxtel.tokio.blocking.queue.length",
            "Blocking work waiting for a thread."
        );
    }
}
