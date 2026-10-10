//! The meter provider: one metrics store, its recorder and its readers.

use std::sync::Arc;
use std::time::SystemTime;

use crate::export::{ExportError, ProcessorStats};
use crate::metrics::data::MetricFamily;
use crate::metrics::export::MetricExporter;
use crate::metrics::reader::{PeriodicReader, PeriodicReaderConfig};
use crate::metrics::store::{Config, ExemplarFilter, MetricsRecorder, Store};
use crate::resource::Resource;

/// One metrics pipeline. Install its [`recorder`](MeterProvider::recorder)
/// with `metrics::set_global_recorder`. Periodic exporters push its metrics
/// on a timer; pull readers, such as a Prometheus endpoint, call
/// [`collect`](MeterProvider::collect).
///
/// Clones share the store and the exporters. When the last clone is
/// dropped, the exporters export once more and shut down.
#[derive(Clone)]
pub struct MeterProvider {
    store: Arc<Store>,
    resource: Resource,
    readers: Arc<[PeriodicReader]>,
}

impl MeterProvider {
    /// A builder.
    pub fn builder() -> MeterProviderBuilder {
        MeterProviderBuilder::default()
    }

    /// The `metrics` facade recorder that writes into this provider.
    pub fn recorder(&self) -> MetricsRecorder {
        MetricsRecorder {
            store: Arc::clone(&self.store),
        }
    }

    /// A snapshot of every metric, sorted by name.
    pub fn collect(&self) -> Vec<MetricFamily> {
        self.store.collect()
    }

    /// When cumulative values started.
    pub fn start_time(&self) -> SystemTime {
        self.store.start_time()
    }

    /// The resource the periodic exporters describe the metrics with.
    pub fn resource(&self) -> &Resource {
        &self.resource
    }

    /// Have every periodic exporter export now, and wait for them. Returns
    /// the first failure.
    pub fn force_flush(&self) -> Result<(), ExportError> {
        self.readers
            .iter()
            .map(PeriodicReader::force_flush)
            .fold(Ok(()), Result::and)
    }

    /// Export once more and stop the periodic exporters. Recording still
    /// works, and pull readers still collect.
    pub fn shutdown(&self) -> Result<(), ExportError> {
        self.readers
            .iter()
            .map(PeriodicReader::shutdown)
            .fold(Ok(()), Result::and)
    }

    /// What happened to exported data points so far, across the periodic
    /// exporters.
    pub fn stats(&self) -> ProcessorStats {
        self.readers
            .iter()
            .map(PeriodicReader::stats)
            .fold(ProcessorStats::default(), |a, b| ProcessorStats {
                exported: a.exported + b.exported,
                export_failed: a.export_failed + b.export_failed,
                dropped: a.dropped + b.dropped,
            })
    }
}

/// Builds a [`MeterProvider`].
pub struct MeterProviderBuilder {
    resource: Option<Resource>,
    cardinality_limit: usize,
    exemplar_filter: Option<ExemplarFilter>,
    buckets: Vec<(String, Arc<[f64]>)>,
    exporters: Vec<(Box<dyn MetricExporter>, PeriodicReaderConfig)>,
}

impl Default for MeterProviderBuilder {
    fn default() -> Self {
        MeterProviderBuilder {
            resource: None,
            cardinality_limit: 2000,
            exemplar_filter: None,
            buckets: Vec::new(),
            exporters: Vec::new(),
        }
    }
}

impl MeterProviderBuilder {
    /// The resource periodic exporters describe the metrics with. Default:
    /// [`Resource::from_env`].
    pub fn with_resource(mut self, resource: Resource) -> Self {
        self.resource = Some(resource);
        self
    }

    /// Export through `exporter` from a background thread, every interval
    /// set by the environment ([`PeriodicReaderConfig::from_env`]). Each
    /// export carries cumulative values. Call it again to add another
    /// exporter.
    pub fn with_periodic_exporter(self, exporter: impl MetricExporter) -> Self {
        self.with_periodic_exporter_config(exporter, PeriodicReaderConfig::from_env())
    }

    /// Export through `exporter` with `config`.
    pub fn with_periodic_exporter_config(
        mut self,
        exporter: impl MetricExporter,
        config: PeriodicReaderConfig,
    ) -> Self {
        self.exporters.push((Box::new(exporter), config));
        self
    }

    /// The most series one metric keeps, counting the overflow series that
    /// collects measurements beyond it (2000).
    pub fn with_cardinality_limit(mut self, limit: usize) -> Self {
        self.cardinality_limit = limit.max(1);
        self
    }

    /// Which measurements may become exemplars. Default:
    /// [`ExemplarFilter::from_env`].
    pub fn with_exemplar_filter(mut self, filter: ExemplarFilter) -> Self {
        self.exemplar_filter = Some(filter);
        self
    }

    /// Bucket bounds for the histogram `name`. Without this, histograms in
    /// seconds use the semantic conventions' duration buckets, and others
    /// the specification's defaults.
    pub fn with_histogram_buckets(mut self, name: impl Into<String>, bounds: &[f64]) -> Self {
        let mut bounds: Vec<f64> = bounds.iter().copied().filter(|b| !b.is_nan()).collect();
        bounds.sort_by(f64::total_cmp);
        bounds.dedup();
        self.buckets.push((name.into(), bounds.into()));
        self
    }

    /// The provider.
    pub fn build(self) -> MeterProvider {
        let resource = self.resource.unwrap_or_else(Resource::from_env);
        let store = Arc::new(Store::new(Config {
            cardinality_limit: self.cardinality_limit,
            exemplar_filter: self
                .exemplar_filter
                .unwrap_or_else(ExemplarFilter::from_env),
            buckets: self.buckets,
        }));
        let readers = self
            .exporters
            .into_iter()
            .map(|(exporter, config)| {
                PeriodicReader::new(Arc::clone(&store), resource.clone(), exporter, config)
            })
            .collect();
        MeterProvider {
            store,
            resource,
            readers,
        }
    }
}
