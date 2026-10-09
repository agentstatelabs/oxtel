//! Head sampling: whether a new span is recorded and exported.

use crate::context::SpanContext;
use crate::id::TraceId;

/// Decides whether a new span is sampled.
#[derive(Clone, Debug, PartialEq)]
pub enum Sampler {
    /// Sample every span.
    AlwaysOn,
    /// Sample no span. Ids are still generated and propagated.
    AlwaysOff,
    /// Sample this fraction of traces, consistently: the same trace id gets
    /// the same decision in every service using the same ratio. It compares
    /// the trace id's 56 random bits with a threshold, as the
    /// specification's `TraceIdRatioBased` sampler does.
    TraceIdRatio(f64),
    /// Follow the parent's decision. For root spans, use the inner sampler.
    ParentBased(Box<Sampler>),
}

impl Default for Sampler {
    /// `parentbased_always_on`, the specification's default.
    fn default() -> Self {
        Sampler::ParentBased(Box::new(Sampler::AlwaysOn))
    }
}

impl Sampler {
    /// Whether a span in `trace_id`, with `parent` (if any), is sampled.
    pub fn should_sample(&self, parent: Option<&SpanContext>, trace_id: TraceId) -> bool {
        match self {
            Sampler::AlwaysOn => true,
            Sampler::AlwaysOff => false,
            Sampler::TraceIdRatio(ratio) => {
                let ratio = ratio.clamp(0.0, 1.0);
                if ratio >= 1.0 {
                    return true;
                }
                if ratio <= 0.0 {
                    return false;
                }
                // Reject when the randomness falls below (1 - ratio) * 2^56.
                let threshold = ((1.0 - ratio) * (1u64 << 56) as f64) as u64;
                trace_id.randomness() >= threshold
            }
            Sampler::ParentBased(root) => match parent {
                Some(parent) if parent.is_valid() => parent.is_sampled(),
                _ => root.should_sample(None, trace_id),
            },
        }
    }

    /// The sampler `OTEL_TRACES_SAMPLER` and `OTEL_TRACES_SAMPLER_ARG` name,
    /// or the default when they're unset or unrecognised.
    pub fn from_env() -> Self {
        let name = std::env::var("OTEL_TRACES_SAMPLER").unwrap_or_default();
        let arg = std::env::var("OTEL_TRACES_SAMPLER_ARG").ok();
        Self::from_name(&name, arg.as_deref()).unwrap_or_default()
    }

    /// The sampler with the specification's name, such as
    /// `parentbased_traceidratio`, and argument. A missing or invalid ratio
    /// means 1.0.
    pub fn from_name(name: &str, arg: Option<&str>) -> Option<Self> {
        let ratio = || {
            arg.and_then(|a| a.trim().parse::<f64>().ok())
                .filter(|r| (0.0..=1.0).contains(r))
                .unwrap_or(1.0)
        };
        Some(match name.trim() {
            "always_on" => Sampler::AlwaysOn,
            "always_off" => Sampler::AlwaysOff,
            "traceidratio" => Sampler::TraceIdRatio(ratio()),
            "parentbased_always_on" => Sampler::ParentBased(Box::new(Sampler::AlwaysOn)),
            "parentbased_always_off" => Sampler::ParentBased(Box::new(Sampler::AlwaysOff)),
            "parentbased_traceidratio" => {
                Sampler::ParentBased(Box::new(Sampler::TraceIdRatio(ratio())))
            }
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::TraceFlags;
    use crate::id::SpanId;

    #[test]
    fn ratio_is_consistent_and_close_to_the_target() {
        let s = Sampler::TraceIdRatio(0.25);
        let n = 100_000;
        let mut sampled = 0;
        for _ in 0..n {
            let t = TraceId::random();
            let first = s.should_sample(None, t);
            assert_eq!(first, s.should_sample(None, t), "not consistent");
            sampled += first as usize;
        }
        let share = sampled as f64 / n as f64;
        assert!((0.24..0.26).contains(&share), "{share}");
    }

    #[test]
    fn parent_based_follows_the_parent() {
        let s = Sampler::default();
        let parent = |sampled| SpanContext {
            trace_id: TraceId::random(),
            span_id: SpanId::random(),
            flags: TraceFlags::default().with_sampled(sampled),
            ..Default::default()
        };
        assert!(s.should_sample(Some(&parent(true)), TraceId::random()));
        assert!(!s.should_sample(Some(&parent(false)), TraceId::random()));
        assert!(s.should_sample(None, TraceId::random()));
    }

    #[test]
    fn names_from_the_specification() {
        assert_eq!(
            Sampler::from_name("parentbased_traceidratio", Some("0.1")),
            Some(Sampler::ParentBased(Box::new(Sampler::TraceIdRatio(0.1))))
        );
        assert_eq!(
            Sampler::from_name("traceidratio", Some("7")),
            Some(Sampler::TraceIdRatio(1.0))
        );
        assert_eq!(Sampler::from_name("nonsense", None), None);
    }
}
