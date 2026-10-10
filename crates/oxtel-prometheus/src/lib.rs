//! Prometheus and OpenMetrics exposition for
//! [oxtel](https://github.com/agentstatelabs/oxtel).
//!
//! [`PrometheusExporter::render`] turns a [`MeterProvider`]'s metrics into
//! the text a Prometheus scrape expects, choosing the format from the
//! scrape's `Accept` header:
//!
//! - **OpenMetrics 1.0**, which Prometheus asks for, carries exemplars
//!   (each histogram bucket and counter links to a trace), units and
//!   `# EOF`;
//! - **the classic text format** (0.0.4) for everything else.
//!
//! A program with an HTTP server can serve [`PrometheusExporter::render`]
//! itself; one without can run a [`PrometheusServer`].
//!
//! Names follow OpenTelemetry's Prometheus compatibility rules: dots become
//! underscores, the unit becomes a suffix (`_seconds`, `_bytes`), counters
//! end in `_total`, and the resource becomes `target_info`.
//!
//! ```
//! use oxtel_prometheus::PrometheusExporter;
//! use oxtel_sdk::metrics::MeterProvider;
//! use oxtel_sdk::Resource;
//!
//! let meters = MeterProvider::builder().build();
//! metrics::with_local_recorder(&meters.recorder(), || {
//!     metrics::counter!("jobs.done").increment(1);
//! });
//! let exporter = PrometheusExporter::new(meters, Resource::builder().without_env().build());
//! let (content_type, body) = exporter.render(Some("application/openmetrics-text"));
//! assert!(content_type.starts_with("application/openmetrics-text"));
//! assert!(body.contains("jobs_done_total"));
//! ```

mod names;
mod render;
mod server;

pub use names::{family_name, label_name, sample_name, unit_suffix};
pub use render::{Format, Options, render};
pub use server::PrometheusServer;

use oxtel_sdk::Resource;
use oxtel_sdk::metrics::MeterProvider;

/// Renders a provider's metrics for Prometheus scrapes.
#[derive(Clone)]
pub struct PrometheusExporter {
    meters: MeterProvider,
    resource: Resource,
    options: Options,
}

impl PrometheusExporter {
    /// An exporter for `meters`, describing the target with `resource`.
    pub fn new(meters: MeterProvider, resource: Resource) -> Self {
        PrometheusExporter {
            meters,
            resource,
            options: Options::default(),
        }
    }

    /// Change what's added to the exposition.
    pub fn with_options(mut self, options: Options) -> Self {
        self.options = options;
        self
    }

    /// The content type and body for a scrape whose `Accept` header is
    /// `accept`.
    pub fn render(&self, accept: Option<&str>) -> (&'static str, String) {
        let format = Format::negotiate(accept);
        (
            format.content_type(),
            render(
                &self.meters.collect(),
                &self.resource,
                format,
                &self.options,
            ),
        )
    }
}
