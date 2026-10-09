//! A counter that stays one atomic until threads contend for it, then
//! spreads across per-thread slots.
//!
//! Uncontended, an add is one compare-and-swap on a single cache line and
//! costs no extra memory. When a compare-and-swap fails, another thread is
//! updating the same counter at that moment, so the counter grows a set of
//! slots, each on its own cache line, and each thread adds to the slot its
//! index picks. Reading sums the base and the slots. (Java's `LongAdder`
//! works the same way.)

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Keeps a value on its own cache line, so threads updating it don't also
/// contend for whatever would otherwise share the line.
#[repr(align(128))]
pub(crate) struct CacheLine<T>(pub(crate) T);

/// How many slots a contended counter spreads over: the machine's
/// parallelism, rounded up to a power of two, at most 64.
fn slot_count() -> usize {
    static COUNT: OnceLock<usize> = OnceLock::new();
    *COUNT.get_or_init(|| {
        std::thread::available_parallelism()
            .map_or(8, |n| n.get())
            .next_power_of_two()
            .clamp(2, 64)
    })
}

/// This thread's slot index, assigned on first use.
fn thread_index() -> usize {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    thread_local! {
        static INDEX: usize = NEXT.fetch_add(1, Ordering::Relaxed);
    }
    INDEX.with(|i| *i)
}

/// A monotonic `u64` sum that scales under contention.
pub(crate) struct Adder {
    base: CacheLine<AtomicU64>,
    slots: OnceLock<Box<[CacheLine<AtomicU64>]>>,
}

impl Adder {
    pub(crate) fn new() -> Self {
        Adder {
            base: CacheLine(AtomicU64::new(0)),
            slots: OnceLock::new(),
        }
    }

    #[inline]
    pub(crate) fn add(&self, value: u64) {
        if let Some(slots) = self.slots.get() {
            slots[thread_index() & (slots.len() - 1)]
                .0
                .fetch_add(value, Ordering::Relaxed);
            return;
        }
        let current = self.base.0.load(Ordering::Relaxed);
        if self
            .base
            .0
            .compare_exchange(
                current,
                current.wrapping_add(value),
                Ordering::Relaxed,
                Ordering::Relaxed,
            )
            .is_ok()
        {
            return;
        }
        // Another thread got there first: spread out from now on.
        let slots = self.slots.get_or_init(|| {
            (0..slot_count())
                .map(|_| CacheLine(AtomicU64::new(0)))
                .collect()
        });
        slots[thread_index() & (slots.len() - 1)]
            .0
            .fetch_add(value, Ordering::Relaxed);
    }

    /// The sum. Each part only grows, so successive reads never decrease.
    pub(crate) fn sum(&self) -> u64 {
        let slots = self.slots.get().map_or(0, |s| {
            s.iter().map(|c| c.0.load(Ordering::Relaxed)).sum::<u64>()
        });
        self.base.0.load(Ordering::Relaxed).wrapping_add(slots)
    }

    /// Raise the sum to at least `value`, for counters set from an outside
    /// total. Not atomic against concurrent adds, which is acceptable: such
    /// counters are set from one place.
    pub(crate) fn raise_to(&self, value: u64) {
        let current = self.sum();
        if value > current {
            self.base.0.fetch_add(value - current, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_from_many_threads_all_count() {
        let adder = Adder::new();
        std::thread::scope(|s| {
            for _ in 0..16 {
                s.spawn(|| {
                    for _ in 0..100_000 {
                        adder.add(1);
                    }
                });
            }
        });
        assert_eq!(adder.sum(), 1_600_000);
    }

    #[test]
    fn stays_one_atomic_until_contended() {
        let adder = Adder::new();
        for _ in 0..1000 {
            adder.add(2);
        }
        assert!(adder.slots.get().is_none(), "one thread never contends");
        assert_eq!(adder.sum(), 2000);
    }

    #[test]
    fn raise_to_only_raises() {
        let adder = Adder::new();
        adder.add(10);
        adder.raise_to(5);
        assert_eq!(adder.sum(), 10);
        adder.raise_to(42);
        assert_eq!(adder.sum(), 42);
    }
}
