//! A real runtime, kept busy, sampled into an in-memory meter provider.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use oxtel_sdk::metrics::{MeterProvider, MetricFamily, PointValue};

fn gauge(families: &[MetricFamily], name: &str) -> f64 {
    let family = families
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("no {name}"));
    match family.points[0].value {
        PointValue::Gauge(v) => v,
        ref other => panic!("{name} isn't a gauge: {other:?}"),
    }
}

#[test]
fn a_busy_runtime_shows_its_workers_tasks_queue_and_load() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .build()
        .unwrap();
    let meters = MeterProvider::builder().build();
    let mut sampler = oxtel_tokio::Sampler::new(runtime.handle()).with_name("main");

    // Hold both workers for a while, so the runtime is busy and what's
    // spawned next has to wait in the global queue.
    let (release, held) = mpsc::channel::<()>();
    let held = std::sync::Arc::new(std::sync::Mutex::new(held));
    let (started, workers_busy) = mpsc::channel();
    for _ in 0..2 {
        let held = std::sync::Arc::clone(&held);
        let started = started.clone();
        runtime.spawn(async move {
            started.send(()).unwrap();
            let until = Instant::now() + Duration::from_millis(300);
            while Instant::now() < until {
                std::hint::spin_loop();
            }
            let _ = held.lock().unwrap().recv_timeout(Duration::from_millis(1));
        });
    }
    for _ in 0..2 {
        workers_busy.recv().unwrap();
    }
    for _ in 0..5 {
        runtime.spawn(async {});
    }
    metrics::with_local_recorder(&meters.recorder(), || sampler.sample());
    let families = meters.collect();
    assert_eq!(gauge(&families, "oxtel.tokio.worker.count"), 2.0);
    assert!(gauge(&families, "oxtel.tokio.task.count") >= 7.0);
    assert!(
        gauge(&families, "oxtel.tokio.global_queue.length") >= 5.0,
        "tasks spawned from outside wait in the global queue"
    );
    for family in &families {
        assert!(
            family.points[0]
                .attributes
                .contains(&("oxtel.tokio.runtime.name".into(), "main".into())),
            "{} is labelled with the runtime's name",
            family.name
        );
    }

    // Once the busy tasks have run their course, the window since the first
    // sample shows the workers mostly busy.
    drop(release);
    std::thread::sleep(Duration::from_millis(350));
    metrics::with_local_recorder(&meters.recorder(), || sampler.sample());
    let utilization = gauge(&meters.collect(), "oxtel.tokio.utilization");
    assert!(
        (0.3..=1.0).contains(&utilization),
        "utilization {utilization}"
    );
}

#[test]
fn an_idle_runtime_is_barely_used() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let meters = MeterProvider::builder().build();
    let mut sampler = oxtel_tokio::Sampler::new(runtime.handle());
    std::thread::sleep(Duration::from_millis(50));
    metrics::with_local_recorder(&meters.recorder(), || sampler.sample());
    let families = meters.collect();
    assert_eq!(gauge(&families, "oxtel.tokio.worker.count"), 1.0);
    assert_eq!(gauge(&families, "oxtel.tokio.task.count"), 0.0);
    assert!(gauge(&families, "oxtel.tokio.utilization") < 0.1);
    assert!(
        families[0].points[0].attributes.is_empty(),
        "no name, no label"
    );
}

#[cfg(tokio_unstable)]
#[test]
fn the_blocking_pool_is_recorded_with_tokio_unstable() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .max_blocking_threads(1)
        .build()
        .unwrap();
    let meters = MeterProvider::builder().build();
    let mut sampler = oxtel_tokio::Sampler::new(runtime.handle());
    let (release, held) = mpsc::channel::<()>();
    let (started, running) = mpsc::channel();
    runtime.spawn_blocking(move || {
        started.send(()).unwrap();
        let _ = held.recv();
    });
    running.recv().unwrap();
    runtime.spawn_blocking(|| {});
    metrics::with_local_recorder(&meters.recorder(), || sampler.sample());
    let families = meters.collect();
    let threads = families
        .iter()
        .find(|f| f.name == "oxtel.tokio.blocking.thread.count")
        .unwrap();
    let busy = threads
        .points
        .iter()
        .find(|p| {
            p.attributes
                .contains(&("oxtel.tokio.thread.state".into(), "busy".into()))
        })
        .unwrap();
    assert_eq!(busy.value, PointValue::Gauge(1.0));
    assert_eq!(gauge(&families, "oxtel.tokio.blocking.queue.length"), 1.0);
    drop(release);
}

#[test]
fn dropping_the_recording_stops_it_at_once() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let recording =
        oxtel_tokio::Sampler::new(runtime.handle()).spawn_every(Duration::from_secs(3600));
    let started = Instant::now();
    drop(recording);
    assert!(started.elapsed() < Duration::from_secs(1));
}
