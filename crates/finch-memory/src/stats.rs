// Streaming statistics primitives: constant-memory, single-pass estimators
// over a stream of observations, with no stored history.
//
// `WelfordStats` (below) backs the pre-indexing degeneracy gate's two
// running baselines (`degenerate_gate.rs`, #1323: compression ratio and
// order-0 Shannon entropy). It is deliberately a standalone, domain-agnostic
// primitive rather than private state buried inside that gate: #1324
// (confidence-based retrieval abstention) wants its own streaming statistic
// (a P² streaming percentile of a retrieval-margin signal) and its issue body
// explicitly asks whoever implements it to check whether a shared statistics
// module already exists here before writing a second one. This module is
// that shared home -- #1324's P² estimator, when implemented, belongs beside
// `WelfordStats` here, not duplicated elsewhere.

/// Online mean, variance, and standard deviation via Welford's algorithm.
///
/// Single-pass and constant-memory: each [`Self::update`] call folds in one
/// new observation using only the running count, mean, and `M2` (the running
/// sum of squared differences from the mean) -- no sample is ever stored, so
/// memory use does not grow with the length of the stream. See Welford
/// (1962) / Knuth, *The Art of Computer Programming* Vol. 2, §4.2.2.
///
/// `Copy` so a caller can cheaply snapshot the statistic BEFORE folding in a
/// new observation (see `DegenerateContentGate::observe` in
/// `degenerate_gate.rs`), to judge that observation against the baseline as
/// it stood before the observation itself could dilute it.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct WelfordStats {
    count: u64,
    mean: f64,
    /// Running sum of squared differences from the mean.
    m2: f64,
}

impl WelfordStats {
    /// A tracker with no observations yet.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Fold one new observation into the running mean/variance.
    pub(crate) fn update(&mut self, value: f64) {
        self.count += 1;
        let delta = value - self.mean;
        self.mean += delta / self.count as f64;
        let delta2 = value - self.mean;
        self.m2 += delta * delta2;
    }

    /// How many observations have been folded in so far.
    pub(crate) fn count(&self) -> u64 {
        self.count
    }

    /// The running mean. `0.0` before any observation, matching an empty
    /// sum's identity rather than a meaningful statistic -- callers must
    /// consult [`Self::count`] before trusting it.
    pub(crate) fn mean(&self) -> f64 {
        self.mean
    }

    /// Sample variance (Bessel-corrected, `n - 1` denominator).
    ///
    /// `None` with fewer than 2 observations, where sample variance is
    /// mathematically undefined rather than merely imprecise.
    pub(crate) fn variance(&self) -> Option<f64> {
        if self.count < 2 {
            return None;
        }
        Some(self.m2 / (self.count - 1) as f64)
    }

    /// Sample standard deviation, or `None` under the same condition as
    /// [`Self::variance`].
    pub(crate) fn stddev(&self) -> Option<f64> {
        self.variance().map(f64::sqrt)
    }

    /// How many standard deviations BELOW the running mean `value` sits, as
    /// a positive number when `value` is below the mean (negative when
    /// above).
    ///
    /// `None` when [`Self::stddev`] is `None` (fewer than 2 observations), or
    /// when the stddev is exactly `0.0` (every observation so far has been
    /// identical, so there is no spread yet to measure a deviation against --
    /// treating a first-seen different value as an unbounded number of
    /// deviations away would manufacture a signal out of an absent one).
    pub(crate) fn deviations_below_mean(&self, value: f64) -> Option<f64> {
        let stddev = self.stddev()?;
        if stddev == 0.0 {
            return None;
        }
        Some((self.mean - value) / stddev)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Closed-form population mean over a fixed slice, for comparison against
    /// `WelfordStats`'s online estimate.
    fn closed_form_mean(values: &[f64]) -> f64 {
        values.iter().sum::<f64>() / values.len() as f64
    }

    /// Closed-form sample variance (Bessel-corrected), computed the
    /// textbook two-pass way: mean first, then the mean of squared
    /// deviations from it. Deliberately a different code path than
    /// `WelfordStats::variance`'s single-pass `M2` recurrence, so the two
    /// are checking each other rather than sharing a bug.
    fn closed_form_sample_variance(values: &[f64]) -> f64 {
        let mean = closed_form_mean(values);
        let sum_sq_dev: f64 = values.iter().map(|v| (v - mean).powi(2)).sum();
        sum_sq_dev / (values.len() - 1) as f64
    }

    #[test]
    fn test_welford_mean_and_stddev_match_closed_form_calculation() {
        let values = [2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0];

        let mut welford = WelfordStats::new();
        for &v in &values {
            welford.update(v);
        }

        let expected_mean = closed_form_mean(&values);
        let expected_variance = closed_form_sample_variance(&values);
        let expected_stddev = expected_variance.sqrt();

        assert_eq!(
            welford.count(),
            values.len() as u64,
            "count must equal the number of update() calls: got {}",
            welford.count()
        );
        assert!(
            (welford.mean() - expected_mean).abs() < 1e-9,
            "welford mean {} vs closed-form mean {} over {:?}",
            welford.mean(),
            expected_mean,
            values
        );
        assert!(
            (welford
                .variance()
                .expect("8 samples is enough for variance")
                - expected_variance)
                .abs()
                < 1e-9,
            "welford variance {:?} vs closed-form variance {} over {:?}",
            welford.variance(),
            expected_variance,
            values
        );
        assert!(
            (welford.stddev().expect("8 samples is enough for stddev") - expected_stddev).abs()
                < 1e-9,
            "welford stddev {:?} vs closed-form stddev {} over {:?}",
            welford.stddev(),
            expected_stddev,
            values
        );
    }

    #[test]
    fn test_welford_variance_and_stddev_are_none_below_two_samples() {
        let mut welford = WelfordStats::new();
        assert_eq!(welford.variance(), None, "0 samples: variance is undefined");
        assert_eq!(welford.stddev(), None, "0 samples: stddev is undefined");

        welford.update(3.0);
        assert_eq!(
            welford.variance(),
            None,
            "1 sample: sample variance (n-1 denominator) is still undefined"
        );
        assert_eq!(
            welford.stddev(),
            None,
            "1 sample: stddev is still undefined"
        );

        welford.update(5.0);
        assert!(
            welford.variance().is_some(),
            "2 samples: variance becomes defined"
        );
    }

    #[test]
    fn test_deviations_below_mean_is_none_when_stddev_is_zero() {
        let mut welford = WelfordStats::new();
        welford.update(10.0);
        welford.update(10.0);
        welford.update(10.0);
        assert_eq!(
            welford.stddev(),
            Some(0.0),
            "three identical observations must have zero spread"
        );
        assert_eq!(
            welford.deviations_below_mean(1.0),
            None,
            "a zero-spread baseline cannot report a finite z-score for a differing value"
        );
    }

    #[test]
    fn test_deviations_below_mean_reports_a_positive_score_for_a_low_value() {
        let mut welford = WelfordStats::new();
        for &v in &[10.0, 12.0, 8.0, 11.0, 9.0, 10.0, 13.0, 7.0] {
            welford.update(v);
        }
        let mean = welford.mean();
        let stddev = welford.stddev().expect("8 samples is enough for stddev");
        let probe = mean - 3.0 * stddev;

        let z = welford
            .deviations_below_mean(probe)
            .expect("stddev is nonzero here");
        assert!(
            (z - 3.0).abs() < 1e-9,
            "a value exactly 3 stddev below the mean ({probe}) must score z = 3.0, got {z} \
             (mean={mean}, stddev={stddev})"
        );

        let above = welford
            .deviations_below_mean(mean + stddev)
            .expect("stddev is nonzero here");
        assert!(
            above < 0.0,
            "a value ABOVE the mean must score negative 'deviations below', got {above}"
        );
    }
}
