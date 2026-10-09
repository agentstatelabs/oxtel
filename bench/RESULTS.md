# Benchmarks: oxtel against the official OpenTelemetry Rust stack

_Phase 0, measured 2026-10-09._

## What's compared

- **Traces:** oxtel's `tracing` layer against `tracing-opentelemetry` 0.34 over `opentelemetry_sdk` 0.33. "`tracing` alone" is the registry with no layer: the floor.
- **Metrics:** oxtel's `metrics` recorder against `opentelemetry_sdk` 0.33 and `metrics-exporter-prometheus` 0.18, the most used `metrics` backend.

**Environment:** Apple M4 Pro (10 performance and 4 efficiency cores), 24 GiB RAM, macOS 26.6.2, rustc 1.94.1, release builds. It's a laptop with other work running, so absolute numbers vary; the comparisons hold across repeated runs except where noted.

## Making it fair

- **Same work.** `tracing-opentelemetry` is told not to record what oxtel doesn't: source locations, thread ids, busy/idle timings. Both record span names, fields, events and parent links.
- **Instrumented-thread cost only, in the micro-benchmarks.** Both trace pipelines use a processor that drops finished spans, so they measure recording, building and handing over a span, not exporting.
- **The official SDK's metrics really aggregate.** A reader is attached; without one its instruments do nothing.
- **Cached handles are compared with cached handles.** The official SDK's bound instruments are behind its `experimental_metrics_bound_instruments` feature, which is enabled here.
- **End to end, each run is its own process.** The first `tracing` workload in a process allocates and frees tens of MiB of registry memory. Running both stacks in one process charged that to whichever ran first, so each run is isolated and warmed up. The receiver runs in the parent, so its decoding isn't charged to either stack.

## Traces: cost per operation on the instrumented thread

Median from criterion, with its 95% confidence interval. "vs oxtel" above 1 means slower than oxtel.

| Workload | `tracing` alone | oxtel | tracing-opentelemetry | vs oxtel |
|---|---|---|---|---|
| Span with 3 fields | 57.5 ns | **258 ns** | 671 ns | 2.60× |
| Same, not sampled | 54.9 ns | **83.7 ns** | 589 ns | 7.04× |
| Parent and 3 children | 167 ns | **852 ns** | 2.23 µs | 2.62× |
| Span with 5 events | 56.0 ns | **1.12 µs** | 1.67 µs | 1.49× |
| Current span context | | **29.5 ns** | 45.4 ns | 1.54× |

- **Unsampled spans are nearly free with oxtel**: about 29 ns over `tracing` alone. oxtel decides to sample when a span starts and records nothing for unsampled spans. `tracing-opentelemetry` builds the span either way.
- **"Current span context"** is what propagation and exemplars read on every use.

## Metrics: cost per measurement

| Workload | oxtel | opentelemetry_sdk | metrics-exporter-prometheus |
|---|---|---|---|
| Counter, attributes on each call | **13.0 ns** | 22.9 ns (1.76×) | 36.2 ns (2.78×) |
| Histogram, attributes on each call | **19.6 ns** | 34.6 ns (1.76×) | 43.4 ns (2.21×) |
| Counter, cached handle | **1.9 ns** | 2.0 ns (1.05×) | 2.0 ns (1.02×) |
| Histogram, cached handle | **5.1 ns** | 8.9 ns (1.76×) | 9.1 ns (1.80×) |
| Counter, 8 threads on one series | 28.5–32.7 ns | 20.4–51.1 ns | 61.6–64.0 ns |

- **"Attributes on each call"** is the common pattern: `counter!("requests", "route" => r).increment(1)`, or `counter.add(1, &attrs)`. It includes looking the series up.
- **8 threads on one series** varies between runs, so ranges over four runs are shown. oxtel is steady at about 30 ns. The official SDK flips between about 20 ns and 51 ns. Striping hot counters across per-thread slots would cut oxtel's figure further, at some memory per series; it's a follow-up, not done here.

**Exemplars** (oxtel only, since the official Rust SDK doesn't record them): a histogram recorded inside a sampled span costs 6.1 ns with exemplars off and 54.9 ns with them on. The difference is reading the span context and timestamping the exemplar. It applies only to measurements inside sampled spans, under the default `trace_based` filter.

## End to end: OTLP/HTTP to a receiver

Four threads, each stack's batch processor at its defaults (queue 2048, batch 512), OTLP/HTTP protobuf to a receiver in another process, median of 3 runs. CPU per span is the child process's user and system time divided by spans produced. Peak heap is the child's live heap above its warmed-up level.

**Burst, fast backend** (as fast as the threads can go):

| Stack | Spans produced/s | Spans exported/s | Delivered | CPU per span | Peak heap |
|---|---|---|---|---|---|
| oxtel | 889,447 | **157,734** | 17.5% | **5.48 µs** | 1.4 MiB |
| tracing-opentelemetry + opentelemetry_sdk | 354,261 | 82,976 | 24.7% | 13.85 µs | 1.1 MiB |

**Burst, slow backend** (250 ms per request):

| Stack | Spans produced/s | Spans exported/s | Delivered | CPU per span | Peak heap |
|---|---|---|---|---|---|
| oxtel | 1,165,765 | 2,000 | 0.4% | **3.41 µs** | 1.4 MiB |
| tracing-opentelemetry + opentelemetry_sdk | 470,633 | 1,980 | 0.7% | 8.51 µs | 1.0 MiB |

**Paced at 40,000 spans/s for 5 s, fast backend:**

| Stack | Spans produced/s | Spans exported/s | Delivered | CPU per span | Peak heap |
|---|---|---|---|---|---|
| oxtel | 39,981 | 39,970 | 100.0% | **5.29 µs** | 0.6 MiB |
| tracing-opentelemetry + opentelemetry_sdk | 39,981 | 39,980 | 100.0% | 7.24 µs | 0.7 MiB |

Reading these:
- **In a burst, producers outrun any single export thread, so both stacks drop most spans.** Neither blocks the program, and both stay within about 1.5 MiB. oxtel exports **1.9×** as many spans per second and spends **2.5×** less CPU per span.
  - Its "delivered" share is lower only because it produces 2.5× as fast into a queue of the same size; spans exported per second is the measure that matters.
- **With a slow backend**, both export what the backend allows (512 spans per 250 ms) and memory stays bounded.
- **At a rate both sustain**, both deliver everything, and oxtel uses **27% less CPU** per span.

## Gate

Phase 0's gate was "oxtel equal or better on overhead". It passes.

- **Traces:** oxtel is better in every case, by 1.5 to 7 times.
- **Metrics:** better or equal everywhere except one contended series, where the official SDK is sometimes faster.
- **End to end:** more throughput, less CPU, bounded memory.

## Follow-ups

- **Striped counters** for hot series under heavy contention.
- **A cheaper exemplar timestamp**, for example a coarse clock.
- **Run these in CI on fixed hardware** to catch regressions. Shared CI runners are too noisy for absolute numbers.

## Reproducing

```bash
cargo bench -p oxtel-bench -- --warm-up-time 2 --measurement-time 4 --noplot
python3 bench/summarize.py target/criterion
cargo run --release -p oxtel-bench --bin pipeline
```
