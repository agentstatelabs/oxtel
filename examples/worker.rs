//! A background worker: it takes jobs off a queue and runs each on a tokio
//! task of its own, inside a span. Every log line a job writes carries the
//! job's fields and trace id, and each job is counted by outcome and timed.
//! This one stops once the queue is empty and closed; a real one runs until
//! it's told to stop.
//!
//! ```bash
//! cargo run -p oxtel-examples --example worker
//! OTEL_METRICS_EXPORTER=console cargo run -p oxtel-examples --example worker   # and the metrics, at exit
//! ```
//!
//! A worker of your own needs:
//!
//! ```toml
//! [dependencies]
//! oxtel = "0.4"
//! metrics = "0.24"
//! tokio = { version = "1", features = ["macros", "rt-multi-thread", "sync", "time"] }
//! tracing = "0.1"
//! ```

use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tracing::Instrument;

/// A unit of work, as it might arrive from a message queue.
struct Job {
    id: u64,
    kind: &'static str,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // First, so the metrics below find oxtel's recorder. Keep the guard
    // until main returns.
    let _telemetry = oxtel::init()?;
    metrics::describe_counter!("jobs.processed", "Jobs processed, by outcome.");
    metrics::describe_histogram!(
        "jobs.duration",
        metrics::Unit::Seconds,
        "How long each job took."
    );

    let (queue, jobs) = mpsc::channel(16);
    let worker = tokio::spawn(work(jobs));
    for (id, kind) in [
        (1, "resize"),
        (2, "resize"),
        (3, "transcode"),
        (4, "resize"),
    ] {
        queue.send(Job { id, kind }).await?;
    }
    // Closing the queue lets the worker finish what's left, then stop.
    drop(queue);
    worker.await?;
    Ok(())
}

/// Takes jobs off the queue, one at a time, until it's closed.
async fn work(mut jobs: mpsc::Receiver<Job>) {
    while let Some(job) = jobs.recv().await {
        let span = tracing::info_span!("job", job.id = job.id, job.kind = job.kind);
        let started = Instant::now();
        // Each job runs on a task of its own, so a panic fails that job and
        // not the worker. A new task doesn't start inside the current span:
        // `instrument` carries the job's span onto it.
        let outcome = match tokio::spawn(run(job).instrument(span)).await {
            Ok(Ok(())) => "ok",
            Ok(Err(_)) => "failed",
            Err(_) => "panicked",
        };
        metrics::counter!("jobs.processed", "outcome" => outcome).increment(1);
        metrics::histogram!("jobs.duration").record(started.elapsed().as_secs_f64());
    }
}

/// One job's work. It runs inside the job's span, so its log lines carry
/// the job's fields and trace id.
async fn run(job: Job) -> Result<(), String> {
    tracing::info!("started");
    // Stands in for the real work.
    tokio::time::sleep(Duration::from_millis(20)).await;
    if job.kind == "transcode" {
        let error = "no encoder for this format";
        tracing::warn!(error, "failed");
        return Err(error.into());
    }
    tracing::info!("done");
    Ok(())
}
