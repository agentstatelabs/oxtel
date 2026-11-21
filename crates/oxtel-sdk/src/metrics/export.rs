//! Exporters: where collected metrics go.

use std::sync::{Arc, Mutex};

use crate::export::ExportError;
use crate::metrics::data::{
    HistogramAggregation, MetricFamily, MetricKind, Temporality, TemporalityPreference,
};
use crate::resource::Resource;

/// Sends collected metrics somewhere. Called from a reader's thread, never
/// from the instrumented code's.
///
/// Values are cumulative: each export carries every series' total since the
/// provider started.
pub trait MetricExporter: Send + Sync + 'static {
    /// Export one collection.
    fn export(&self, metrics: &[MetricFamily], resource: &Resource) -> Result<(), ExportError>;

    /// Where this exporter sends data, in a few words for people, such as
    /// `http://collector:4318/v1/traces (OTLP)`. oxtel's warnings about
    /// failed exports name it. Never include credentials.
    fn destination(&self) -> String {
        "a custom exporter".into()
    }

    /// Release resources. No exports follow.
    fn shutdown(&self) -> Result<(), ExportError> {
        Ok(())
    }

    /// The temporality it wants for metrics of `kind`: cumulative unless it
    /// says otherwise. The periodic reader turns cumulative values into
    /// deltas for it.
    fn temporality(&self, kind: MetricKind) -> Temporality {
        let _ = kind;
        Temporality::Cumulative
    }

    /// How it wants histograms aggregated: in explicit buckets unless it
    /// says otherwise.
    fn histogram_aggregation(&self) -> HistogramAggregation {
        HistogramAggregation::Explicit
    }
}

/// Keeps each export in memory, for tests.
#[derive(Clone, Default)]
pub struct InMemoryMetricExporter {
    exports: Arc<Mutex<Vec<Vec<MetricFamily>>>>,
    resource: Arc<Mutex<Option<Resource>>>,
    temporality: TemporalityPreference,
    histograms: HistogramAggregation,
}

impl InMemoryMetricExporter {
    /// Ask for this temporality, as an OTLP exporter can. Default:
    /// cumulative.
    pub fn with_temporality_preference(mut self, preference: TemporalityPreference) -> Self {
        self.temporality = preference;
        self
    }

    /// Ask for histograms aggregated this way, as an OTLP exporter can.
    /// Default: explicit buckets.
    pub fn with_histogram_aggregation(mut self, aggregation: HistogramAggregation) -> Self {
        self.histograms = aggregation;
        self
    }

    /// Every export so far, oldest first.
    pub fn exports(&self) -> Vec<Vec<MetricFamily>> {
        self.exports
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The most recent export, if any.
    pub fn last(&self) -> Option<Vec<MetricFamily>> {
        self.exports
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .last()
            .cloned()
    }

    /// The resource of the last export.
    pub fn resource(&self) -> Option<Resource> {
        self.resource
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Forget the exports so far.
    pub fn reset(&self) {
        self.exports
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
}

impl MetricExporter for InMemoryMetricExporter {
    fn destination(&self) -> String {
        "memory".into()
    }

    fn temporality(&self, kind: MetricKind) -> Temporality {
        self.temporality.temporality(kind)
    }

    fn histogram_aggregation(&self) -> HistogramAggregation {
        self.histograms
    }

    fn export(&self, metrics: &[MetricFamily], resource: &Resource) -> Result<(), ExportError> {
        self.exports
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(metrics.to_vec());
        *self.resource.lock().unwrap_or_else(|e| e.into_inner()) = Some(resource.clone());
        Ok(())
    }
}
