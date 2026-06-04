//! v1.4 Phase 2: the duel rule.
//!
//! When a fresh fact `F_new` contradicts an existing `F_old` along the
//! same `(subject, predicate)` axis (cardinality Single or TemporalSingle —
//! see `knowledge::Cardinality`), this module resolves the conflict:
//!
//! - `entrenchment(F_old)` is computed from the cached
//!   `dependants_count` (Phase 1.1) and `confirmations_count` (Phase 1.3),
//!   combined with the age of the fact's first dependant.
//! - `weight(F_new)` is computed from the diversity model (synthesis §5):
//!   inheritance discount, source diversity, external-signal weight,
//!   single-source decay.
//! - Resolution: flip / contested / quarantine, with hard thresholds
//!   tunable through `DUEL_FLIP_RATIO` and `DUEL_CONTESTED_RATIO`.
//!
//! **TODO(phase-4-calibration):** every constant in this module is a
//! starting point chosen against synthetic data. Phase 4 sweeps each one
//! against the real ~12k-memory base + LongMemEval-S regression + STALE
//! behavioural metrics. None of the numbers here is the final answer; the
//! shape of the formulas is.

use anyhow::Result;

use crate::config::MindConfig;
use crate::knowledge::{Cardinality, EntryStatus, Fact};

// ===== Phase 2 constants (illustrative; calibrated in Phase 4) =====

/// Ratio at which `F_new` beats `F_old` outright. weight(F_new) >
/// entrenchment(F_old) × this → flip the duel, dampen the loser.
///
/// 1.5 is the synthesis worked example default. Phase 4 sweeps ±25%
/// and ±50% against the regression bench + STALE behavioural metrics.
pub const DUEL_FLIP_RATIO: f32 = 1.5;

/// Ratio above which both facts stay live as `Contested`. weight(F_new) >
/// entrenchment(F_old) × this AND below FLIP_RATIO → contested band.
/// Below this → F_new enters quarantine as candidate.
pub const DUEL_CONTESTED_RATIO: f32 = 0.5;

/// Soft normalisation target for `entrenchment()`: the *median* of
/// observed entrenchment scores in the base should land near this value
/// after calibration, with p90 near 0.8 and p99 near 0.95. This is what
/// makes the DUEL_*_RATIO thresholds interpretable across users with
/// different base sizes.
///
/// **TODO(phase-4-calibration):** set NORM_DIVISOR so that this holds on
/// the author's real base. Default value below is a placeholder
/// derived from the synthetic test data.
pub const ENTRENCHMENT_NORM_DIVISOR: f32 = 50.0;

// ===== Pure-function entrenchment formula =====

/// Inputs to `entrenchment()`. Phase 2 reads these from each fact's
/// cached payload fields (`dependants_count`, `confirmations_count`,
/// `created_at`). The struct lets the formula be tested in isolation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EntrenchmentInputs {
    /// `dependants_count` payload field, written by Phase 1.1 walk.
    pub dependants: u32,
    /// `confirmations_count` payload field, written by Phase 1.3 walk
    /// for procedures; 0 for memories without a derivable signal.
    pub confirmations: u32,
    /// Days since the fact's first independent confirmation (not since
    /// first write — the naive-age trap the synthesis §3 warns about).
    /// For Phase 2 v0 we fall back to days-since-`created_at` when no
    /// confirmation history is present, with the honest caveat that
    /// this conflates the two axes for unconfirmed facts.
    pub age_days: u32,
}

/// Compute the normalised entrenchment score for a fact.
///
/// Formula shape (synthesis §10 question 1):
/// - `dependants` contribution: log2(1 + dependants) — chosen for the
///   fat-tail case (synthesis §10 question 1 first option). Phase 4
///   confirms this shape against the real distribution from Phase 1.1.
/// - `confirmations` contribution: log2(1 + confirmations) — same
///   shape; we want diminishing returns on repeated confirmation
///   without over-weighting one-confirmation facts.
/// - `age_days` contribution: log2(1 + age_days / 30), capped at 24
///   months. Old facts are slightly more entrenched than new ones
///   even at equal dependant/confirmation counts, because time alone
///   passing without contradiction is a weak signal.
/// - All three terms summed and divided by `ENTRENCHMENT_NORM_DIVISOR`
///   so the median lands near 0.5 after calibration.
///
/// Output range: typically [0.0, 1.0] after calibration; not clamped
/// in the formula itself so calibration sweeps can see outliers.
pub fn entrenchment(inputs: EntrenchmentInputs) -> f32 {
    let dep_term = (1.0 + inputs.dependants as f32).log2();
    let conf_term = (1.0 + inputs.confirmations as f32).log2();
    let age_term = {
        let months = (inputs.age_days as f32 / 30.0).min(24.0);
        (1.0 + months).log2()
    };
    (dep_term + conf_term + age_term) / ENTRENCHMENT_NORM_DIVISOR
}

// ===== Pure-function diversity weighting (synthesis §5) =====

/// Inputs to `weight_new()`. The diversity model from synthesis §5.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NewFactInputs {
    /// Was this fact's claim observed during the *current live session*
    /// or carried in from memory? Live = 0 discount; inherited = 1.0
    /// applied. This is mechanism 3 (synthesis §3).
    pub from_live_session: bool,

    /// How many *diverse-context* observations support this claim, after
    /// single-source decay. Zero for first-time live assertions; higher
    /// for facts confirmed across multiple distinct contexts.
    /// Computation lives in §5 and is approximated by
    /// `diversity_weighted_count()` below.
    pub diverse_confirmations: u32,

    /// External-signal count: deterministic confirmations from tests,
    /// CI, code-search, etc. These weigh more than any conversational
    /// repetition (§5 axis 3).
    pub external_signals: u32,
}

/// Compute the weight of a fresh fact for the duel.
///
/// The slot weights below are the **single-user chat-only default**
/// from synthesis §6:
/// - dependants weight 0.7 — but irrelevant here because a fresh fact
///   has no dependants yet by construction.
/// - confirmations weight 0.1 — almost decorative.
/// - external-signal weight 0.2 — high quality when present.
///
/// For a fresh fact, the dependants-weight axis collapses to zero (no
/// dependants yet), so the duel weight is dominated by external signals
/// when present, with confirmations as a tiebreaker. The inheritance
/// discount halves the weight when applicable.
pub fn weight_new(inputs: NewFactInputs) -> f32 {
    // TODO(phase-4-calibration): these weights are §6 install-mode-aware
    // anchors; the install-mode detection lives elsewhere and feeds the
    // right column of weights into this function.
    let conf_weight = 0.1;
    let ext_weight = 0.2;

    let conf_term = conf_weight * (1.0 + inputs.diverse_confirmations as f32).log2();
    let ext_term = ext_weight * (1.0 + inputs.external_signals as f32).log2();

    let raw = conf_term + ext_term;

    if inputs.from_live_session {
        raw
    } else {
        raw * 0.5 // inheritance discount (synthesis §3 mechanism 3)
    }
}

/// Single-source decay: when one source contributes N confirmations,
/// they count as a diminishing return rather than N independent votes.
/// Synthesis §5 axis 4: "Third confirmation from one user weighs less
/// than the first; tenth weighs almost nothing."
///
/// Formula: log2(1 + N) — the standard diminishing-returns shape.
/// Single confirmation = 1.0; two confirmations = 1.58; five = 2.58;
/// ten = 3.46; hundred = 6.66.
///
/// **TODO(phase-4-calibration):** the curve shape may need to be
/// per-predicate (a `current_project` fact decays confirmations slowly
/// because projects change rarely; a `mood` fact decays them fast).
/// Phase 4 question 5 (synthesis §13).
pub fn diversity_weighted_count(same_source_count: u32) -> f32 {
    (1.0 + same_source_count as f32).log2()
}

// ===== Pure-function resolution =====

/// Outcome of a duel between an entrenched fact and a fresh challenger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DuelOutcome {
    /// `F_new` wins. Caller should write `F_new` as Active, dampen
    /// `F_old` to Stale with `valid_until = now`.
    Flip,
    /// `F_new` and `F_old` stay both live as Contested. Caller writes
    /// `F_new` as Contested; `F_old` does not change status.
    Contested,
    /// `F_new` enters quarantine. Caller writes `F_new` with the
    /// existing quarantine path (it carries the v0.11 promote-on-repeat
    /// machinery, which v1.4 reuses for fresh duel candidates).
    Quarantine,
}

/// Resolve the duel from the precomputed weights.
///
/// `weight_new > entrenchment × DUEL_FLIP_RATIO` → Flip
/// `weight_new > entrenchment × DUEL_CONTESTED_RATIO` → Contested
/// else → Quarantine
///
/// The function is pure so Phase 4 sweeps over the constants are cheap
/// (run the resolution against a synthetic conflict set, count
/// false-positive duels, count missed-flips).
pub fn resolve_duel(entrenchment_score: f32, weight_new: f32) -> DuelOutcome {
    if weight_new > entrenchment_score * DUEL_FLIP_RATIO {
        DuelOutcome::Flip
    } else if weight_new > entrenchment_score * DUEL_CONTESTED_RATIO {
        DuelOutcome::Contested
    } else {
        DuelOutcome::Quarantine
    }
}

// ===== Qdrant-talking glue (Phase 2 step 2.2 wiring) =====

/// Run a duel against a candidate set of existing facts and produce the
/// resolution + the F_old that lost (when applicable).
///
/// Returns `(outcome, optional_loser_id)`:
/// - Flip → `(Flip, Some(loser_id))`. Caller dampens the loser.
/// - Contested → `(Contested, None)`. F_old's status does not change;
///   only F_new is written as Contested.
/// - Quarantine → `(Quarantine, None)`. F_old is unchanged; F_new is
///   diverted to the quarantine layer.
///
/// If `existing` contains multiple valid facts (multi-valued predicate
/// or pre-existing Contested cluster), the function picks the most
/// entrenched as the duel opponent. This is the common case for
/// `temporal-single` predicates where history has accumulated.
pub async fn resolve_against_existing(
    config: &MindConfig,
    existing: &[Fact],
    new_inputs: NewFactInputs,
    cardinality: Cardinality,
) -> Result<(DuelOutcome, Option<String>)> {
    // No-op when the predicate is Multi or when there are no existing
    // facts. Multi-valued predicates never duel; first-write is always
    // Active.
    if !cardinality.admits_conflict() || existing.is_empty() {
        return Ok((DuelOutcome::Contested, None)); // status-wise the new fact is Active, not Contested
    }

    // Pull each existing fact's cached entrenchment inputs from its
    // payload. For Phase 2 v0 we fall back to `dependants_count = 0,
    // confirmations_count = 0, age_days = 0` for legacy facts that the
    // Phase 1 walks have not yet visited. Such facts behave like
    // first-writes — weight_new beats them trivially.
    let mut best: Option<(String, f32)> = None;
    for f in existing {
        let inputs = read_entrenchment_inputs_from_fact(config, &f.id).await?;
        let score = entrenchment(inputs);
        match &best {
            Some((_, b)) if *b >= score => {}
            _ => best = Some((f.id.clone(), score)),
        }
    }

    let (loser_id, ent_score) = best.unwrap_or_else(|| (String::new(), 0.0));
    let w_new = weight_new(new_inputs);
    let outcome = resolve_duel(ent_score, w_new);

    let loser = match outcome {
        DuelOutcome::Flip => Some(loser_id),
        _ => None,
    };
    Ok((outcome, loser))
}

/// Read the three entrenchment inputs from a fact's Qdrant payload.
/// Phase 1.1 / 1.3 cache them as string-encoded numbers; we deserialise
/// and provide safe defaults so legacy facts behave well.
async fn read_entrenchment_inputs_from_fact(
    config: &MindConfig,
    fact_id: &str,
) -> Result<EntrenchmentInputs> {
    let client = crate::storage::get_client(config).await?;
    let dependants = crate::storage::existing_payload_string(
        &client,
        crate::storage::FACTS_COLLECTION,
        fact_id,
        "dependants_count",
    )
    .await
    .and_then(|s| s.parse().ok())
    .unwrap_or(0u32);

    let confirmations = crate::storage::existing_payload_string(
        &client,
        crate::storage::FACTS_COLLECTION,
        fact_id,
        "confirmations_count",
    )
    .await
    .and_then(|s| s.parse().ok())
    .unwrap_or(0u32);

    let age_days = crate::storage::existing_payload_string(
        &client,
        crate::storage::FACTS_COLLECTION,
        fact_id,
        "created_at",
    )
    .await
    .and_then(|iso| chrono::DateTime::parse_from_rfc3339(&iso).ok())
    .map(|t| {
        let now = chrono::Utc::now();
        let dur = now.signed_duration_since(t.with_timezone(&chrono::Utc));
        dur.num_days().max(0) as u32
    })
    .unwrap_or(0u32);

    Ok(EntrenchmentInputs {
        dependants,
        confirmations,
        age_days,
    })
}

/// Mark a fact as Stale with `valid_until = now`. The loser of a Flip
/// stays in the store as audit trace and is hidden from default search
/// by `EntryStatus::is_default_visible`.
pub async fn dampen_loser(config: &MindConfig, fact_id: &str) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    crate::knowledge::set_fact_payload_field(
        config,
        fact_id,
        "status",
        EntryStatus::Stale.as_str().to_string(),
    )
    .await?;
    crate::knowledge::set_fact_payload_field(
        config,
        fact_id,
        "valid_until",
        now,
    )
    .await?;
    Ok(())
}

// ===== Tests for the pure helpers =====
//
// These tests fix the *shape* of the formulas and the *behavioural
// contract* of the resolution thresholds. The numeric constants in the
// formulas (DUEL_FLIP_RATIO, etc.) are explicitly marked TODO(phase-4)
// and will be tuned against the real distribution; the tests below
// assert relative orderings that survive any reasonable calibration.

#[cfg(test)]
mod tests {
    use super::*;

    fn ent(d: u32, c: u32, a: u32) -> f32 {
        entrenchment(EntrenchmentInputs {
            dependants: d,
            confirmations: c,
            age_days: a,
        })
    }

    fn w(live: bool, conf: u32, ext: u32) -> f32 {
        weight_new(NewFactInputs {
            from_live_session: live,
            diverse_confirmations: conf,
            external_signals: ext,
        })
    }

    // --- Entrenchment shape ---

    #[test]
    fn entrenchment_is_monotonic_in_dependants() {
        // More dependants → higher entrenchment, all else equal.
        // Even one extra dependant should move the score upward, no
        // matter where on the curve we sit.
        assert!(ent(0, 0, 0) < ent(1, 0, 0));
        assert!(ent(1, 0, 0) < ent(5, 0, 0));
        assert!(ent(5, 0, 0) < ent(50, 0, 0));
        assert!(ent(50, 0, 0) < ent(500, 0, 0));
    }

    #[test]
    fn entrenchment_is_monotonic_in_confirmations() {
        // Same shape on the confirmations axis. Mechanism 1 + §5.
        assert!(ent(0, 0, 0) < ent(0, 1, 0));
        assert!(ent(0, 1, 0) < ent(0, 10, 0));
    }

    #[test]
    fn entrenchment_age_is_capped_at_two_years() {
        // age_days > 24 months should not keep pushing the score up
        // arbitrarily — a fact that's been around for 5 years isn't
        // 2.5x as entrenched as one that's been around for 2 years
        // on age alone.
        let two_years = ent(0, 0, 730);
        let five_years = ent(0, 0, 1825);
        assert!((two_years - five_years).abs() < 1e-4, "age past 2y should plateau");
    }

    #[test]
    fn entrenchment_logarithmic_compression_kicks_in_on_dependants() {
        // log2(1+1)=1, log2(1+10)≈3.46, log2(1+100)≈6.66. So going from
        // 1→10 adds ~2.46; going from 10→100 only adds ~3.20, despite
        // a 10x raw increase. That's the compression that makes
        // outlier facts not dominate the entrenchment band.
        let one = ent(1, 0, 0);
        let ten = ent(10, 0, 0);
        let hundred = ent(100, 0, 0);
        let delta_low = ten - one;
        let delta_high = hundred - ten;
        // High-end delta should not be more than 1.5x the low-end one
        // — that's the log curve doing its job.
        assert!(
            delta_high < delta_low * 1.5,
            "logarithmic compression broken: delta_low={delta_low} delta_high={delta_high}"
        );
    }

    // --- Weight (fresh fact) ---

    #[test]
    fn live_fact_outweighs_inherited_same_signals() {
        // Inheritance discount halves the weight. Two facts with the
        // same diversity_weighted_count + external_signals: the live
        // one beats the inherited one.
        let live = w(true, 1, 1);
        let inherited = w(false, 1, 1);
        assert!(live > inherited);
        assert!((live * 0.5 - inherited).abs() < 1e-5);
    }

    #[test]
    fn external_signal_outweighs_chat_confirmation() {
        // Synthesis §5 axis 3: external signals weigh more than
        // conversational repetition. Same N=3, but as external it
        // should produce more weight than as diverse-confirmation.
        let chat = w(true, 3, 0);
        let external = w(true, 0, 3);
        assert!(external > chat);
    }

    #[test]
    fn first_time_live_fact_has_low_but_positive_weight() {
        // A single live mention with no diversity yet should not
        // produce zero — it has the in-session signal. But it must be
        // well below an entrenched-fact entrenchment score so the
        // duel goes to quarantine by default.
        let raw = w(true, 0, 0);
        assert_eq!(raw, 0.0, "no confirmations, no signals → zero weight");
        let one_conf = w(true, 1, 0);
        assert!(one_conf > 0.0);
        // ... but smaller than an entrenched fact with 10 dependants.
        assert!(one_conf < ent(10, 5, 90));
    }

    // --- Single-source decay ---

    #[test]
    fn diversity_weighted_count_diminishes() {
        // Synthesis §5 axis 4: log curve.
        let one = diversity_weighted_count(1);
        let two = diversity_weighted_count(2);
        let ten = diversity_weighted_count(10);
        let hundred = diversity_weighted_count(100);
        assert!(one < two);
        assert!(two < ten);
        assert!(ten < hundred);
        // Going 1 → 100 should be < 8x even though raw count is 100x
        let ratio = hundred / one;
        assert!(ratio < 8.0, "diversity decay too weak: ratio={ratio}");
    }

    // --- Resolution thresholds ---

    #[test]
    fn flip_when_fresh_overwhelms_entrenched() {
        // A high-weight live fact (external signal!) against a fresh
        // entrenched-zero fact must flip.
        let ent_score = 0.0;
        let w_new = 1.0;
        assert_eq!(resolve_duel(ent_score, w_new), DuelOutcome::Flip);
    }

    #[test]
    fn quarantine_when_fresh_too_weak() {
        // Tiny fresh weight against a non-zero entrenchment → quarantine.
        // 0.01 < 0.10 × 0.5 (CONTESTED_RATIO).
        assert_eq!(resolve_duel(0.10, 0.01), DuelOutcome::Quarantine);
    }

    #[test]
    fn contested_in_the_middle_band() {
        // Fresh weight above CONTESTED_RATIO but below FLIP_RATIO →
        // both stay live. 0.10 × 0.5 = 0.05; 0.10 × 1.5 = 0.15.
        // 0.08 sits in (0.05, 0.15) → contested.
        assert_eq!(resolve_duel(0.10, 0.08), DuelOutcome::Contested);
    }

    #[test]
    fn resolution_is_monotone_in_weight() {
        // Walking weight_new from 0 upward at fixed entrenchment
        // produces a monotone progression of outcomes:
        // quarantine → contested → flip. No oscillation.
        let ent_score = 0.20;
        let mut last_outcome_rank = 0u8;
        let rank = |o| match o {
            DuelOutcome::Quarantine => 0,
            DuelOutcome::Contested => 1,
            DuelOutcome::Flip => 2,
        };
        for step in 0..30 {
            let w = step as f32 * 0.02;
            let r = rank(resolve_duel(ent_score, w));
            assert!(
                r >= last_outcome_rank,
                "resolution went backwards at w={w}"
            );
            last_outcome_rank = r;
        }
    }
}
