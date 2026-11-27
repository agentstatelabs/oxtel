//! Views: changing how an instrument's metric comes out, without changing
//! the code that records it.

use crate::metrics::data::MetricKind;

/// A view: which instruments it selects, and what it changes about the
/// metric they make. A field left `None` selects everything, or changes
/// nothing. The first view that selects an instrument applies to it.
///
/// ```
/// use oxtel_sdk::metrics::{MeterProvider, View, ViewAggregation};
///
/// let provider = MeterProvider::builder()
///     // No `cache.lookups` at all.
///     .with_view(View::named("cache.lookups").with_aggregation(ViewAggregation::Drop))
///     // Fewer labels on every `http.*` metric.
///     .with_view(View::named("http.*").with_attributes_excluded(["url.path"]))
///     .build();
/// ```
#[derive(Clone, Debug, Default, PartialEq)]
pub struct View {
    /// Selects instruments with this name. `*` matches any run of
    /// characters and `?` any one.
    pub instrument_name: Option<String>,
    /// Selects instruments of this kind.
    pub instrument_kind: Option<MetricKind>,
    /// Selects instruments with this unit, in UCUM, as described.
    pub unit: Option<String>,
    /// Selects instruments of this scope: the crate that recorded them.
    pub meter_name: Option<String>,
    /// The metric's name instead of the instrument's.
    pub name: Option<String>,
    /// The metric's description instead of the instrument's.
    pub description: Option<String>,
    /// How measurements are aggregated, instead of the default.
    pub aggregation: Option<ViewAggregation>,
    /// Which attributes the metric keeps.
    pub attribute_keys: Option<AttributeKeys>,
    /// How many series the metric may have, instead of the provider's limit.
    pub cardinality_limit: Option<usize>,
}

/// How a view aggregates an instrument's measurements.
#[derive(Clone, Debug, PartialEq)]
pub enum ViewAggregation {
    /// The instrument's default.
    Default,
    /// Nothing: the metric isn't recorded or exported.
    Drop,
    /// A histogram in these explicit buckets.
    ExplicitBuckets {
        /// The bucket boundaries.
        boundaries: Vec<f64>,
        /// Keep the minimum and maximum.
        record_min_max: bool,
    },
    /// A histogram in exponential buckets, for every reader.
    Exponential {
        /// The most buckets for each sign.
        max_size: u32,
        /// The finest scale.
        max_scale: i8,
        /// Keep the minimum and maximum.
        record_min_max: bool,
    },
}

/// Which attributes a view keeps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttributeKeys {
    /// Only these.
    Included(Vec<String>),
    /// All but these.
    Excluded(Vec<String>),
}

impl View {
    /// A view of the instruments named `name`, which may have `*` and `?`
    /// wildcards.
    pub fn named(name: impl Into<String>) -> View {
        View {
            instrument_name: Some(name.into()),
            ..View::default()
        }
    }

    /// Aggregate this way.
    pub fn with_aggregation(mut self, aggregation: ViewAggregation) -> Self {
        self.aggregation = Some(aggregation);
        self
    }

    /// Keep only these attributes.
    pub fn with_attributes_included<S: Into<String>>(
        mut self,
        keys: impl IntoIterator<Item = S>,
    ) -> Self {
        self.attribute_keys = Some(AttributeKeys::Included(
            keys.into_iter().map(Into::into).collect(),
        ));
        self
    }

    /// Keep all attributes but these.
    pub fn with_attributes_excluded<S: Into<String>>(
        mut self,
        keys: impl IntoIterator<Item = S>,
    ) -> Self {
        self.attribute_keys = Some(AttributeKeys::Excluded(
            keys.into_iter().map(Into::into).collect(),
        ));
        self
    }

    /// Rename the metric.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Whether this view selects the instrument.
    pub(crate) fn selects(
        &self,
        name: &str,
        kind: MetricKind,
        unit: Option<&str>,
        meter: &str,
    ) -> bool {
        self.instrument_name
            .as_deref()
            .is_none_or(|pattern| wildcard(pattern, name))
            && self.instrument_kind.is_none_or(|k| k == kind)
            && self.unit.as_deref().is_none_or(|u| Some(u) == unit)
            && self.meter_name.as_deref().is_none_or(|m| m == meter)
    }
}

impl AttributeKeys {
    /// Whether a series keeps the attribute `key`.
    pub(crate) fn keeps(&self, key: &str) -> bool {
        match self {
            AttributeKeys::Included(keys) => keys.iter().any(|k| k == key),
            AttributeKeys::Excluded(keys) => !keys.iter().any(|k| k == key),
        }
    }
}

/// Whether `text` matches `pattern`, where `*` matches any run of
/// characters and `?` any one.
fn wildcard(pattern: &str, text: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards() {
        assert!(wildcard("http.*", "http.server.request.duration"));
        assert!(wildcard("*", ""));
        assert!(wildcard("db.?lient.*", "db.client.connection.count"));
        assert!(wildcard("*.duration", "rpc.server.duration"));
        assert!(!wildcard("http.*", "rpc.server.duration"));
        assert!(!wildcard("jobs", "jobs.done"));
        assert!(wildcard("a*b*c", "axxbyyc"));
        assert!(!wildcard("a*b*c", "axxbyy"));
    }

    #[test]
    fn selectors_must_all_match() {
        let view = View {
            instrument_name: Some("http.*".into()),
            instrument_kind: Some(MetricKind::Histogram),
            unit: Some("s".into()),
            meter_name: Some("oxtel_tower".into()),
            ..View::default()
        };
        let selects = |name, kind, unit, meter| view.selects(name, kind, unit, meter);
        assert!(selects(
            "http.server.request.duration",
            MetricKind::Histogram,
            Some("s"),
            "oxtel_tower"
        ));
        assert!(!selects(
            "http.server.request.duration",
            MetricKind::Counter,
            Some("s"),
            "oxtel_tower"
        ));
        assert!(!selects(
            "http.server.request.duration",
            MetricKind::Histogram,
            None,
            "oxtel_tower"
        ));
        assert!(!selects(
            "http.server.request.duration",
            MetricKind::Histogram,
            Some("s"),
            "app"
        ));
        assert!(View::default().selects("anything", MetricKind::Gauge, None, "app"));
    }
}
