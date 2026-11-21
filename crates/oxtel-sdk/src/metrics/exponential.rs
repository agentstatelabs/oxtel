//! The base-2 exponential bucket histogram aggregation, as the metrics SDK
//! specification defines it: buckets whose boundaries are powers of
//! `2^(2^-scale)`, at the finest scale that keeps the recorded range within
//! `max_size` buckets.

use crate::metrics::data::{ExponentialBuckets, ExponentialHistogramPoint};

/// The specification's default bucket count, per sign.
pub const DEFAULT_MAX_SIZE: u32 = 160;
/// The specification's default starting (and finest) scale.
pub const DEFAULT_MAX_SCALE: i8 = 20;
/// The coarsest scale: at -10, every double fits in a few buckets.
const MIN_SCALE: i32 = -10;

/// One series' exponential histogram, as it's recorded.
#[derive(Clone, Debug)]
pub(crate) struct ExpState {
    max_size: usize,
    scale: i32,
    zero_count: u64,
    positive: ExponentialBuckets,
    negative: ExponentialBuckets,
    count: u64,
    sum: f64,
    min: f64,
    max: f64,
}

impl ExpState {
    pub(crate) fn new(max_size: u32, max_scale: i8) -> Self {
        ExpState {
            // Two buckets at least, or no scale would fit some ranges.
            max_size: (max_size as usize).max(2),
            scale: i32::from(max_scale).clamp(MIN_SCALE, 20),
            zero_count: 0,
            positive: ExponentialBuckets::default(),
            negative: ExponentialBuckets::default(),
            count: 0,
            sum: 0.0,
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
        }
    }

    /// Records `value`. Infinities and NaN are ignored, as the
    /// specification says.
    pub(crate) fn record(&mut self, value: f64) {
        if !value.is_finite() {
            return;
        }
        self.count += 1;
        self.sum += value;
        self.min = self.min.min(value);
        self.max = self.max.max(value);
        if value == 0.0 {
            self.zero_count += 1;
            return;
        }
        let magnitude = value.abs();
        let mut index = map_to_index(magnitude, self.scale);
        let buckets = if value > 0.0 {
            &self.positive
        } else {
            &self.negative
        };
        let change = buckets.change_to_fit(index, self.max_size, self.scale);
        if change > 0 {
            self.scale -= change;
            self.positive.downscale(change);
            self.negative.downscale(change);
            index = map_to_index(magnitude, self.scale);
        }
        let buckets = if value > 0.0 {
            &mut self.positive
        } else {
            &mut self.negative
        };
        buckets.increment(index);
    }

    pub(crate) fn snapshot(&self) -> ExponentialHistogramPoint {
        let any = self.count > 0;
        ExponentialHistogramPoint {
            scale: self.scale,
            zero_count: self.zero_count,
            positive: self.positive.clone(),
            negative: self.negative.clone(),
            count: self.count,
            sum: self.sum,
            min: any.then_some(self.min),
            max: any.then_some(self.max),
            exemplars: Vec::new(),
        }
    }
}

impl ExponentialBuckets {
    /// How much coarser the scale must get for `index` to fit with the
    /// buckets already held, within `max_size` of them.
    fn change_to_fit(&self, index: i32, max_size: usize, scale: i32) -> i32 {
        if self.counts.is_empty() {
            return 0;
        }
        let end = self.offset + self.counts.len() as i32 - 1;
        let (mut low, mut high) = (self.offset.min(index), end.max(index));
        let mut change = 0;
        while i64::from(high) - i64::from(low) + 1 > max_size as i64 && scale - change > MIN_SCALE {
            low >>= 1;
            high >>= 1;
            change += 1;
        }
        change
    }

    fn increment(&mut self, index: i32) {
        if self.counts.is_empty() {
            self.offset = index;
            self.counts.push(1);
            return;
        }
        if index < self.offset {
            let grow = (self.offset - index) as usize;
            self.counts.splice(0..0, std::iter::repeat_n(0, grow));
            self.offset = index;
        }
        let at = (index - self.offset) as usize;
        if at >= self.counts.len() {
            self.counts.resize(at + 1, 0);
        }
        self.counts[at] += 1;
    }

    /// The same counts at a scale `change` coarser: each bucket merges into
    /// the one its index, shifted right by `change`, names.
    pub(crate) fn downscale(&mut self, change: i32) {
        if change <= 0 || self.counts.is_empty() {
            return;
        }
        let offset = self.offset >> change;
        let end = (self.offset + self.counts.len() as i32 - 1) >> change;
        let mut counts = vec![0; (end - offset + 1) as usize];
        for (i, &n) in self.counts.iter().enumerate() {
            counts[(((self.offset + i as i32) >> change) - offset) as usize] += n;
        }
        self.offset = offset;
        self.counts = counts;
    }

    /// The count of the bucket at `index`.
    fn get(&self, index: i32) -> u64 {
        usize::try_from(index - self.offset)
            .ok()
            .and_then(|i| self.counts.get(i))
            .copied()
            .unwrap_or(0)
    }

    /// These counts less `earlier`'s, which are at the same scale and
    /// within the same range, with empty buckets trimmed from both ends.
    fn minus(&self, earlier: &ExponentialBuckets) -> ExponentialBuckets {
        let counts: Vec<u64> = self
            .counts
            .iter()
            .enumerate()
            .map(|(i, &n)| n.saturating_sub(earlier.get(self.offset + i as i32)))
            .collect();
        let Some(first) = counts.iter().position(|&n| n > 0) else {
            return ExponentialBuckets::default();
        };
        let last = counts.iter().rposition(|&n| n > 0).unwrap_or(first);
        ExponentialBuckets {
            offset: self.offset + first as i32,
            counts: counts[first..=last].to_vec(),
        }
    }
}

/// The change between two snapshots of one cumulative histogram: `later`
/// less `earlier`. The scale only ever gets coarser, so `earlier` is
/// brought to `later`'s scale first; that's exact. Min, max and exemplars
/// are `later`'s.
pub(crate) fn delta(
    later: &ExponentialHistogramPoint,
    earlier: &ExponentialHistogramPoint,
) -> ExponentialHistogramPoint {
    let change = earlier.scale - later.scale;
    let (mut positive, mut negative) = (earlier.positive.clone(), earlier.negative.clone());
    positive.downscale(change);
    negative.downscale(change);
    ExponentialHistogramPoint {
        scale: later.scale,
        zero_count: later.zero_count.saturating_sub(earlier.zero_count),
        positive: later.positive.minus(&positive),
        negative: later.negative.minus(&negative),
        count: later.count.saturating_sub(earlier.count),
        sum: later.sum - earlier.sum,
        min: later.min,
        max: later.max,
        exemplars: later.exemplars.clone(),
    }
}

/// The index of the bucket `value` (positive and finite) falls in at
/// `scale`. Buckets include their upper boundary: at scale 0, bucket 0 is
/// (1, 2]. Subnormal values count as the smallest normal one.
pub(crate) fn map_to_index(value: f64, scale: i32) -> i32 {
    let value = value.max(f64::MIN_POSITIVE);
    let bits = value.to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as i32 - 1023;
    let power_of_two = bits & ((1 << 52) - 1) == 0;
    if scale <= 0 {
        // The exponent alone, less one for exact powers of two, which close
        // the bucket below them.
        let exponent = if power_of_two { exponent - 1 } else { exponent };
        return exponent >> -scale;
    }
    if power_of_two {
        return (exponent << scale) - 1;
    }
    let scale_factor = std::f64::consts::LOG2_E * f64::from(1u32 << scale);
    (value.ln() * scale_factor).ceil() as i32 - 1
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bucket's lower and upper boundaries at `scale`.
    fn bounds(index: i32, scale: i32) -> (f64, f64) {
        let base = 2f64.powf(2f64.powi(-scale));
        (base.powi(index), base.powi(index + 1))
    }

    #[test]
    fn values_land_in_the_bucket_whose_boundaries_hold_them() {
        // Upper boundaries are inclusive.
        assert_eq!(map_to_index(1.0, 0), -1);
        assert_eq!(map_to_index(2.0, 0), 0);
        assert_eq!(map_to_index(3.0, 0), 1);
        assert_eq!(map_to_index(4.0, 0), 1);
        assert_eq!(map_to_index(4.0, -1), 0);
        assert_eq!(map_to_index(5.0, -1), 1);
        assert_eq!(map_to_index(0.5, 0), -2);
        assert_eq!(map_to_index(2.0, 3), 7);
        for scale in [-4, -1, 0, 1, 3, 8, 20] {
            for value in [1.5, 3.7, 10.0, 0.001, 123_456.789, 7.0e-300, 1.0e300] {
                let index = map_to_index(value, scale);
                let (low, high) = bounds(index, scale);
                let slack = 1e-9 * high;
                assert!(
                    low - slack <= value && value <= high + slack,
                    "{value} at scale {scale}: bucket {index} is ({low}, {high}]"
                );
            }
        }
    }

    #[test]
    fn the_scale_drops_until_the_range_fits() {
        let mut h = ExpState::new(4, 20);
        h.record(1.5);
        assert_eq!(h.scale, 20, "one value fits at the finest scale");
        h.record(100.0);
        assert!(h.scale < 20);
        let p = h.snapshot();
        assert!(p.positive.counts.len() <= 4, "{p:?}");
        assert_eq!(p.positive.counts.iter().sum::<u64>(), 2);
        for value in [1.5, 100.0] {
            let index = map_to_index(value, p.scale);
            assert!(
                p.positive.get(index) > 0,
                "{value} isn't in bucket {index}: {p:?}"
            );
        }
    }

    #[test]
    fn zeros_and_negatives_and_non_finite_values() {
        let mut h = ExpState::new(160, 20);
        for v in [0.0, -0.0, -3.0, 3.0, f64::NAN, f64::INFINITY] {
            h.record(v);
        }
        let p = h.snapshot();
        assert_eq!(p.zero_count, 2);
        assert_eq!(p.count, 4);
        assert_eq!(p.sum, 0.0);
        assert_eq!((p.min, p.max), (Some(-3.0), Some(3.0)));
        assert_eq!(p.negative.counts.iter().sum::<u64>(), 1);
        assert_eq!(p.negative.offset, p.positive.offset, "a mirror image");
    }

    #[test]
    fn deltas_survive_a_change_of_scale() {
        let mut h = ExpState::new(8, 20);
        for v in [1.0, 1.1, 1.2] {
            h.record(v);
        }
        let earlier = h.snapshot();
        for v in [1.1, 50.0, 2000.0] {
            h.record(v);
        }
        let later = h.snapshot();
        assert!(later.scale < earlier.scale, "the range grew");
        let d = delta(&later, &earlier);
        assert_eq!(d.count, 3);
        assert_eq!(d.positive.counts.iter().sum::<u64>(), 3);
        assert!((d.sum - 2051.1).abs() < 1e-9);
        for v in [1.1, 50.0, 2000.0] {
            assert!(d.positive.get(map_to_index(v, d.scale)) > 0, "{v}: {d:?}");
        }
        let none = delta(&later, &later);
        assert_eq!(none.count, 0);
        assert!(none.positive.counts.is_empty());
    }
}
