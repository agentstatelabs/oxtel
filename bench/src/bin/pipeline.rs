//! End to end: spans from four threads, through each stack's batch
//! processor and OTLP/HTTP exporter, to a receiver that decodes and counts
//! them.
//!
//! `cargo run --release -p oxtel-bench --bin pipeline`
//!
//! Every run is its own process, so neither stack inherits the other's
//! one-time costs (the first `tracing` workload in a process allocates and
//! frees tens of MiB of registry memory), and each warms up before it's
//! measured. The receiver runs in the parent, so its decoding isn't charged
//! to either stack. Each child reports its own CPU time and peak heap.
//!
//! Modes:
//! - **burst**: as fast as the threads can go, against a fast backend and a
//!   slow one (250 ms per request). Producers outrun any single export
//!   thread, so queues fill and spans drop; what matters is throughput,
//!   that nothing blocks, and that memory stays bounded.
//! - **paced**: 40,000 spans/s for 5 s, a rate both stacks sustain, to
//!   compare CPU per span with everything delivered.

use std::alloc::System;
use std::io::{BufRead, BufReader, Write as _};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::WithExportConfig;
use oxtel_bench::Receiver;
use stats_alloc::{INSTRUMENTED_SYSTEM, StatsAlloc};
use tracing::Dispatch;
use tracing_subscriber::layer::SubscriberExt;

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

const THREADS: usize = 4;
const BURST_PER_THREAD: usize = 250_000;
const PACED_RATE: usize = 40_000;
const PACED_SECS: u64 = 5;
const REPEATS: usize = 3;
const STACKS: [&str; 2] = ["oxtel", "otel"];

/// Live heap. stats_alloc already counts realloc growth in
/// `bytes_allocated` (and shrinkage in `bytes_deallocated`);
/// `bytes_reallocated` is the same change again, so it isn't added.
fn heap_in_use() -> isize {
    let s = GLOBAL.stats();
    s.bytes_allocated as isize - s.bytes_deallocated as isize
}

// ─── Child: one stack, one mode ─────────────────────────────

enum Stack {
    Oxtel(oxtel_sdk::trace::TracerProvider),
    Otel(opentelemetry_sdk::trace::SdkTracerProvider),
}

impl Stack {
    fn new(name: &str, url: &str) -> (Stack, Dispatch) {
        match name {
            "oxtel" => {
                let provider = oxtel_sdk::trace::TracerProvider::builder()
                    .with_resource(oxtel_sdk::Resource::builder().without_env().build())
                    .with_sampler(oxtel_sdk::trace::Sampler::AlwaysOn)
                    .with_batch_exporter_config(
                        oxtel_otlp::OtlpHttpExporter::builder()
                            .with_url(url)
                            .build(),
                        oxtel_sdk::trace::BatchConfig::default(),
                    )
                    .build();
                let dispatch = Dispatch::new(tracing_subscriber::registry().with(provider.layer()));
                (Stack::Oxtel(provider), dispatch)
            }
            _ => {
                let exporter = opentelemetry_otlp::SpanExporter::builder()
                    .with_http()
                    .with_endpoint(url)
                    .build()
                    .expect("OTLP exporter");
                let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
                    .with_sampler(opentelemetry_sdk::trace::Sampler::AlwaysOn)
                    .with_span_processor(
                        opentelemetry_sdk::trace::BatchSpanProcessor::builder(exporter).build(),
                    )
                    .build();
                let layer = tracing_opentelemetry::layer()
                    .with_tracer(provider.tracer("bench"))
                    .with_location(false)
                    .with_threads(false)
                    .with_tracked_inactivity(false);
                let dispatch = Dispatch::new(tracing_subscriber::registry().with(layer));
                (Stack::Otel(provider), dispatch)
            }
        }
    }

    fn flush(&self) {
        match self {
            Stack::Oxtel(p) => {
                let _ = p.force_flush();
            }
            Stack::Otel(p) => {
                let _ = p.force_flush();
            }
        }
    }
}

fn produce(dispatch: &Dispatch, per_thread: usize, paced: bool) {
    // Paced: each thread emits 100 spans, then waits for the next 100th of
    // its share of a second.
    let per_tick = 100;
    let tick = Duration::from_secs_f64(per_tick as f64 / (PACED_RATE / THREADS) as f64);
    std::thread::scope(|s| {
        for t in 0..THREADS {
            let dispatch = dispatch.clone();
            s.spawn(move || {
                tracing::dispatcher::with_default(&dispatch, || {
                    let start = Instant::now();
                    for i in 0..per_thread {
                        let span = tracing::info_span!("op", t, i, route = "/items");
                        let _entered = span.enter();
                        drop(_entered);
                        drop(span);
                        if paced && (i + 1) % per_tick == 0 {
                            let due = tick * ((i + 1) / per_tick) as u32;
                            if let Some(wait) = due.checked_sub(start.elapsed()) {
                                std::thread::sleep(wait);
                            }
                        }
                    }
                })
            });
        }
    });
}

fn child(stack: &str, url: &str, mode: &str) {
    let (stack, dispatch) = Stack::new(stack, url);
    // Warm up: registry pages, connections, first batch.
    produce(&dispatch, 2_000, false);
    stack.flush();
    std::thread::sleep(Duration::from_millis(300));
    // Tell the parent warm-up is over, so it counts from here, then wait
    // for its go-ahead.
    println!("ready");
    let _ = std::io::stdout().flush();
    let mut go = String::new();
    let _ = std::io::stdin().read_line(&mut go);

    let paced = mode == "paced";
    let per_thread = if paced {
        PACED_RATE / THREADS * PACED_SECS as usize
    } else {
        BURST_PER_THREAD
    };
    let before = heap_in_use();
    let peak = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(AtomicBool::new(false));
    let monitor = {
        let (peak, done) = (Arc::clone(&peak), Arc::clone(&done));
        std::thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                peak.fetch_max((heap_in_use() - before).max(0) as usize, Ordering::Relaxed);
                std::thread::sleep(Duration::from_micros(500));
            }
        })
    };
    let cpu = cpu_time::ProcessTime::now();
    let start = Instant::now();
    produce(&dispatch, per_thread, paced);
    let producing = start.elapsed();
    stack.flush();
    let total = start.elapsed();
    let cpu = cpu.elapsed();
    done.store(true, Ordering::Relaxed);
    let _ = monitor.join();
    // produced producing_s total_s cpu_s peak_bytes
    println!(
        "{} {} {} {} {}",
        THREADS * per_thread,
        producing.as_secs_f64(),
        total.as_secs_f64(),
        cpu.as_secs_f64(),
        peak.load(Ordering::Relaxed)
    );
}

// ─── Parent: receivers, runs, table ─────────────────────────

struct Run {
    produced: f64,
    producing: f64,
    total: f64,
    cpu: f64,
    peak: f64,
    delivered: f64,
}

fn run_child(stack: &str, receiver: &Receiver, mode: &str, latency: Duration) -> Run {
    let mut child = Command::new(std::env::current_exe().expect("exe"))
        .args([
            "--child",
            stack,
            &format!("{}/v1/traces", receiver.base),
            mode,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("run child");
    let mut lines = BufReader::new(child.stdout.take().expect("stdout")).lines();
    assert_eq!(lines.next().and_then(Result::ok).as_deref(), Some("ready"));
    // Count only what the measured run delivers. Warm-up requests have
    // landed: the child flushed and waited before saying ready.
    let before = receiver.spans.load(Ordering::Relaxed);
    let _ = writeln!(child.stdin.take().expect("stdin"), "go");
    let text = lines.next().and_then(Result::ok).unwrap_or_default();
    let _ = child.wait();
    // Requests still in flight when the child exits are lost to both
    // stacks alike; give the receiver time to count what arrived.
    std::thread::sleep(latency + Duration::from_millis(300));
    let v: Vec<f64> = text
        .split_whitespace()
        .filter_map(|x| x.parse().ok())
        .collect();
    assert_eq!(v.len(), 5, "child output: {text:?}");
    let delivered = (receiver.spans.load(Ordering::Relaxed) - before) as f64;
    Run {
        produced: v[0],
        producing: v[1],
        total: v[2],
        cpu: v[3],
        peak: v[4],
        delivered: delivered.max(0.0),
    }
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

fn label(stack: &str) -> &'static str {
    if stack == "oxtel" {
        "oxtel"
    } else {
        "tracing-opentelemetry + opentelemetry_sdk"
    }
}

fn parent() {
    println!(
        "{THREADS} threads; batch processor defaults (queue 2048, batch 512, 5 s delay); \
         OTLP/HTTP protobuf to a receiver in another process; median of {REPEATS} runs.\n"
    );
    let scenarios = [
        ("Burst, fast backend", "burst", Duration::ZERO),
        (
            "Burst, slow backend (250 ms per request)",
            "burst",
            Duration::from_millis(250),
        ),
        (
            "Paced at 40,000 spans/s for 5 s, fast backend",
            "paced",
            Duration::ZERO,
        ),
    ];
    for (title, mode, latency) in scenarios {
        let receiver = Receiver::start(latency);
        println!("### {title}\n");
        println!(
            "| Stack | Spans produced/s | Spans exported/s | Delivered | CPU per span | Peak heap |"
        );
        println!("|---|---|---|---|---|---|");
        for stack in STACKS {
            let runs: Vec<Run> = (0..REPEATS)
                .map(|_| run_child(stack, &receiver, mode, latency))
                .collect();
            let m = |f: fn(&Run) -> f64| median(runs.iter().map(f).collect());
            println!(
                "| {} | {:.0} | {:.0} | {:.1}% | {:.2} µs | {:.1} MiB |",
                label(stack),
                m(|r| r.produced / r.producing),
                m(|r| r.delivered / r.total),
                m(|r| 100.0 * r.delivered / r.produced),
                m(|r| 1e6 * r.cpu / r.produced),
                m(|r| r.peak / (1024.0 * 1024.0)),
            );
        }
        println!();
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--child") {
        child(&args[2], &args[3], &args[4]);
    } else {
        parent();
    }
}
