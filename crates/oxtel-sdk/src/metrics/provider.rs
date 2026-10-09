//! The meter provider: one metrics store, its recorder and its readers.

use std::sync::Arc;
use std::time::SystemTime;

use crate::metrics::data::MetricFamily;
use crate::metrics::store::{Config, ExemplarFilter, MetricsRecorder, Store};

/// One metrics pipeline. Install its [`recorder`](MeterProvider::recorder)
/// with `metrics::set_global_recorder`; readers call
/// [`collect`](MeterProvider::collect).
#[derive(Clone)]
pub struct MeterProvider {
    store: Arc<Store>,
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
}

/// Builds a [`MeterProvider`].
pub struct MeterProviderBuilder {
    cardinality_limit: usize,
    exemplar_filter: Option<ExemplarFilter>,
    buckets: Vec<(String, Arc<[f64]>)>,
}

impl Default for MeterProviderBuilder {
    fn default() -> Self {
        MeterProviderBuilder {
            cardinality_limit: 2000,
            exemplar_filter: None,
            buckets: Vec::new(),
        }
    }
}

impl MeterProviderBuilder {
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
        MeterProvider {
            store: Arc::new(Store::new(Config {
                cardinality_limit: self.cardinality_limit,
                exemplar_filter: self
                    .exemplar_filter
                    .unwrap_or_else(ExemplarFilter::from_env),
                buckets: self.buckets,
            })),
        }
    }
}
