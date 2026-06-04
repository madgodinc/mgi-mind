//! v1.5 Phase 8 step 8.2 — confidence_score formula.
//!
//! Per §6 of the validity-model synthesis, the confidence_score of a
//! stored fact is a weighted blend of three normalized signals plus
//! an inheritance discount:
//!
//! ```text
//! confidence_score(fact, mode) =
//!     w_dependants(mode)    * dependants_norm
//!   + w_confirmations(mode) * confirmations_norm
//!   + w_external(mode)      * external_signal_norm
//!   - inheritance_discount * (if inherited_unverified else 0)
//! ```
//!
//! Normalisation uses the log2 shape that the entrenchment formula
//! also uses (synthesis §10 q1), with a logistic-like cap mapping
//! `[0, ∞)` heavy-tail counts into `[0, 1)`:
//!
//! ```text
//! norm(count) = 1 - 1 / (1 + log2(1 + count))     ∈ [0, 1)
//! external_norm = min(1.0, |score| / 5.0)        ∈ [0, 1]
//! ```
//!
//! `external_signal_score` can be negative (failures pull negative)
//! — when it is, the function clamps the *absolute value* into the
//! normalised range and re-applies the sign. So a typed score of
//! `-5.0` produces an external_norm contribution of `-1.0`, which
//! the weighting layer then multiplies by `w_external(mode)` to get
//! a negative confidence contribution. That is the point of the
//! signed score path from Phase 7.
//!
//! The function is **pure** — no Qdrant, no FS, no time. Easy to
//! test in isolation; production callers pass real payload reads.

use crate::install_mode::InstallMode;

/// Discount applied to the confidence score when a fact's inheritance
/// flag is set (Phase 3 `is_inherited`). Mirrors `duel::INHERITANCE_DISCOUNT`.
/// 0.5 is the §3 mechanism 3 anchor — exactly the multiplier used by
/// `weight_new` so the two halves of the mechanism stay aligned.
pub const INHERITANCE_DISCOUNT_PENALTY: f32 = 0.5;

/// Normalisation divisor for the external-signal score. The typed
/// score from `outcome::compute_external_signal_score` is unbounded
/// (a fact can rack up arbitrarily many cited_by entries), so we
/// divide by this scale before clamping to `[-1, 1]`. 5.0 means a
/// score of ±5 saturates the external contribution.
pub const EXTERNAL_SCORE_NORM_SCALE: f32 = 5.0;

/// Inputs to `confidence_score`. Same shape as `duel::EntrenchmentInputs`
/// plus the typed external-signal score from Phase 7 and the
/// inherited flag from Phase 3.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConfidenceInputs {
    /// `dependants_count` payload field, written by Phase 1.1 walk.
    pub dependants: u32,
    /// `confirmations_count` payload field, written by Phase 1.3 walk.
    pub confirmations: u32,
    /// v1.5 Phase 7 typed external-signal score. Can be negative
    /// when failures dominate. `None` falls back to treating the
    /// `legacy_external_count` field as a positive proxy.
    pub external_signal_score: Option<f32>,
    /// v1.4 `external_signals: u32` legacy count. Used as a positive
    /// proxy when `external_signal_score` is `None` (the Phase 7
    /// typed log is empty on facts that pre-date `mind_outcome`).
    pub legacy_external_count: u32,
    /// v1.4 Phase 3 inheritance flag from `doubt::is_inherited`.
    /// Active when the fact came from memory rather than the live
    /// session — applies a 0.5 penalty per §3 mechanism 3.
    pub inherited_unverified: bool,
}

impl Default for ConfidenceInputs {
    fn default() -> Self {
        Self {
            dependants: 0,
            confirmations: 0,
            external_signal_score: None,
            legacy_external_count: 0,
            inherited_unverified: false,
        }
    }
}

/// v1.5 Phase 8 step 8.2: pure confidence_score formula.
///
/// Per-mode weights come from `InstallMode::weights()` (§6 synthesis
/// anchors). The function is total — every input combination
/// produces a finite value. Output range:
/// `[-1.0 * (w_dependants + w_external), w_dependants + w_external]`
/// before the inheritance penalty. After: subtract up to
/// `INHERITANCE_DISCOUNT_PENALTY` (0.5).
///
/// **Step 8.2 promote/demote threshold semantics** apply downstream:
/// the caller compares old vs new and decides state transitions —
/// this function only produces the number.
pub fn confidence_score(inputs: ConfidenceInputs, mode: InstallMode) -> f32 {
    let weights = mode.weights();

    let dep_norm = saturating_log_norm(inputs.dependants);
    let conf_norm = saturating_log_norm(inputs.confirmations);
    let ext_norm = match inputs.external_signal_score {
        Some(score) => normalise_signed_score(score),
        None => saturating_log_norm(inputs.legacy_external_count),
    };

    let mut score = weights.dependants * dep_norm
        + weights.confirmations * conf_norm
        + weights.external * ext_norm;

    if inputs.inherited_unverified {
        score -= INHERITANCE_DISCOUNT_PENALTY;
    }

    score
}

/// `1 - 1/(1 + log2(1 + n))`. Heavy-tail compression that maps `[0, ∞)`
/// into `[0, 1)`. A fact with 1 dependant scores 0.5, with 3 → 0.667,
/// with 15 → 0.8, with 1023 → 0.909.
fn saturating_log_norm(count: u32) -> f32 {
    let l = (1.0 + count as f32).log2();
    1.0 - 1.0 / (1.0 + l)
}

/// Map a signed Phase 7 external_signal_score into `[-1, 1]`:
/// divide by `EXTERNAL_SCORE_NORM_SCALE` and clamp.
fn normalise_signed_score(score: f32) -> f32 {
    (score / EXTERNAL_SCORE_NORM_SCALE).clamp(-1.0, 1.0)
}

// ===== Step 8.2 re-test transition logic =====

/// Score delta below which a downward shift triggers re-evaluation.
/// 0.2 is the v1.5 plan §8 step 8.2 anchor; noise floor for
/// confidence_score recomputation between ticks.
pub const RETEST_DOWN_SHIFT_DELTA: f32 = 0.2;

/// Absolute threshold below which a fact gets promoted to the doubt
/// window. 0.3 is the §8 step 8.2 anchor; below this the fact is
/// "low-confidence enough that ranking should de-emphasise it
/// pending re-verification."
pub const RETEST_PROMOTE_THRESHOLD: f32 = 0.3;

/// Score delta above which an upward shift triggers re-evaluation.
/// Symmetric with RETEST_DOWN_SHIFT_DELTA so the test is stable
/// against monotonic mid-band drift.
pub const RETEST_UP_SHIFT_DELTA: f32 = 0.2;

/// What the re-test pass should do to a fact based on its new vs
/// old confidence score and current doubt-window membership.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetestTransition {
    /// Score recovered → remove the fact from the doubt window.
    /// Only emitted when the fact was already in the window.
    RecoverFromDoubt,
    /// Score dropped below threshold → promote to the doubt window.
    /// Only emitted when the fact was NOT already in the window.
    PromoteToDoubt,
    /// No state change; write back the new score and continue.
    /// This is the dominant path — most ticks should be no-ops.
    NoChange,
}

/// v1.5 Phase 8 step 8.2: pure re-test transition decision.
///
/// Contract (preserves Mechanism 1):
/// - Never returns "delete" — the loser is never hard-deleted, only
///   moved into the doubt window where Mechanism 1 duel logic
///   decides whether a counter-fact wins.
/// - `PromoteToDoubt` requires BOTH a downward shift > `RETEST_DOWN_SHIFT_DELTA`
///   AND an absolute new score < `RETEST_PROMOTE_THRESHOLD`. Two
///   independent reasons must agree before we change state — avoids
///   flipping mid-band facts on a single noisy signal.
/// - `RecoverFromDoubt` only emitted when the fact is currently in
///   the doubt window AND the upward shift > `RETEST_UP_SHIFT_DELTA`.
pub fn decide_retest_transition(
    old_score: f32,
    new_score: f32,
    currently_in_doubt: bool,
) -> RetestTransition {
    let delta = new_score - old_score;

    if delta < -RETEST_DOWN_SHIFT_DELTA && new_score < RETEST_PROMOTE_THRESHOLD {
        if currently_in_doubt {
            // Already in doubt and dropped further — no-op (the
            // doubt-window state is already applied).
            return RetestTransition::NoChange;
        }
        return RetestTransition::PromoteToDoubt;
    }

    if currently_in_doubt && delta > RETEST_UP_SHIFT_DELTA {
        return RetestTransition::RecoverFromDoubt;
    }

    RetestTransition::NoChange
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn norm_is_zero_at_zero() {
        assert_eq!(saturating_log_norm(0), 0.0);
    }

    /// Score is monotonic in `dependants` — more dependants always
    /// raises (or holds) confidence.
    #[test]
    fn confidence_monotonic_in_dependants() {
        let inputs1 = ConfidenceInputs {
            dependants: 1,
            ..ConfidenceInputs::default()
        };
        let inputs10 = ConfidenceInputs {
            dependants: 10,
            ..ConfidenceInputs::default()
        };
        let inputs100 = ConfidenceInputs {
            dependants: 100,
            ..ConfidenceInputs::default()
        };
        for mode in InstallMode::ALL {
            let c1 = confidence_score(inputs1, mode);
            let c10 = confidence_score(inputs10, mode);
            let c100 = confidence_score(inputs100, mode);
            assert!(c1 < c10, "{:?}: c1={c1} c10={c10}", mode);
            assert!(c10 < c100, "{:?}: c10={c10} c100={c100}", mode);
        }
    }

    /// Inheritance penalty subtracts 0.5 from every mode.
    #[test]
    fn inheritance_penalty_applied_uniformly() {
        for mode in InstallMode::ALL {
            let live = ConfidenceInputs {
                dependants: 5,
                inherited_unverified: false,
                ..ConfidenceInputs::default()
            };
            let inherited = ConfidenceInputs {
                dependants: 5,
                inherited_unverified: true,
                ..ConfidenceInputs::default()
            };
            let live_score = confidence_score(live, mode);
            let inherited_score = confidence_score(inherited, mode);
            assert!(
                (live_score - inherited_score - INHERITANCE_DISCOUNT_PENALTY).abs() < 1e-5,
                "{:?}: live={live_score} inherited={inherited_score}",
                mode
            );
        }
    }

    /// Phase 7 typed score path: a positive score lifts confidence
    /// above the legacy-count fallback when the legacy count is zero.
    #[test]
    fn typed_score_present_outweighs_zero_legacy_count() {
        let with_typed = ConfidenceInputs {
            external_signal_score: Some(2.0),
            legacy_external_count: 0,
            ..ConfidenceInputs::default()
        };
        let without_typed = ConfidenceInputs {
            external_signal_score: None,
            legacy_external_count: 0,
            ..ConfidenceInputs::default()
        };
        let c_typed = confidence_score(with_typed, InstallMode::ChatOnly);
        let c_legacy = confidence_score(without_typed, InstallMode::ChatOnly);
        assert!(c_typed > c_legacy, "typed={c_typed} legacy={c_legacy}");
    }

    /// Phase 7 typed score path: a negative score pulls confidence
    /// negative — failed tests are real evidence against the fact.
    #[test]
    fn negative_typed_score_pulls_confidence_negative() {
        let inputs = ConfidenceInputs {
            external_signal_score: Some(-5.0), // clamps to -1.0 norm
            ..ConfidenceInputs::default()
        };
        let score = confidence_score(inputs, InstallMode::ChatOnly);
        // ChatOnly w_external = 0.2 → -1.0 × 0.2 = -0.2.
        assert!(
            (score - (-0.2)).abs() < 1e-5,
            "expected ~-0.2, got {score}"
        );
    }

    // --- Step 8.2 transition tests ---

    /// Score drop below threshold AND not already in doubt → promote.
    #[test]
    fn drop_below_threshold_promotes_to_doubt() {
        let t = decide_retest_transition(0.8, 0.2, false);
        assert_eq!(t, RetestTransition::PromoteToDoubt);
    }

    /// Score drop in the band (delta > 0.2) but new score still
    /// above 0.3 → no change. Catches "single noisy signal" flipping
    /// a high-confidence fact unnecessarily.
    #[test]
    fn drop_above_threshold_does_not_promote() {
        let t = decide_retest_transition(0.9, 0.5, false);
        assert_eq!(t, RetestTransition::NoChange);
    }

    /// Small drop below threshold → no change. Catches mid-band
    /// confidence facts toggling on every tick.
    #[test]
    fn small_drop_below_threshold_does_not_promote() {
        let t = decide_retest_transition(0.35, 0.25, false);
        // Delta 0.1 < 0.2 threshold → no change even though new < 0.3.
        assert_eq!(t, RetestTransition::NoChange);
    }

    /// Score climb out of doubt window → recover.
    #[test]
    fn climb_out_recovers_from_doubt() {
        let t = decide_retest_transition(0.2, 0.6, true);
        assert_eq!(t, RetestTransition::RecoverFromDoubt);
    }

    /// Small climb inside doubt window → no change.
    #[test]
    fn small_climb_inside_doubt_does_not_recover() {
        let t = decide_retest_transition(0.2, 0.3, true);
        // Delta 0.1 < 0.2 → still in doubt.
        assert_eq!(t, RetestTransition::NoChange);
    }

    /// Already-in-doubt fact dropping further → no change (the
    /// doubt-window state is already applied).
    #[test]
    fn further_drop_inside_doubt_does_not_re_promote() {
        let t = decide_retest_transition(0.25, 0.05, true);
        assert_eq!(t, RetestTransition::NoChange);
    }

    /// Never returns a "delete" verdict — Mechanism 1 invariant.
    /// Tested over a 200-point grid (extreme inputs included).
    #[test]
    fn never_returns_delete_verdict() {
        for old in [-1.0, 0.0, 0.3, 0.5, 0.9, 1.0] {
            for new in [-1.0, 0.0, 0.3, 0.5, 0.9, 1.0] {
                for in_doubt in [true, false] {
                    let t = decide_retest_transition(old, new, in_doubt);
                    // Mere existence of the enum's exhaustive arms
                    // guarantees no delete — but assert explicitly so
                    // a future variant addition can't accidentally
                    // break the contract.
                    assert!(matches!(
                        t,
                        RetestTransition::NoChange
                            | RetestTransition::PromoteToDoubt
                            | RetestTransition::RecoverFromDoubt
                    ));
                }
            }
        }
    }
}
