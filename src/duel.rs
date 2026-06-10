#![allow(dead_code)]
// v1.5 calibration scaffolding: WEIGHT_CONFIRMATIONS / WEIGHT_EXTERNAL_SIGNAL
// stay declared because Phase 4 calibration sweeps grep them as the discoverable
// tunables. weight_new + diversity_weighted_count are the public formula surface
// downstream callers (relevance.rs ranking layer) wire in v1.6.

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

/// Slot weight applied to the diverse-confirmations term of
/// `weight_new`. Synthesis §6 single-user chat-only column anchor;
/// Phase 4 calibration will adjust per install mode.
///
/// **TODO(phase-4-calibration):** part of the install-mode weight
/// matrix in §6. Extracted from inline magic-number in the v1.4
/// post-critic round so calibration sweeps can grep DUEL_/WEIGHT_
/// and find every tunable in one shot.
pub const WEIGHT_CONFIRMATIONS: f32 = 0.1;

/// Slot weight applied to the external-signal term of `weight_new`.
/// Higher than confirmations because deterministic signals (cargo
/// test exit 0, CI green, etc.) are stronger evidence than
/// conversational repetition. Synthesis §5 axis 3.
pub const WEIGHT_EXTERNAL_SIGNAL: f32 = 0.2;

/// Multiplier applied to `weight_new` when the fact came from
/// memory rather than the live session. Synthesis §3 mechanism 3 —
/// inheritance discount. Should match `doubt::INHERITANCE_DISCOUNT_MULTIPLIER`
/// (the two modules implement complementary halves of the same
/// mechanism); the next calibration cycle should unify them under
/// one re-exported constant. For now both modules declare 0.5
/// independently; the duplicate is intentional and tracked.
pub const INHERITANCE_DISCOUNT: f32 = 0.5;

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

    /// External-signal count (v1.4 legacy slot): deterministic
    /// confirmations from tests, CI, code-search, etc. These weigh
    /// more than any conversational repetition (§5 axis 3). When
    /// v1.5 Phase 7 typed signals are present (see
    /// `external_signal_score`), they take precedence over this
    /// raw count.
    pub external_signals: u32,

    /// v1.5 Phase 7 step 7.2: pre-computed signed score from the
    /// typed `external_signals_v15` log. When `Some(_)`, weight_new
    /// uses this directly instead of the log2-shape applied to the
    /// raw count above. `None` falls back to v1.4 behaviour, which
    /// matters because Phase 1 migration writes the legacy count
    /// (`external_signals: u32`) but Phase 7's typed log is empty
    /// until users start posting `mind_outcome` calls.
    pub external_signal_score: Option<f32>,
}

impl Default for NewFactInputs {
    /// All-zero / no-signal default. Tests and call sites that only
    /// care about a subset of fields use `..NewFactInputs::default()`
    /// to populate the rest. The default represents "a fresh
    /// first-mention live fact with no diversity, no external signals,
    /// no typed-score" — the baseline F_new in §5.
    fn default() -> Self {
        Self {
            from_live_session: true,
            diverse_confirmations: 0,
            external_signals: 0,
            external_signal_score: None,
        }
    }
}

/// Compute the weight of a fresh fact for the duel using the
/// hardcoded `ChatOnly` slot weights. This is the v1.4 API kept
/// for callers that have not yet been threaded with an install
/// mode; v1.5+ call sites should prefer `weight_new_for_mode()`.
///
/// The slot weights are the **single-user chat-only default**
/// from synthesis §6:
/// - dependants weight 0.7 — irrelevant here because a fresh fact
///   has no dependants yet by construction.
/// - confirmations weight 0.1 — almost decorative.
/// - external-signal weight 0.2 — high quality when present.
///
/// For a fresh fact, the dependants-weight axis collapses to zero
/// (no dependants yet), so the duel weight is dominated by external
/// signals when present, with confirmations as a tiebreaker. The
/// inheritance discount halves the weight when applicable.
pub fn weight_new(inputs: NewFactInputs) -> f32 {
    weight_new_for_mode(inputs, crate::install_mode::InstallMode::ChatOnly)
}

/// v1.5 Phase 6 step 6.3: install-mode-aware weight_new.
///
/// Picks the `confirmations` and `external` slot weights from the
/// configured install mode (§6 synthesis) instead of the hardcoded
/// ChatOnly anchors. Same formula shape; only the multipliers shift.
///
/// Gate (v1.5 plan): when `mode = ChatOnly` this MUST equal the v1.4
/// `weight_new` output bit-for-bit. The contract is tested below.
pub fn weight_new_for_mode(inputs: NewFactInputs, mode: crate::install_mode::InstallMode) -> f32 {
    let weights = mode.weights();
    let conf_term = weights.confirmations * (1.0 + inputs.diverse_confirmations as f32).log2();

    // v1.5 Phase 7 step 7.2: prefer the typed-signal score when
    // present. The typed score is signed (failures pull negative)
    // and already incorporates per-type weights, so we just multiply
    // by the install-mode external slot weight. Falling back to the
    // legacy log2 shape on Phase 1-migrated facts keeps the count
    // useful until users adopt mind_outcome.
    let ext_term = match inputs.external_signal_score {
        Some(typed_score) => weights.external * typed_score,
        None => weights.external * (1.0 + inputs.external_signals as f32).log2(),
    };

    let raw = conf_term + ext_term;

    if inputs.from_live_session {
        raw
    } else {
        raw * INHERITANCE_DISCOUNT // synthesis §3 mechanism 3
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
    // v1.5 Phase 6 step 6.3: pick the install-mode-aware slot weights.
    // The mode lives in the runtime config, written by
    // `mgimind config install-mode <mode>`. Default ChatOnly keeps
    // legacy behaviour identical bit-for-bit (see test
    // chat_only_mode_matches_legacy_weight_new in this file).
    let w_new = weight_new_for_mode(new_inputs, config.install_mode);
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
    crate::knowledge::set_fact_payload_field(config, fact_id, "valid_until", now).await?;
    Ok(())
}

/// v1.7 (#111 follow-up): mark a TemporalSingle chain entry as superseded.
/// Like `dampen_loser` it sets `valid_until` and hides the fact from default
/// queries, but uses a distinct status so audit / history tools can tell
/// "natural end-of-life in a chain" from "lost a contradiction duel". The
/// distinction matters for explanation tools (mind_history) and for
/// users reasoning about why a fact disappeared from search.
pub async fn mark_superseded(config: &MindConfig, fact_id: &str) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    crate::knowledge::set_fact_payload_field(
        config,
        fact_id,
        "status",
        EntryStatus::Superseded.as_str().to_string(),
    )
    .await?;
    crate::knowledge::set_fact_payload_field(config, fact_id, "valid_until", now).await?;
    Ok(())
}

/// Retire the loser of a Flip by predicate cardinality — the single source of
/// truth for "which terminal status does a displaced fact get". Used by BOTH the
/// write path (`add_fact`, when a new value flips the old) and the consolidate
/// batch pass, so the card->action mapping lives in one place instead of being
/// re-derived at each call site. `Single` (and the unreachable `Multi`) dampen to
/// stale; `TemporalSingle` supersedes (kept as queryable history).
pub async fn retire_loser(
    config: &MindConfig,
    cardinality: crate::knowledge::Cardinality,
    fact_id: &str,
) -> Result<()> {
    match cardinality {
        crate::knowledge::Cardinality::TemporalSingle => mark_superseded(config, fact_id).await,
        // Single dampens; Multi never reaches a Flip (admits_conflict() == false),
        // so it can only arrive here through a misuse — treat it as a dampen
        // rather than panic, since this runs on the live write path.
        _ => dampen_loser(config, fact_id).await,
    }
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
            ..NewFactInputs::default()
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
        assert!(
            (two_years - five_years).abs() < 1e-4,
            "age past 2y should plateau"
        );
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
            assert!(r >= last_outcome_rank, "resolution went backwards at w={w}");
            last_outcome_rank = r;
        }
    }

    // --- v1.5 Phase 6 step 6.3: install-mode-aware weight_new ---

    /// Contract: weight_new (v1.4 API) must equal weight_new_for_mode(_,
    /// ChatOnly) bit-for-bit. Catches accidental drift if someone
    /// adjusts ChatOnly anchors but forgets the v1.4 surface stayed
    /// frozen against the original anchors.
    #[test]
    fn chat_only_mode_matches_legacy_weight_new() {
        use crate::install_mode::InstallMode;
        for from_live in [true, false] {
            for diverse in 0..6u32 {
                for ext in 0..6u32 {
                    let inputs = NewFactInputs {
                        from_live_session: from_live,
                        diverse_confirmations: diverse,
                        external_signals: ext,
                        ..NewFactInputs::default()
                    };
                    let legacy = weight_new(inputs);
                    let modal = weight_new_for_mode(inputs, InstallMode::ChatOnly);
                    assert_eq!(
                        legacy.to_bits(),
                        modal.to_bits(),
                        "ChatOnly mode must equal legacy weight_new at \
                         (from_live={from_live}, diverse={diverse}, ext={ext}): \
                         legacy={legacy} modal={modal}"
                    );
                }
            }
        }
    }

    /// DevWithCi mode emphasises external signal (anchor 0.35 vs
    /// ChatOnly's 0.2). For the same fresh fact with non-zero
    /// external signal count, DevWithCi must produce a strictly
    /// higher weight than ChatOnly. This is the v1.5 plan Step 6.3
    /// gate: "DevWithCi mode lifts external-signal-driven precision
    /// by ≥ 5pp" — at the formula level, that translates into a
    /// strictly larger weight when external signals are present.
    #[test]
    fn dev_with_ci_mode_lifts_external_signal_weight() {
        use crate::install_mode::InstallMode;
        let with_ext = NewFactInputs {
            from_live_session: true,
            diverse_confirmations: 0,
            external_signals: 5,
            ..NewFactInputs::default()
        };
        let chat = weight_new_for_mode(with_ext, InstallMode::ChatOnly);
        let dev = weight_new_for_mode(with_ext, InstallMode::DevWithCi);
        assert!(
            dev > chat,
            "DevWithCi must lift external-signal weight: chat={chat} dev={dev}"
        );
        // Sanity: lift must be at least 50% — DevWithCi external
        // anchor (0.35) is 1.75x ChatOnly's (0.2).
        assert!(
            dev > chat * 1.5,
            "DevWithCi lift too small: chat={chat} dev={dev}"
        );
    }

    /// MultiTenant mode emphasises confirmations (anchor 0.4 vs
    /// ChatOnly's 0.1). For the same fresh fact with non-zero
    /// confirmation count, MultiTenant must produce a strictly
    /// higher confirmation contribution than ChatOnly.
    #[test]
    fn multi_tenant_mode_lifts_confirmation_weight() {
        use crate::install_mode::InstallMode;
        let with_conf = NewFactInputs {
            from_live_session: true,
            diverse_confirmations: 5,
            external_signals: 0,
            ..NewFactInputs::default()
        };
        let chat = weight_new_for_mode(with_conf, InstallMode::ChatOnly);
        let multi = weight_new_for_mode(with_conf, InstallMode::MultiTenant);
        assert!(
            multi > chat,
            "MultiTenant must lift confirmation weight: chat={chat} multi={multi}"
        );
        // MultiTenant confirmation anchor (0.4) is 4x ChatOnly's
        // (0.1) — lift must be substantial.
        assert!(
            multi > chat * 3.0,
            "MultiTenant lift too small: chat={chat} multi={multi}"
        );
    }

    /// Inheritance discount applies uniformly across all modes —
    /// it's a §3 Mechanism 3 invariant, orthogonal to the §6
    /// per-mode weights.
    #[test]
    fn inheritance_discount_uniform_across_modes() {
        use crate::install_mode::InstallMode;
        for mode in InstallMode::ALL {
            let inputs = NewFactInputs {
                from_live_session: false,
                diverse_confirmations: 3,
                external_signals: 3,
                ..NewFactInputs::default()
            };
            let inherited = weight_new_for_mode(inputs, mode);
            let live = weight_new_for_mode(
                NewFactInputs {
                    from_live_session: true,
                    ..inputs
                },
                mode,
            );
            assert!(
                (inherited - live * INHERITANCE_DISCOUNT).abs() < 1e-5,
                "inheritance discount broken for {:?}: live={live} inherited={inherited}",
                mode
            );
        }
    }

    // --- v1.5 Phase 7 step 7.2: typed external_signal_score path ---

    /// When `external_signal_score: None`, the formula falls back to
    /// the v1.4 log2 shape on `external_signals: u32`. Catches
    /// accidental override of the fallback by stub callers.
    #[test]
    fn typed_score_none_falls_back_to_legacy_count() {
        use crate::install_mode::InstallMode;
        let inputs = NewFactInputs {
            from_live_session: true,
            diverse_confirmations: 0,
            external_signals: 3,
            external_signal_score: None,
        };
        let with_explicit_legacy = weight_new_for_mode(inputs, InstallMode::ChatOnly);

        // For external_signals=3, the legacy ext_term is
        // 0.2 * log2(1 + 3) = 0.2 * 2 = 0.4.
        let expected = 0.4;
        assert!(
            (with_explicit_legacy - expected).abs() < 1e-5,
            "legacy fallback broken: got {with_explicit_legacy}, expected {expected}"
        );
    }

    /// When typed score is positive, it replaces the log2 shape on
    /// `external_signals`. A score of 1.7 (= test_passed=1.0 +
    /// user_confirmed=0.7) under ChatOnly yields ext_term = 0.2 * 1.7
    /// = 0.34, irrespective of the legacy count.
    #[test]
    fn typed_score_some_replaces_legacy_count() {
        use crate::install_mode::InstallMode;
        let inputs = NewFactInputs {
            from_live_session: true,
            diverse_confirmations: 0,
            external_signals: 999, // ignored when typed score is Some
            external_signal_score: Some(1.7),
        };
        let w = weight_new_for_mode(inputs, InstallMode::ChatOnly);
        let expected = 0.2 * 1.7;
        assert!(
            (w - expected).abs() < 1e-5,
            "typed-score override broken: got {w}, expected {expected}"
        );
    }

    /// A failed test pulls the typed score negative; weight_new can
    /// then go negative too (which is correct — failed evidence is
    /// real evidence the claim is wrong).
    #[test]
    fn typed_score_negative_pulls_weight_negative() {
        use crate::install_mode::InstallMode;
        let inputs = NewFactInputs {
            from_live_session: true,
            diverse_confirmations: 0,
            external_signals: 0,
            external_signal_score: Some(-0.5),
        };
        let w = weight_new_for_mode(inputs, InstallMode::ChatOnly);
        assert!(
            w < 0.0,
            "negative typed score must produce negative weight, got {w}"
        );
    }

    /// Per-mode external slot weight applies to the typed score too.
    /// DevWithCi (0.35) lifts a positive typed score above ChatOnly (0.2).
    #[test]
    fn typed_score_respects_per_mode_external_weight() {
        use crate::install_mode::InstallMode;
        let inputs = NewFactInputs {
            from_live_session: true,
            diverse_confirmations: 0,
            external_signals: 0,
            external_signal_score: Some(1.0),
        };
        let chat = weight_new_for_mode(inputs, InstallMode::ChatOnly);
        let dev = weight_new_for_mode(inputs, InstallMode::DevWithCi);
        assert_eq!(chat, 0.2);
        assert_eq!(dev, 0.35);
        assert!(
            dev > chat,
            "DevWithCi external lift must apply to typed score"
        );
    }
}
