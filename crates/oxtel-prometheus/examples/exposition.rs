//! Prints a sample exposition: `cargo run -p oxtel-prometheus --example
//! exposition -- [openmetrics|text]`. CI pipes it into `promtool check
//! metrics` and an OpenMetrics parser.

use oxtel_prometheus::{Format, Options, render};
use oxtel_sdk::Resource;
use oxtel_sdk::metrics::MeterProvider;
use oxtel_sdk::trace::{Sampler, TracerProvider};
use tracing_subscriber::layer::SubscriberExt;

fn main() {
    let format = match std::env::args().nth(1).as_deref() {
        Some("text") => Format::Text,
        _ => Format::OpenMetrics,
    };
    let resource = Resource::builder()
        .without_env()
        .with_service_name("checkout")
        .build();
    let tracer = TracerProvider::builder()
        .with_resource(resource.clone())
        .with_sampler(Sampler::AlwaysOn)
        .build();
    let meters = MeterProvider::builder().build();
    let subscriber = tracing_subscriber::registry().with(tracer.layer());
    tracing::subscriber::with_default(subscriber, || {
        metrics::with_local_recorder(&meters.recorder(), || {
            metrics::describe_histogram!(
                "http.server.request.duration",
                metrics::Unit::Seconds,
                "Duration of \"HTTP\" server requests."
            );
            metrics::describe_counter!("jobs.done", "Jobs finished.\nBy queue.");
            metrics::describe_gauge!(
                "cache.size",
                metrics::Unit::Bytes,
                "Bytes held in the cache."
            );
            metrics::describe_gauge!("queue.depth", "Jobs waiting.");
            for (route, seconds) in [("/items", 0.012), ("/items", 0.2), ("/cart", 1.7)] {
                let span = tracing::info_span!("request");
                let _entered = span.enter();
                metrics::histogram!("http.server.request.duration", "http.route" => route)
                    .record(seconds);
            }
            metrics::counter!("jobs.done", "queue" => "email").increment(5);
            metrics::counter!("jobs.done", "queue" => "say \"hi\"\\now").increment(1);
            metrics::gauge!("cache.size").set(1048576.0);
            metrics::gauge!("queue.depth", "a.b" => "1", "a_b" => "2").set(3.0);
        });
    });
    print!(
        "{}",
        render(&meters.collect(), &resource, format, &Options::default())
    );
}
