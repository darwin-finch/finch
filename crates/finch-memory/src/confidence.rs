//! Confidence-based retrieval abstention (#1324).
//!
//! Two independent pieces, kept deliberately small:
//!
//! - [`retrieval_margin`]: the confidence signal itself -- the gap between
//!   the best and second-best retrieved candidate's score. The simplest
//!   signal tested against more elaborate alternatives in the validated
//!   research behind #1324; this module does not try to improve on it.
//! - [`P2Quantile`]: a streaming, O(1)-memory estimator of a target
//!   percentile of that margin (the P² algorithm, Jain & Chlamtac 1985),
//!   so the abstention threshold adapts online instead of being a fixed,
//!   hand-picked margin cutoff. No history is stored -- five markers only,
//!   updated one observation at a time.
//!
//! #1323 (a separate, sibling ticket) investigated a Welford-based
//! streaming-stats primitive for a different purpose -- a pre-indexing
//! degenerate-content quality gate -- and both tickets asked whether the two
//! should share a module. As of this change #1323 has not landed anything in
//! this crate (checked via `git log`/open PRs before writing this), so there
//! is no existing shared statistics location to place this alongside.
//! `P2Quantile` stays self-contained here; if #1323 lands a genuine shared
//! streaming-stats home first, move this estimator there rather than
//! inventing a speculative shared abstraction now.
//!
//! Neither piece decides policy (what fraction of queries to answer, what to
//! do with an under-determined turn) -- see `MemoryConfig::confidence_abstention`
//! and its use in `MemorySystem::query_with_sources` in `lib.rs` for that.

/// The confidence signal: the margin between the best and second-best
/// retrieval candidate's score. `scores` need not be sorted.
///
/// Returns `None` when fewer than two scores are available: a margin is
/// undefined for zero or one candidate. This module makes no policy choice
/// about what a caller should do in that case.
pub(crate) fn retrieval_margin(scores: &[f32]) -> Option<f32> {
    let mut best = f32::NEG_INFINITY;
    let mut second = f32::NEG_INFINITY;
    let mut seen = 0usize;
    for &s in scores {
        seen += 1;
        if s > best {
            second = best;
            best = s;
        } else if s > second {
            second = s;
        }
    }
    if seen < 2 {
        return None;
    }
    Some(best - second)
}

/// A streaming (P²) estimator of the `p`-th percentile of a data stream,
/// after Jain & Chlamtac (1985), "The P² Algorithm for Dynamic Calculation
/// of Quantiles and Histograms Without Storing Observations". O(1) memory:
/// five marker heights and five marker positions, no observation history.
///
/// The estimate is only meaningful once five observations have been seen
/// (`value()` returns `None` before that) -- an explicit minimum-sample
/// guard, the same shape #1323's Welford-based gate uses for its own
/// minimum-sample-count guard, applied here to a different statistic.
#[derive(Debug, Clone)]
pub(crate) struct P2Quantile {
    /// Buffer for the first five observations, sorted once full to seed the
    /// five marker heights in ascending order. Empty after initialization.
    initial: Vec<f64>,
    /// Marker heights (`q`), always kept sorted ascending.
    q: [f64; 5],
    /// Marker positions (`n`), integer-valued but stored as `f64` for
    /// uniform arithmetic with the desired positions below.
    n: [f64; 5],
    /// Desired marker positions (`n'`).
    np: [f64; 5],
    /// Desired marker position increments (`dn'`), constant for a given `p`.
    dn: [f64; 5],
    /// Whether the five initial markers have been seeded.
    initialized: bool,
}

impl P2Quantile {
    /// A new tracker targeting the `p`-th percentile, `p` in `[0.0, 1.0]`.
    pub(crate) fn new(p: f64) -> Self {
        debug_assert!(
            (0.0..=1.0).contains(&p),
            "P2Quantile target percentile must be in [0.0, 1.0]; got {p}"
        );
        let p = p.clamp(0.0, 1.0);
        Self {
            initial: Vec::with_capacity(5),
            q: [0.0; 5],
            n: [1.0, 2.0, 3.0, 4.0, 5.0],
            np: [1.0, 1.0 + 2.0 * p, 1.0 + 4.0 * p, 3.0 + 2.0 * p, 5.0],
            dn: [0.0, p / 2.0, p, (1.0 + p) / 2.0, 1.0],
            initialized: false,
        }
    }

    /// Fold one new observation into the running estimate.
    pub(crate) fn observe(&mut self, x: f64) {
        if !self.initialized {
            self.initial.push(x);
            if self.initial.len() < 5 {
                return;
            }
            self.initial.sort_by(|a, b| {
                a.partial_cmp(b)
                    .expect("P2Quantile observations must not be NaN")
            });
            self.q.copy_from_slice(&self.initial);
            self.initial.clear();
            self.initialized = true;
            return;
        }

        // B.1: find the cell k (0-indexed marker index) such that
        // q[k] <= x < q[k+1], extending the outer markers if x falls
        // outside the currently tracked range.
        let k = if x < self.q[0] {
            self.q[0] = x;
            0
        } else if x >= self.q[4] {
            self.q[4] = x;
            3
        } else {
            (0..4).find(|&i| x < self.q[i + 1]).unwrap_or(3)
        };

        // B.2: every marker position strictly after cell k shifts by one
        // real observation; every desired position advances by its own
        // fixed increment regardless of which cell x landed in.
        for i in (k + 1)..5 {
            self.n[i] += 1.0;
        }
        for i in 0..5 {
            self.np[i] += self.dn[i];
        }

        // B.3: adjust the three interior markers (indices 1..=3) toward
        // their desired positions, one step at a time, via parabolic
        // interpolation when it keeps the markers sorted, linear otherwise.
        for i in 1..4 {
            let d = self.np[i] - self.n[i];
            if (d >= 1.0 && self.n[i + 1] - self.n[i] > 1.0)
                || (d <= -1.0 && self.n[i - 1] - self.n[i] < -1.0)
            {
                let sign = if d >= 0.0 { 1.0 } else { -1.0 };
                let candidate = self.parabolic(i, sign);
                self.q[i] = if self.q[i - 1] < candidate && candidate < self.q[i + 1] {
                    candidate
                } else {
                    self.linear(i, sign)
                };
                self.n[i] += sign;
            }
        }
    }

    /// Parabolic (P²) interpolation formula for adjusting marker `i` by
    /// `d` (`+1.0` or `-1.0`).
    fn parabolic(&self, i: usize, d: f64) -> f64 {
        let (n, q) = (&self.n, &self.q);
        q[i] + d / (n[i + 1] - n[i - 1])
            * ((n[i] - n[i - 1] + d) * (q[i + 1] - q[i]) / (n[i + 1] - n[i])
                + (n[i + 1] - n[i] - d) * (q[i] - q[i - 1]) / (n[i] - n[i - 1]))
    }

    /// Linear fallback for adjusting marker `i` by `d` (`+1.0` or `-1.0`),
    /// used whenever the parabolic estimate would leave the markers
    /// unsorted.
    fn linear(&self, i: usize, d: f64) -> f64 {
        let j = (i as isize + d as isize) as usize;
        self.q[i] + d * (self.q[j] - self.q[i]) / (self.n[j] - self.n[i])
    }

    /// The current estimate of the target percentile, or `None` before five
    /// observations have been seen (the gate stays inactive/permissive
    /// until then, mirroring #1323's own minimum-sample-count guard).
    pub(crate) fn value(&self) -> Option<f64> {
        self.initialized.then_some(self.q[2])
    }
}

#[cfg(test)]
impl P2Quantile {
    /// Test-only: an already-initialized tracker whose current estimate is
    /// exactly `value`, so a test can exercise the abstention decision at a
    /// precise, known threshold without replaying real observations to
    /// reach it. Never constructed by production code -- `cfg(test)` only,
    /// same crate, so no `test-support` feature is needed (unlike a seam a
    /// *different* crate's tests must drive).
    pub(crate) fn fixed_for_test(value: f64) -> Self {
        let mut tracker = Self::new(0.5);
        tracker.q = [value; 5];
        tracker.initialized = true;
        tracker
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- retrieval_margin ---

    #[test]
    fn test_retrieval_margin_returns_gap_between_top_two() {
        let margin = retrieval_margin(&[0.9, 0.3, 0.7]).expect("two candidates give a margin");
        assert!(
            (margin - 0.2).abs() < 1e-6,
            "margin must be best (0.9) minus second-best (0.7) = 0.2, \
             independent of input order; got {margin}"
        );
    }

    #[test]
    fn test_retrieval_margin_none_for_fewer_than_two_scores() {
        assert_eq!(
            retrieval_margin(&[]),
            None,
            "zero candidates have no defined margin"
        );
        assert_eq!(
            retrieval_margin(&[0.5]),
            None,
            "a single candidate has no second-best to take a margin against"
        );
    }

    #[test]
    fn test_retrieval_margin_zero_when_top_two_are_tied() {
        let margin = retrieval_margin(&[0.5, 0.5, 0.1]);
        assert_eq!(
            margin,
            Some(0.0),
            "a tie between the best and second-best is a real zero margin, \
             not an error; got {margin:?}"
        );
    }

    #[test]
    fn test_retrieval_margin_ignores_candidates_beyond_the_top_two() {
        // A third, even-lower candidate must not change the margin at all.
        let with_third = retrieval_margin(&[1.0, 0.4, -5.0]);
        let without_third = retrieval_margin(&[1.0, 0.4]);
        assert_eq!(
            with_third, without_third,
            "the margin is defined only by the top two candidates; a third, \
             lower one must not perturb it (got {with_third:?} vs \
             {without_third:?})"
        );
    }

    // --- P2Quantile convergence ---

    /// A tiny deterministic xorshift PRNG so the convergence tests need no
    /// external `rand` dependency and are byte-for-byte reproducible.
    struct XorShift64(u64);

    impl XorShift64 {
        fn next_u64(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }

        /// Uniform in `[0.0, 1.0)`.
        fn next_f64(&mut self) -> f64 {
            (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    /// Feeds `n` uniform(0,1) samples from a fixed seed into a tracker
    /// targeting `p`, returning its final estimate. The true `p`-th
    /// percentile of Uniform(0,1) is `p` itself, so the estimate can be
    /// checked directly against `p`.
    fn converged_estimate(p: f64, n: usize, seed: u64) -> f64 {
        let mut rng = XorShift64(seed);
        let mut tracker = P2Quantile::new(p);
        for _ in 0..n {
            tracker.observe(rng.next_f64());
        }
        tracker
            .value()
            .expect("n >> 5 observations must have initialized the tracker")
    }

    #[test]
    fn test_p2_quantile_converges_to_the_median_on_a_uniform_stream() {
        let estimate = converged_estimate(0.5, 20_000, 0xC0FFEE);
        assert!(
            (estimate - 0.5).abs() < 0.02,
            "the streaming median of Uniform(0,1) over 20,000 samples must \
             converge near the true median 0.5 within a 0.02 tolerance; got \
             {estimate}"
        );
    }

    #[test]
    fn test_p2_quantile_converges_to_the_90th_percentile_on_a_uniform_stream() {
        let estimate = converged_estimate(0.9, 20_000, 0xC0FFEE);
        assert!(
            (estimate - 0.9).abs() < 0.02,
            "the streaming 90th percentile of Uniform(0,1) over 20,000 \
             samples (the default 'answer only the top 10%' cutoff's target \
             percentile) must converge near the true value 0.9 within a 0.02 \
             tolerance; got {estimate}"
        );
    }

    #[test]
    fn test_p2_quantile_converges_to_the_10th_percentile_on_a_uniform_stream() {
        let estimate = converged_estimate(0.1, 20_000, 0xC0FFEE);
        assert!(
            (estimate - 0.1).abs() < 0.02,
            "the streaming 10th percentile of Uniform(0,1) over 20,000 \
             samples must converge near the true value 0.1 within a 0.02 \
             tolerance; got {estimate}"
        );
    }

    #[test]
    fn test_p2_quantile_returns_none_before_five_observations() {
        let mut tracker = P2Quantile::new(0.9);
        for i in 0..4 {
            assert_eq!(
                tracker.value(),
                None,
                "the tracker must report no estimate before its fifth \
                 observation (at observation {i}), so a caller cannot act on \
                 a statistic with too few samples to be meaningful"
            );
            tracker.observe(0.1 * i as f64);
        }
        tracker.observe(0.5);
        assert!(
            tracker.value().is_some(),
            "the tracker must report an estimate once five observations \
             have been seen"
        );
    }

    #[test]
    fn test_p2_quantile_is_deterministic_for_the_same_stream() {
        let a = converged_estimate(0.75, 500, 42);
        let b = converged_estimate(0.75, 500, 42);
        assert_eq!(
            a, b,
            "replaying the exact same seeded stream must produce the exact \
             same estimate -- no hidden nondeterminism (wall clock, thread \
             scheduling) in the algorithm itself"
        );
    }
}
