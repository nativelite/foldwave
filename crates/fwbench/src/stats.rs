//! Summary statistics for latency samples.
//!
//! Pure arithmetic, so the numbers the harness reports are themselves tested.
//! `standards/performance.md` requires medians and variance rather than
//! cherry-picked bests, so [`Summary`] carries the whole distribution shape.

use std::time::Duration;

/// The distribution of a set of latency samples.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    /// Number of samples the summary was computed from.
    pub count: usize,
    /// Fastest sample.
    pub min: Duration,
    /// Median: half the samples were at or below this.
    pub p50: Duration,
    /// 90th percentile.
    pub p90: Duration,
    /// 99th percentile, the tail the section 4 target is set against.
    pub p99: Duration,
    /// Slowest sample.
    pub max: Duration,
    /// Arithmetic mean, reported alongside the median rather than instead of it.
    pub mean: Duration,
}

/// The percentile of a sorted slice, by the nearest-rank method.
///
/// `q` is clamped to `0..=100`. An empty slice gives zero, because a harness
/// that invents a number for no data is worse than one that says zero.
pub fn percentile(sorted: &[Duration], q: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let q = q.clamp(0.0, 100.0);
    // Nearest rank: the smallest value at or above q% of the distribution.
    let rank = (q / 100.0 * sorted.len() as f64).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

impl Summary {
    /// Summarize samples. The input is copied and sorted, so the caller's
    /// ordering is left alone.
    pub fn of(samples: &[Duration]) -> Summary {
        if samples.is_empty() {
            return Summary {
                count: 0,
                min: Duration::ZERO,
                p50: Duration::ZERO,
                p90: Duration::ZERO,
                p99: Duration::ZERO,
                max: Duration::ZERO,
                mean: Duration::ZERO,
            };
        }
        let mut sorted = samples.to_vec();
        sorted.sort_unstable();
        let total: Duration = sorted.iter().sum();
        Summary {
            count: sorted.len(),
            min: sorted[0],
            p50: percentile(&sorted, 50.0),
            p90: percentile(&sorted, 90.0),
            p99: percentile(&sorted, 99.0),
            max: sorted[sorted.len() - 1],
            mean: total / sorted.len() as u32,
        }
    }

    /// One line: `n=200 p50=0.42ms p90=0.81ms p99=2.10ms (min 0.30 max 3.40)`.
    pub fn line(&self) -> String {
        format!(
            "n={} p50={} p90={} p99={} (min {} max {} mean {})",
            self.count,
            ms(self.p50),
            ms(self.p90),
            ms(self.p99),
            ms(self.min),
            ms(self.max),
            ms(self.mean),
        )
    }
}

/// Format a duration in milliseconds, at a resolution that stays honest for
/// sub-millisecond values.
pub fn ms(d: Duration) -> String {
    format!("{:.2}ms", d.as_secs_f64() * 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn micros(values: &[u64]) -> Vec<Duration> {
        values.iter().map(|&v| Duration::from_micros(v)).collect()
    }

    #[test]
    fn percentile_of_nothing_is_zero() {
        assert_eq!(percentile(&[], 50.0), Duration::ZERO);
    }

    #[test]
    fn percentile_of_one_sample_is_that_sample() {
        let s = micros(&[42]);
        assert_eq!(percentile(&s, 0.0), Duration::from_micros(42));
        assert_eq!(percentile(&s, 50.0), Duration::from_micros(42));
        assert_eq!(percentile(&s, 100.0), Duration::from_micros(42));
    }

    #[test]
    fn percentile_uses_nearest_rank() {
        // Ten samples, 1..10: p50 is the 5th, p90 the 9th, p100 the 10th.
        let s = micros(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        assert_eq!(percentile(&s, 50.0), Duration::from_micros(5));
        assert_eq!(percentile(&s, 90.0), Duration::from_micros(9));
        assert_eq!(percentile(&s, 100.0), Duration::from_micros(10));
    }

    #[test]
    fn percentile_clamps_out_of_range_inputs() {
        let s = micros(&[1, 2, 3]);
        assert_eq!(percentile(&s, -10.0), Duration::from_micros(1));
        assert_eq!(percentile(&s, 500.0), Duration::from_micros(3));
    }

    #[test]
    fn summary_of_nothing_is_all_zero_not_a_panic() {
        let s = Summary::of(&[]);
        assert_eq!(s.count, 0);
        assert_eq!(s.p50, Duration::ZERO);
        assert_eq!(s.max, Duration::ZERO);
    }

    #[test]
    fn summary_reports_the_distribution() {
        let s = Summary::of(&micros(&[5, 1, 3, 2, 4]));
        assert_eq!(s.count, 5);
        assert_eq!(s.min, Duration::from_micros(1));
        assert_eq!(s.max, Duration::from_micros(5));
        assert_eq!(s.p50, Duration::from_micros(3));
        assert_eq!(s.mean, Duration::from_micros(3));
    }

    #[test]
    fn summary_does_not_reorder_the_caller_samples() {
        let samples = micros(&[3, 1, 2]);
        let copy = samples.clone();
        let _ = Summary::of(&samples);
        assert_eq!(samples, copy);
    }

    #[test]
    fn summary_handles_every_sample_being_equal() {
        let s = Summary::of(&micros(&[7, 7, 7, 7]));
        assert_eq!(s.min, s.max);
        assert_eq!(s.p99, Duration::from_micros(7));
        assert_eq!(s.mean, Duration::from_micros(7));
    }

    #[test]
    fn ms_keeps_sub_millisecond_values_readable() {
        assert_eq!(ms(Duration::from_micros(400)), "0.40ms");
        assert_eq!(ms(Duration::from_micros(1_500)), "1.50ms");
        assert_eq!(ms(Duration::ZERO), "0.00ms");
    }

    #[test]
    fn summary_line_names_every_figure_it_shows() {
        let line = Summary::of(&micros(&[1_000])).line();
        for part in ["n=1", "p50=", "p90=", "p99=", "min ", "max ", "mean "] {
            assert!(line.contains(part), "missing {part:?} in {line:?}");
        }
    }
}
