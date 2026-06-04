#![allow(dead_code)]
// Allowed-dead for the scaffold commit: the constants and pure helpers
// here are wired into the ranking layer (`relevance.rs`) and the
// background tokio task in subsequent commits on this same branch.
// Landing them together with their unit tests keeps the diff bisectable.

//! v1.4 Phase 3: doubt window + inheritance discount.
//!
//! Two anti-ossification mechanisms from synthesis §3 mechanisms 2 + 3:
//!
//! 1. **Doubt window** — entrenched facts must re-justify themselves
//!    on retrieval. If the surrounding context drifts away from the
//!    fact's origin context, the retrieval does NOT strengthen the
//!    fact. After N retrievals-without-confirmation, the fact's
//!    confidence is temporarily reduced.
//!
//! 2. **Active re-test (background)** — Mechanism 2's blind spot was
//!    that facts no longer retrieved never enter the doubt window.
//!    The background pass walks top-N entrenched-low-traffic facts
//!    and proactively checks context drift against the current memory
//!    centroid. Three hard guarantees (synthesis §10 question 5):
//!    never runs during an active MCP call, caps per-tick scan,
//!    adaptive cadence.
//!
//! 3. **Inheritance discount** — facts entering a session from memory
//!    (briefing, session_last) are flagged `inherited_unverified`.
//!    Flagged facts cannot co-confirm each other (one source agreeing
//!    with itself ≠ two confirmations). Flag clears at first
//!    in-session confirmation.
//!
//! **TODO(phase-4-calibration):** every constant here is illustrative.
//! Phase 4 sweeps against the real distribution from Phase 1.1 +
//! LongMemEval-S regression + STALE behavioural metrics.

// ===== Doubt window constants (Phase 4 calibration targets) =====

/// Number of "surfaced but not confirmed" retrievals before a fact's
/// confidence is temporarily reduced. Mechanism 2 entry condition.
///
/// **TODO(phase-4-calibration):** synthesis §10 question 4 — fixed N
/// vs scaled by entrenchment.
pub const DOUBT_WINDOW_N_RETRIEVALS: u32 = 5;

/// Cosine distance threshold above which a retrieval's context counts
/// as "drifted" from the fact's origin context. Drifted retrievals
/// don't strengthen the fact and count toward the doubt window.
///
/// **TODO(phase-4-calibration):** 0.4 is a starting point in MiniLM
/// space. Phase 4 sweeps against precision on real conflicts.
pub const DOUBT_DRIFT_THRESHOLD: f32 = 0.4;

/// Multiplier applied to a fact's confidence while it sits in the
/// doubt window. 0.5 means a doubted fact is half-weighted in
/// ranking until it earns a fresh in-context confirmation.
pub const DOUBT_CONFIDENCE_MULTIPLIER: f32 = 0.5;

// ===== Background re-test cadence (synthesis §10 question 5) =====

/// Per-tick cap on facts scanned by the background re-test pass.
/// Bounds the worst-case work per cadence interval; the walk is
/// amortised across many ticks rather than done in one breath.
pub const BACKGROUND_PER_TICK_CAP: usize = 50;

/// Default cadence between background passes when nothing changes.
/// Starts at one hour; adaptive cadence in `background_cadence_next`
/// expands toward 24h on quiet graphs and contracts toward 5min on
/// busy ones.
pub const BACKGROUND_CADENCE_DEFAULT_SECONDS: u64 = 3600;

pub const BACKGROUND_CADENCE_MAX_SECONDS: u64 = 86_400; // 24 hours
pub const BACKGROUND_CADENCE_MIN_SECONDS: u64 = 300; // 5 minutes

// ===== Inheritance discount =====

/// Multiplier applied to an inherited fact's weight in any duel.
/// Synthesis §3 mechanism 3. 1.0 = full weight (live in-session);
/// 0.5 means the inherited fact contributes at half the weight a
/// live observation would.
pub const INHERITANCE_DISCOUNT_MULTIPLIER: f32 = 0.5;

// ===== Pure helpers — context drift check =====

/// Cosine-distance check between a fact's stored origin context vector
/// and the centroid of the recently active memory set.
///
/// Returns `true` when the drift exceeds the threshold — the retrieval
/// or background pass should treat the fact as "surfaced but not
/// confirmed" rather than strengthening it.
///
/// Inputs are pre-normalised vectors (the embedder L2-normalises by
/// default), so the dot product is the cosine similarity. Drift =
/// 1.0 - cosine_similarity.
pub fn is_context_drifted(
    origin_context_vec: &[f32],
    current_centroid_vec: &[f32],
    threshold: f32,
) -> bool {
    if origin_context_vec.is_empty() || current_centroid_vec.is_empty() {
        return false; // no signal yet — don't penalise
    }
    if origin_context_vec.len() != current_centroid_vec.len() {
        return false; // dimension mismatch — bail to safe default
    }
    let cos: f32 = origin_context_vec
        .iter()
        .zip(current_centroid_vec.iter())
        .map(|(a, b)| a * b)
        .sum();
    let drift = 1.0 - cos;
    drift > threshold
}

/// Compute the centroid (mean) of a slice of L2-normalised vectors.
/// Returns an empty vector when the input is empty so callers can
/// detect "no signal yet".
///
/// Used by the background re-test pass to summarise recent activity
/// against which each entrenched fact's origin context is checked.
pub fn centroid(vectors: &[Vec<f32>]) -> Vec<f32> {
    if vectors.is_empty() {
        return Vec::new();
    }
    let dim = vectors[0].len();
    let mut sum = vec![0.0f32; dim];
    for v in vectors {
        if v.len() != dim {
            continue;
        }
        for (i, x) in v.iter().enumerate() {
            sum[i] += x;
        }
    }
    let n = vectors.len() as f32;
    for x in &mut sum {
        *x /= n;
    }
    // Re-normalise so the centroid is comparable to L2-normalised
    // vectors via dot product.
    let norm: f32 = sum.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut sum {
            *x /= norm;
        }
    }
    sum
}

// ===== Pure helpers — adaptive background cadence =====

/// Compute the next cadence interval based on graph activity.
///
/// `edits_since_last_pass`: count of dependant-graph changes (fact
/// adds, fact dampenings, dependant_count updates) observed since the
/// previous background pass fired.
///
/// Returns seconds until the next pass should run, bounded by
/// `BACKGROUND_CADENCE_MIN_SECONDS` (5 min) and
/// `BACKGROUND_CADENCE_MAX_SECONDS` (24 h). The rule of thumb is:
/// "how stale could the cache plausibly be?" — a quiet graph rarely
/// invalidates cache, so we slow down; a busy graph invalidates
/// often, so we speed up.
///
/// **TODO(phase-4-calibration):** the exact thresholds for "busy" vs
/// "quiet" are placeholders; Phase 4 picks them against the author's
/// real edit rate.
pub fn background_cadence_next(
    previous_seconds: u64,
    edits_since_last_pass: usize,
) -> u64 {
    if edits_since_last_pass == 0 {
        // Quiet → double the cadence, up to the cap.
        (previous_seconds.saturating_mul(2)).min(BACKGROUND_CADENCE_MAX_SECONDS)
    } else if edits_since_last_pass >= 20 {
        // Busy → halve the cadence, down to the floor.
        (previous_seconds / 2).max(BACKGROUND_CADENCE_MIN_SECONDS)
    } else {
        // Moderate → hold steady at the default if we drifted away.
        previous_seconds.clamp(BACKGROUND_CADENCE_MIN_SECONDS, BACKGROUND_CADENCE_MAX_SECONDS)
    }
}

// ===== Pure helpers — doubt window state machine =====

/// Apply one retrieval event to a fact's doubt-window counter.
///
/// `prior_count`: how many "surfaced but not confirmed" retrievals had
/// already accumulated when this retrieval happened.
/// `drifted`: did this retrieval's context drift exceed the threshold?
///
/// Returns the new counter and whether the fact has entered the doubt
/// window as a result.
pub fn apply_retrieval_event(prior_count: u32, drifted: bool) -> (u32, DoubtState) {
    if !drifted {
        // Context matches origin → strengthen (counter resets).
        return (0, DoubtState::Outside);
    }
    let new_count = prior_count.saturating_add(1);
    if new_count >= DOUBT_WINDOW_N_RETRIEVALS {
        (new_count, DoubtState::Inside)
    } else {
        (new_count, DoubtState::Outside)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoubtState {
    /// Fact has not yet entered the doubt window (or has been
    /// re-confirmed). Full confidence in ranking.
    Outside,
    /// Fact is in the doubt window — confidence multiplier applies.
    /// Earns its way out by a fresh non-drifted retrieval (which
    /// resets the counter to 0).
    Inside,
}

impl DoubtState {
    /// Multiplier applied to the fact's confidence when ranking. Phase
    /// 4 ranking layer multiplies the cached `confidence_score` by
    /// this value before final ordering.
    pub fn confidence_multiplier(self) -> f32 {
        match self {
            DoubtState::Outside => 1.0,
            DoubtState::Inside => DOUBT_CONFIDENCE_MULTIPLIER,
        }
    }
}

// ===== Tests =====

#[cfg(test)]
mod tests {
    use super::*;

    // --- Context drift ---

    #[test]
    fn identical_vectors_do_not_drift() {
        let v = vec![0.6, 0.8, 0.0];
        assert!(!is_context_drifted(&v, &v, 0.4));
    }

    #[test]
    fn orthogonal_vectors_drift() {
        // Dot product = 0 → cosine = 0 → drift = 1.0 > 0.4
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        assert!(is_context_drifted(&a, &b, 0.4));
    }

    #[test]
    fn opposite_vectors_strongly_drift() {
        // Cosine = -1 → drift = 2.0 → clearly above threshold
        let a = vec![1.0, 0.0];
        let b = vec![-1.0, 0.0];
        assert!(is_context_drifted(&a, &b, 0.4));
    }

    #[test]
    fn empty_vectors_do_not_penalise() {
        // No signal yet — return false (don't strengthen, but also
        // don't punish). The caller decides whether to skip the fact
        // entirely or treat it as "no info."
        let v = vec![1.0, 0.0];
        assert!(!is_context_drifted(&[], &v, 0.4));
        assert!(!is_context_drifted(&v, &[], 0.4));
        assert!(!is_context_drifted(&[], &[], 0.4));
    }

    #[test]
    fn dimension_mismatch_does_not_panic_or_penalise() {
        // Defensive: somehow we got vectors of different sizes (model
        // change between writes?). The check must not panic — it
        // must fall back to "not drifted" so the fact gets the
        // benefit of the doubt and is re-checked next pass.
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0];
        assert!(!is_context_drifted(&a, &b, 0.4));
    }

    // --- Centroid ---

    #[test]
    fn centroid_of_one_vector_is_that_vector() {
        let v = vec![0.6, 0.8, 0.0];
        let c = centroid(&[v.clone()]);
        // After re-normalisation; v is already L2-normalised so the
        // centroid is identical.
        for (a, b) in v.iter().zip(c.iter()) {
            assert!((a - b).abs() < 1e-5);
        }
    }

    #[test]
    fn centroid_of_orthogonal_pair_is_diagonal() {
        // (1,0) and (0,1) → mean (0.5, 0.5) → normalised (0.707, 0.707)
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        let c = centroid(&[a, b]);
        let expected = 1.0 / 2.0f32.sqrt();
        assert!((c[0] - expected).abs() < 1e-5);
        assert!((c[1] - expected).abs() < 1e-5);
    }

    #[test]
    fn centroid_of_empty_is_empty() {
        assert!(centroid(&[]).is_empty());
    }

    // --- Adaptive background cadence ---

    #[test]
    fn cadence_doubles_when_graph_quiet() {
        let next = background_cadence_next(3600, 0);
        assert_eq!(next, 7200);
    }

    #[test]
    fn cadence_caps_at_24_hours() {
        let next = background_cadence_next(80_000, 0);
        assert_eq!(next, BACKGROUND_CADENCE_MAX_SECONDS);
    }

    #[test]
    fn cadence_halves_when_graph_busy() {
        let next = background_cadence_next(3600, 25);
        assert_eq!(next, 1800);
    }

    #[test]
    fn cadence_floors_at_5_minutes() {
        let next = background_cadence_next(200, 25);
        assert_eq!(next, BACKGROUND_CADENCE_MIN_SECONDS);
    }

    #[test]
    fn cadence_holds_when_moderate_edits() {
        // Between 0 and 20 edits → stay near current cadence (clamped).
        let next = background_cadence_next(3600, 5);
        assert_eq!(next, 3600);
    }

    // --- Doubt window state machine ---

    #[test]
    fn non_drifted_retrieval_resets_counter() {
        // Counter at 3, fresh confirming retrieval → reset to 0,
        // outside the window.
        let (n, state) = apply_retrieval_event(3, false);
        assert_eq!(n, 0);
        assert_eq!(state, DoubtState::Outside);
    }

    #[test]
    fn drifted_retrieval_increments_counter() {
        let (n, state) = apply_retrieval_event(2, true);
        assert_eq!(n, 3);
        assert_eq!(state, DoubtState::Outside); // still below N=5
    }

    #[test]
    fn enough_drifted_retrievals_enter_window() {
        // From prior 4, one more drifted retrieval crosses N=5.
        let (n, state) = apply_retrieval_event(4, true);
        assert_eq!(n, 5);
        assert_eq!(state, DoubtState::Inside);
    }

    #[test]
    fn doubt_state_multiplier_matches_constant() {
        assert!((DoubtState::Outside.confidence_multiplier() - 1.0).abs() < 1e-6);
        assert!(
            (DoubtState::Inside.confidence_multiplier() - DOUBT_CONFIDENCE_MULTIPLIER).abs() < 1e-6
        );
    }

    #[test]
    fn saturating_add_protects_against_overflow() {
        // Pathologically high prior_count should saturate, not wrap.
        let (n, _) = apply_retrieval_event(u32::MAX, true);
        assert_eq!(n, u32::MAX);
    }
}
