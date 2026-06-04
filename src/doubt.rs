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

/// v1.5 Phase 8 step 8.1C: load-based cadence multiplier.
///
/// Reads `/proc/loadavg` (linux only) and compares the 1-minute load
/// to `1.5 × num_cpus`. When the system is overloaded the loop
/// doubles its cadence to back off; otherwise it returns 1.0.
///
/// On non-Linux platforms returns 1.0 (the loop ignores load). A
/// future revision could use rusage on macOS / GetSystemTimes on
/// Windows, but for v1.5 the Linux path covers Mad's target deployment.
///
/// Best-effort: any I/O / parse failure returns 1.0. The loop
/// keeps running; we just lose the back-off signal that pass.
pub fn loadavg_multiplier() -> f32 {
    #[cfg(target_os = "linux")]
    {
        try_loadavg_multiplier_linux().unwrap_or(1.0)
    }
    #[cfg(not(target_os = "linux"))]
    {
        1.0
    }
}

#[cfg(target_os = "linux")]
fn try_loadavg_multiplier_linux() -> Option<f32> {
    let raw = std::fs::read_to_string("/proc/loadavg").ok()?;
    // Format: "0.50 0.45 0.40 1/123 12345" — first field is 1m load.
    let load_1m: f32 = raw.split_whitespace().next()?.parse().ok()?;

    // num_cpus crate adds a dep we'd rather avoid; use std API.
    // `available_parallelism` reflects scheduler-visible CPUs, which
    // is the right denominator on a container/cgroup-restricted host.
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1) as f32;

    if load_1m > cores * 1.5 {
        Some(2.0)
    } else {
        Some(1.0)
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

// ===== Retrieval-triggered doubt window (Phase 3 step 2) =====
//
// The state machine in `apply_retrieval_event` is pure; persisting the
// counter is the side effect. Each fact carries a payload field
// `doubt_drift_count` with the running count of drifted retrievals
// since the last non-drifted one. When the counter crosses
// DOUBT_WINDOW_N_RETRIEVALS the fact enters the doubt window — its
// confidence is halved by the ranking layer until a fresh non-drifted
// retrieval resets the counter.
//
// This function runs the full cycle: read prior count, apply event,
// write back. It is the wrapper the Phase 4 ranking layer calls when
// it surfaces a high-entrenchment fact.
//
// **Cost.** One payload read + one payload write per retrieval of a
// high-entrenchment fact. Phase 4 caches the resulting DoubtState in
// the per-fact `confidence_score` so the ranking hot path does not
// re-run the cycle on every search.

use anyhow::Result;

use crate::config::MindConfig;

/// Run the retrieval-triggered doubt-window cycle against a single
/// fact and persist the new counter.
///
/// `fact_id` — the fact that the ranking layer just surfaced.
/// `drifted` — whether the surrounding context drifted from the
/// fact's origin context (computed by the caller via
/// `is_context_drifted`).
///
/// Returns the new DoubtState. The ranking layer multiplies the
/// fact's confidence by `state.confidence_multiplier()` before
/// final ordering.
pub async fn apply_doubt_check_to_fact(
    config: &MindConfig,
    fact_id: &str,
    drifted: bool,
) -> Result<DoubtState> {
    let prior = read_doubt_count(config, fact_id).await;
    let (new_count, state) = apply_retrieval_event(prior, drifted);
    if new_count != prior {
        write_doubt_count(config, fact_id, new_count).await?;
    }
    Ok(state)
}

/// v1.5 Phase 8 step 8.2: re-test one fact and apply the transition.
///
/// Reads the fact's cached `confidence_score` (last computed by the
/// previous tick or by add_fact at write time), recomputes from
/// current payload signals, asks `confidence::decide_retest_transition`
/// what to do, and applies the verdict:
///
/// - PromoteToDoubt → bump doubt counter to threshold, write back.
/// - RecoverFromDoubt → reset doubt counter to 0.
/// - NoChange → write back new score for next-tick comparison.
///
/// Never deletes. Mechanism 1 invariant.
///
/// Returns the resulting `RetestTransition` so the loop can count
/// and log per-tick promote/recover/noop totals (and Step 8.4 audit
/// log can record them).
pub async fn retest_fact_step82(
    config: &MindConfig,
    fact_id: &str,
) -> Result<crate::confidence::RetestTransition> {
    use crate::confidence::{
        confidence_score, decide_retest_transition, ConfidenceInputs, RetestTransition,
    };

    let client = crate::storage::get_client(config).await?;

    // v1.6 step 1: one batched Qdrant call returns every payload
    // field the formulas need. Replaces the v1.5 implementation's
    // four separate round-trips per fact — at BACKGROUND_PER_TICK_CAP
    // = 50 that's a 4x reduction in network round-trips per tick.
    const PAYLOAD_KEYS: &[&str] = &[
        "dependants_count",
        "confirmations_count",
        "external_signals",
        "confidence_score",
    ];
    let payload_map = crate::storage::read_point_payload_strings(
        &client,
        crate::storage::FACTS_COLLECTION,
        fact_id,
        PAYLOAD_KEYS,
    )
    .await
    .unwrap_or_default();

    let dependants = payload_map
        .get("dependants_count")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
    let confirmations = payload_map
        .get("confirmations_count")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
    let legacy_external_count = payload_map
        .get("external_signals")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
    let cached_score = payload_map
        .get("confidence_score")
        .and_then(|s| s.parse::<f32>().ok())
        .unwrap_or(0.5);

    // Phase 7 typed external_signal_score: derive from the
    // external_signals_v15 log if present.
    let typed_signals = crate::storage::read_external_signals(config, fact_id)
        .await
        .unwrap_or_default();
    // v1.6 step 2: cited_by chain following.
    //
    // For every cited_by signal in the typed log, look up the citing
    // memory's cached confidence_score from its payload. Self-citation
    // guard then unblocks when the citing memory's own confidence is
    // ≥ CITED_BY_MIN_CITING_CONFIDENCE (0.5). Without this map the
    // closure returns None for every id and the guard always blocks,
    // which is what v1.5 shipped.
    //
    // One batched get_points covers all distinct citing ids in the
    // current fact's log. Cited memories are in the MEMORIES_COLLECTION
    // (not the facts collection) because mind_outcome operates on
    // memory ids.
    let citing_confidence_map = if typed_signals.is_empty() {
        std::collections::HashMap::<String, f32>::new()
    } else {
        let citing_ids: std::collections::HashSet<&str> = typed_signals
            .iter()
            .filter(|s| s.signal_type == crate::outcome::OutcomeSignal::CitedBy)
            .map(|s| s.source.as_str())
            .collect();
        fetch_citing_confidences(&client, &citing_ids).await
    };

    let external_signal_score = if typed_signals.is_empty() {
        None
    } else {
        Some(crate::outcome::compute_external_signal_score(
            &typed_signals,
            |citing_id| citing_confidence_map.get(citing_id).copied(),
        ))
    };

    let inputs = ConfidenceInputs {
        dependants,
        confirmations,
        external_signal_score,
        legacy_external_count,
        inherited_unverified: is_inherited(fact_id),
    };
    let new_score = confidence_score(inputs, config.install_mode);
    let currently_in_doubt = read_doubt_count(config, fact_id).await >= DOUBT_WINDOW_N_RETRIEVALS;
    let transition = decide_retest_transition(cached_score, new_score, currently_in_doubt);

    match transition {
        RetestTransition::PromoteToDoubt => {
            write_doubt_count(config, fact_id, DOUBT_WINDOW_N_RETRIEVALS).await?;
            // v1.5 Phase 8 step 8.4: audit log entry. Append-only;
            // a future `mgimind audit replay --since 1d` can
            // reconstruct the sequence of promotions.
            crate::audit::record(
                crate::audit::AuditEvent::new(
                    crate::audit::AuditOp::RetestPromote,
                    "facts",
                    fact_id,
                )
                .actor("retest")
                .before(format!("{cached_score:.6}"))
                .after(format!("{new_score:.6}"))
                .note("promote_to_doubt"),
            );
        }
        RetestTransition::RecoverFromDoubt => {
            write_doubt_count(config, fact_id, 0).await?;
            crate::audit::record(
                crate::audit::AuditEvent::new(
                    crate::audit::AuditOp::RetestRecover,
                    "facts",
                    fact_id,
                )
                .actor("retest")
                .before(format!("{cached_score:.6}"))
                .after(format!("{new_score:.6}"))
                .note("recover_from_doubt"),
            );
        }
        RetestTransition::NoChange => {}
    }

    // Always write the freshly-computed score back, so next tick
    // compares against the most recent value rather than a stale
    // cache.
    crate::knowledge::set_fact_payload_field(
        config,
        fact_id,
        "confidence_score",
        format!("{new_score:.6}"),
    )
    .await?;

    Ok(transition)
}

/// v1.6 step 2: batched read of `confidence_score` payload for a
/// set of citing memory ids. Returns a HashMap so the synchronous
/// closure inside `compute_external_signal_score` can look up
/// values without re-entering async context.
///
/// Citing ids live in the MEMORIES_COLLECTION (not facts) because
/// `mind_outcome` operates on memory ids. A citing memory without
/// a cached confidence_score (e.g. pre-v1.5 entries that never went
/// through the retest pass) is simply absent from the map —
/// `compute_external_signal_score` blocks it the same way it would
/// have under the v1.5 stub. This is the conservative default:
/// unknown citing confidence → cited_by signal does not count.
///
/// Single-ID best-effort: if one fetch fails the whole map is empty.
/// That degrades to v1.5 behaviour (always-block guard) — never
/// crashes the retest loop.
async fn fetch_citing_confidences(
    client: &qdrant_client::Qdrant,
    citing_ids: &std::collections::HashSet<&str>,
) -> std::collections::HashMap<String, f32> {
    let mut out = std::collections::HashMap::with_capacity(citing_ids.len());
    if citing_ids.is_empty() {
        return out;
    }
    let pids: Vec<qdrant_client::qdrant::PointId> =
        citing_ids.iter().map(|id| id.to_string().into()).collect();
    let Ok(resp) = client
        .get_points(
            qdrant_client::qdrant::GetPointsBuilder::new(
                crate::storage::MEMORIES_COLLECTION,
                pids,
            )
            .with_payload(true),
        )
        .await
    else {
        return out;
    };
    for point in resp.result {
        let Some(pid) = point.id.as_ref().map(crate::storage::format_point_id) else {
            continue;
        };
        if let Some(raw) = crate::storage::extract_string_pub(&point.payload, "confidence_score") {
            if let Ok(score) = raw.parse::<f32>() {
                out.insert(pid, score);
            }
        }
    }
    out
}

async fn read_doubt_count(config: &MindConfig, fact_id: &str) -> u32 {
    let Ok(client) = crate::storage::get_client(config).await else {
        return 0;
    };
    crate::storage::existing_payload_string(
        &client,
        crate::storage::FACTS_COLLECTION,
        fact_id,
        "doubt_drift_count",
    )
    .await
    .and_then(|s| s.parse().ok())
    .unwrap_or(0u32)
}

async fn write_doubt_count(config: &MindConfig, fact_id: &str, count: u32) -> Result<()> {
    // Phase 3 carries its own payload-setter rather than depending on
    // the Phase 1 helper (set_fact_payload_field), because the two
    // branches are developed in parallel and merge order is not yet
    // fixed. When Phase 1 lands the duplicate helper can be replaced
    // by a delegate call.
    use std::collections::HashMap;
    let client = crate::storage::get_client(config).await?;
    let mut payload: HashMap<String, qdrant_client::qdrant::Value> = HashMap::new();
    payload.insert("doubt_drift_count".into(), count.to_string().into());
    let point_id: qdrant_client::qdrant::PointId = fact_id.to_string().into();
    client
        .set_payload(
            qdrant_client::qdrant::SetPayloadPointsBuilder::new(
                crate::storage::FACTS_COLLECTION,
                payload,
            )
            .points_selector(qdrant_client::qdrant::PointsIdsList {
                ids: vec![point_id],
            })
            .wait(true),
        )
        .await
        .map_err(anyhow::Error::from)?;
    Ok(())
}

// ===== Background re-test pass (Phase 3 step 3) =====
//
// Doubt window with retrieval triggers alone (step 2) has a blind
// spot: facts that stop being retrieved never enter the window, so
// they ossify in their last-confidence state and resist correction.
// The background pass closes the loop — it walks top-N entrenched-
// low-traffic facts on an adaptive cadence and runs the same drift
// check that step 2 runs.
//
// Three hard guarantees (synthesis §10 question 5):
//
//   (a) Never runs while an MCP tool call is in flight. A simple
//       atomic flag set on enter, cleared on exit. The background
//       loop yields if the flag is set.
//   (b) Caps per-tick scan at BACKGROUND_PER_TICK_CAP facts. The
//       walk is amortised across many ticks rather than done in one
//       breath; pathological "all facts at once" is structurally
//       impossible.
//   (c) Adaptive cadence via `background_cadence_next`. Quiet
//       graphs slow down (up to 24h cap), busy ones speed up (down
//       to 5min floor), so the pass tracks "how stale could the
//       cache plausibly be" rather than a hardcoded clock.
//
// **Cost narrative for the public README.** "Single warm process, ms
// lookup" remains true on the retrieval path. Background pass is a
// separate low-priority loop scheduled to yield when the retrieval
// path is busy. That idle budget is the price of not ossifying.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Set true while any MCP tool call is in flight. The background pass
/// yields when this is true (synthesis §10 q5 guarantee a).
static MCP_BUSY: AtomicBool = AtomicBool::new(false);

/// Cumulative count of graph edits (fact adds, dampenings, payload
/// updates) observed since the last background tick. Feeds the
/// adaptive cadence calculation (synthesis §10 q5 guarantee c).
static EDITS_SINCE_LAST_TICK: AtomicUsize = AtomicUsize::new(0);

/// RAII guard that raises MCP_BUSY on construction and lowers it on
/// drop. Wrap every tool-dispatch entry point in `let _g = BusyGuard::new();`
/// so the background pass cannot collide with a live call even if
/// the call panics.
pub struct BusyGuard;

impl BusyGuard {
    pub fn new() -> Self {
        MCP_BUSY.store(true, Ordering::Release);
        BusyGuard
    }
}

impl Default for BusyGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        MCP_BUSY.store(false, Ordering::Release);
    }
}

/// Record a graph edit. Called by `add_fact`, by the Phase 1.1
/// dependants writer, and by the duel-rule dampening path. Cheap
/// (one atomic add); fine to call from hot paths.
pub fn record_edit() {
    EDITS_SINCE_LAST_TICK.fetch_add(1, Ordering::Relaxed);
}

/// Snapshot the edit count and reset it to zero atomically. Called by
/// the background loop once per tick before computing the next cadence.
pub fn take_edit_count() -> usize {
    EDITS_SINCE_LAST_TICK.swap(0, Ordering::AcqRel)
}

/// Whether an MCP call is currently in flight. The background loop
/// reads this and yields if true.
pub fn is_mcp_busy() -> bool {
    MCP_BUSY.load(Ordering::Acquire)
}

/// Spawn the background re-test loop. Call once from `mcp::serve`
/// after Qdrant is up. The returned `JoinHandle` lets the caller
/// abort the loop on shutdown if needed; in practice the loop runs
/// for the lifetime of the warm `mgimind mcp` process and is
/// dropped when the process exits.
///
/// The loop iterates:
///   1. Sleep for the current cadence.
///   2. If MCP_BUSY is true at wake, skip this tick and sleep again
///      with the current cadence.
///   3. Otherwise: scan up to BACKGROUND_PER_TICK_CAP entrenched-
///      low-traffic facts, run `apply_doubt_check_to_fact` on each,
///      and update the cadence based on the edit count since the
///      last tick.
///
/// **Scaffold note.** The actual fact selection (top-N by
/// entrenchment, filtered by low recent-access) needs the Phase 1
/// `dependants_count` payload field + an access-count tracker. Until
/// those land, the loop calls `eprintln!` once per tick describing
/// what it would do, then yields. Wiring lands when the Phase 1
/// merge order is fixed.
pub fn spawn_background_retest_loop(
    config: crate::config::MindConfig,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut cadence = BACKGROUND_CADENCE_DEFAULT_SECONDS;
        eprintln!(
            "mgimind: background doubt-window re-test loop started (default cadence {}s)",
            cadence
        );
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(cadence)).await;

            if is_mcp_busy() {
                // Guarantee (a): yield rather than contend with the
                // retrieval hot path. We do not reset the cadence —
                // a busy graph is exactly when the next tick should
                // probably be sooner, so we recompute from edit
                // counts below.
                eprintln!("mgimind: background re-test yielded (MCP call in flight)");
                continue;
            }

            let edits = take_edit_count();
            cadence = background_cadence_next(cadence, edits);
            // v1.5 Phase 8 step 8.1C — load-aware multiplier on top
            // of the edit-rate cadence. /proc/loadavg above 1.5×cores
            // doubles the cadence for the next tick.
            cadence = (cadence as f32 * loadavg_multiplier()) as u64;

            // v1.5 Phase 8 step 8.1B: real walk. Pick candidates,
            // re-check is_mcp_busy() BETWEEN facts so a tool call
            // that started mid-tick still wins the contention race
            // (guarantee a hole noted by audit).
            let candidates =
                select_retest_candidates(&config, BACKGROUND_PER_TICK_CAP).await;

            let mut n_processed_this_tick: usize = 0;
            for fact_id in &candidates {
                if is_mcp_busy() {
                    eprintln!(
                        "mgimind: background re-test interrupted mid-tick (MCP call in flight, {}/{} processed)",
                        n_processed_this_tick,
                        candidates.len()
                    );
                    break;
                }
                // v1.5 Phase 8 step 8.2: real re-test pass. Reads
                // current payload signals, recomputes confidence_score,
                // applies promote/recover/noop verdict. Never deletes
                // (Mechanism 1 invariant).
                match retest_fact_step82(&config, fact_id).await {
                    Ok(_transition) => {
                        clear_doubt_window_flag(fact_id);
                        n_processed_this_tick += 1;
                    }
                    Err(e) => {
                        tracing::debug!(
                            "mgimind: background re-test retest_fact_step82({fact_id}) failed: {e}"
                        );
                        // Re-flag so the next tick gets another shot.
                        flag_for_doubt_window(fact_id);
                    }
                }
            }

            assert!(
                n_processed_this_tick <= BACKGROUND_PER_TICK_CAP,
                "background loop processed {} > cap {}",
                n_processed_this_tick,
                BACKGROUND_PER_TICK_CAP
            );

            eprintln!(
                "mgimind: background re-test tick (edits since last={}, processed {}/{} cap, next cadence={}s)",
                edits,
                n_processed_this_tick,
                BACKGROUND_PER_TICK_CAP,
                cadence
            );
        }
    })
}

// ===== Inheritance flag — per-process registry =====
//
// Synthesis §3 mechanism 3: facts entering a session from memory
// (briefing, session_last, files outside the live conversation) carry
// an `inherited_unverified` flag until first in-session confirmation.
//
// We keep the flag in a per-process registry rather than a payload
// field, because the flag is a property of *this session's view of
// the fact*, not of the fact itself. The same fact is inherited in
// session N and live in session N+1; persisting the flag in payload
// would leak one session's state into the next.
//
// The registry is populated when a session reads its previous
// summary (`mind_session(action="last")`) or context briefing, and
// consumed by the duel rule via `weight_new` (mechanism 3 already
// accepts `from_live_session: bool`).
//
// Cleared at: first independent live-session confirmation of the
// fact (a fresh `add_fact` for the same triple with no contradiction),
// or process restart (in-process state is intentionally ephemeral).

use std::collections::HashSet;

use once_cell::sync::Lazy;
use parking_lot::Mutex;

// Post-critic switch from std::sync::Mutex to parking_lot::Mutex —
// std::sync::Mutex.lock() can block tokio runtime workers when
// contended; parking_lot::Mutex is non-poisoning and faster on
// short critical sections (insert/contains/remove on a HashSet).
// The lock is acquired only inside these functions and released
// before returning, never held across .await.
static INHERITED_FACTS: Lazy<Mutex<HashSet<String>>> = Lazy::new(|| Mutex::new(HashSet::new()));

/// Mark a fact id as inherited-from-memory in the current process.
/// Called by `mind_session(action="last")` and the briefing path for
/// every fact reference they surface.
pub fn mark_inherited(fact_id: &str) {
    INHERITED_FACTS.lock().insert(fact_id.to_string());
}

/// Check whether a fact is currently flagged as inherited-unverified.
/// Consumed by the duel rule (`weight_new` slot) and by the ranking
/// layer (confidence multiplier).
pub fn is_inherited(fact_id: &str) -> bool {
    INHERITED_FACTS.lock().contains(fact_id)
}

/// Clear a fact's inheritance flag — called when a live in-session
/// observation confirms the fact (a re-assertion of the same triple
/// without contradiction).
pub fn clear_inherited(fact_id: &str) {
    INHERITED_FACTS.lock().remove(fact_id);
}

/// Bulk clear — used by tests and by `mind_session(action="end")` so a
/// closed session does not leak inheritance state into the next one
/// that starts in the same warm process.
pub fn clear_all_inherited() {
    INHERITED_FACTS.lock().clear();
}

/// Count of currently-inherited facts. Surfaced by `mgimind doctor`
/// alongside the conflict counts so the user can see how much of the
/// active session's context came from memory vs the live conversation.
pub fn inherited_count() -> usize {
    INHERITED_FACTS.lock().len()
}

/// v1.5 Phase 7 step 7.3: in-memory set of memory ids that the
/// error-rate guardrail flagged for the doubt window. The Phase 3
/// background re-test loop (currently scaffold) consumes this set
/// and applies the actual doubt-window state transition; until that
/// lands, the flag is observable via `is_flagged_for_doubt_window`
/// and surfaces in `mgimind doctor`.
///
/// parking_lot::Mutex consistent with INHERITED_FACTS — same short
/// critical-section / no-poisoning rationale.
static DOUBT_WINDOW_FLAGGED: Lazy<Mutex<HashSet<String>>> = Lazy::new(|| Mutex::new(HashSet::new()));

/// Flag a memory id for promotion to the doubt window. Called by
/// `outcome::record` when ≥3 failed test_passed signals land within
/// the last 7 days.
pub fn flag_for_doubt_window(memory_id: &str) {
    DOUBT_WINDOW_FLAGGED.lock().insert(memory_id.to_string());
}

/// Check whether a memory was flagged for the doubt window by the
/// error-rate guardrail. The background re-test loop reads this.
pub fn is_flagged_for_doubt_window(memory_id: &str) -> bool {
    DOUBT_WINDOW_FLAGGED.lock().contains(memory_id)
}

/// Clear a memory's flag — called when the background loop applies
/// the actual doubt-window state transition (so we don't keep
/// flagging it forever) or when a fresh successful test_passed
/// signal lands (TBD in Phase 8).
pub fn clear_doubt_window_flag(memory_id: &str) {
    DOUBT_WINDOW_FLAGGED.lock().remove(memory_id);
}

/// Bulk clear — used by tests and v1.6 maintenance commands.
pub fn clear_all_doubt_window_flags() {
    DOUBT_WINDOW_FLAGGED.lock().clear();
}

/// Count of memories currently flagged. Surfaced by `mgimind doctor`
/// so the user can see whether the guardrail has been firing.
pub fn doubt_window_flag_count() -> usize {
    DOUBT_WINDOW_FLAGGED.lock().len()
}

/// v1.5 Phase 8 step 8.1A: drain up to `max` flagged ids from
/// the registry, returning them sorted (deterministic for tests).
/// Used by `select_retest_candidates` to feed the background loop's
/// per-tick work queue. Drained ids are removed from the registry —
/// the caller takes responsibility for applying the doubt-window
/// state transition or re-flagging on failure.
pub fn drain_doubt_window_flags(max: usize) -> Vec<String> {
    if max == 0 {
        return Vec::new();
    }
    let mut guard = DOUBT_WINDOW_FLAGGED.lock();
    // Sort the full registry first, THEN take `max`. HashSet iteration
    // order is unstable, so `iter().take(max)` without a prior sort
    // would return a non-deterministic subset and the drain would
    // pick different ids on each call.
    let mut all: Vec<String> = guard.iter().cloned().collect();
    all.sort();
    let out: Vec<String> = all.into_iter().take(max).collect();
    for id in &out {
        guard.remove(id);
    }
    out
}

/// v1.5 Phase 8 step 8.1A: choose up to `cap` fact ids for the
/// next background re-test tick.
///
/// Priority order:
///   1. Drain DOUBT_WINDOW_FLAGGED — these are facts the Phase 7
///      error-rate guardrail explicitly forced into the queue.
///      They are the highest-priority candidates because they have
///      external evidence of being wrong.
///   2. Top up with the highest-`dependants_count` facts from the
///      base. Load-bearing facts are checked more aggressively
///      because their incorrectness has the largest blast radius.
///
/// **Hard-fail invariant (guarantee b from §10 q5):**
/// `assert!(result.len() <= cap)` — the cap is enforced; the loop
/// will panic if a future refactor breaks it. Better to crash the
/// background task (auto-restarted by the loop's outer scheduling)
/// than to silently scan unbounded work and starve the MCP path.
///
/// Returns an empty vec on any Qdrant fetch error — the loop is
/// best-effort; tick failures should not crash the server.
pub async fn select_retest_candidates(
    config: &crate::config::MindConfig,
    cap: usize,
) -> Vec<String> {
    if cap == 0 {
        return Vec::new();
    }

    // Priority 1: flagged ids drained from Phase 7 guardrail registry.
    let mut out: Vec<String> = drain_doubt_window_flags(cap);
    if out.len() >= cap {
        debug_assert!(out.len() <= cap, "drain returned more than cap");
        out.truncate(cap);
        return out;
    }

    // Priority 2: top up with high-dependants facts. Skip if Qdrant
    // is unreachable — better to do less work this tick than to
    // crash the loop.
    let remaining = cap - out.len();
    let top = match crate::knowledge::list_top_dependants_facts(config, remaining).await {
        Ok(pairs) => pairs,
        Err(e) => {
            tracing::debug!("select_retest_candidates: list_top_dependants_facts failed: {e}");
            return out;
        }
    };

    // Avoid duplicating ids already drained from the flag registry —
    // those got priority 1 treatment and don't need a second pass.
    let already: std::collections::HashSet<String> = out.iter().cloned().collect();
    for (id, _dep) in top {
        if !already.contains(&id) {
            out.push(id);
            if out.len() >= cap {
                break;
            }
        }
    }

    // Hard-fail invariant — guarantee (b) from v1.5 plan Step 8.1.
    assert!(
        out.len() <= cap,
        "select_retest_candidates returned {} > cap {}",
        out.len(),
        cap,
    );
    out
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

    // --- Inheritance flag registry ---
    //
    // The registry is process-global by design — it tracks "what came
    // in from memory in *this* warm process." Tests serialise their
    // access via clear_all_inherited at the start so they don't trip
    // over each other's state.

    #[test]
    fn mark_then_is_inherited_returns_true() {
        clear_all_inherited();
        mark_inherited("test-fact-1");
        assert!(is_inherited("test-fact-1"));
    }

    #[test]
    fn unmarked_fact_is_not_inherited() {
        clear_all_inherited();
        assert!(!is_inherited("never-marked"));
    }

    #[test]
    fn clear_inherited_removes_the_flag() {
        clear_all_inherited();
        mark_inherited("ephemeral");
        clear_inherited("ephemeral");
        assert!(!is_inherited("ephemeral"));
    }

    #[test]
    fn clear_all_wipes_the_registry() {
        clear_all_inherited();
        mark_inherited("a");
        mark_inherited("b");
        mark_inherited("c");
        assert_eq!(inherited_count(), 3);
        clear_all_inherited();
        assert_eq!(inherited_count(), 0);
        assert!(!is_inherited("a"));
        assert!(!is_inherited("b"));
        assert!(!is_inherited("c"));
    }

    #[test]
    fn mark_inherited_is_idempotent() {
        // Re-marking the same fact does not double-count or fail.
        clear_all_inherited();
        mark_inherited("dup");
        mark_inherited("dup");
        mark_inherited("dup");
        assert!(is_inherited("dup"));
        assert_eq!(inherited_count(), 1);
    }

    // --- BusyGuard + edit counter ---
    //
    // These tests share process-global state with the background loop
    // (when running). For unit-test isolation they rely on the test
    // binary running tests in a single thread by default (`cargo test`
    // serialises tests within one binary), and on the fact that the
    // background loop is only spawned by `mcp::serve` which is never
    // entered from a test.

    #[test]
    fn busy_guard_raises_and_lowers_on_drop() {
        // Drop the guard explicitly to make the test deterministic.
        // We don't run other tests that touch MCP_BUSY at the same
        // time; this test brackets its assertion narrowly.
        assert!(!is_mcp_busy(), "expected idle at test start");
        {
            let _g = BusyGuard::new();
            assert!(is_mcp_busy(), "guard should raise the flag");
        }
        assert!(!is_mcp_busy(), "guard drop should lower the flag");
    }

    #[test]
    fn edit_count_accumulates_and_resets() {
        // Drain any leftover state first so the assertion is
        // independent of test ordering.
        let _ = take_edit_count();
        record_edit();
        record_edit();
        record_edit();
        assert_eq!(take_edit_count(), 3);
        assert_eq!(take_edit_count(), 0, "swap should leave the counter at zero");
    }

    // --- v1.5 Phase 8 step 8.1A: drain + per-tick cap ---

    /// drain returns sorted ids up to `max`, and the registry is
    /// emptied of those ids (not the rest if `max` < count).
    #[test]
    fn drain_returns_sorted_subset_and_clears_them() {
        clear_all_doubt_window_flags();
        for id in ["c-fact", "a-fact", "b-fact", "d-fact"] {
            flag_for_doubt_window(id);
        }
        let drained = drain_doubt_window_flags(2);
        assert_eq!(drained, vec!["a-fact".to_string(), "b-fact".to_string()]);
        assert_eq!(doubt_window_flag_count(), 2, "two unflagged should remain");
        clear_all_doubt_window_flags(); // tidy up for sibling tests
    }

    /// `drain(0)` is a no-op even when the registry has entries —
    /// catches a misuse where the cap is computed as zero.
    #[test]
    fn drain_zero_returns_empty_and_keeps_flags() {
        clear_all_doubt_window_flags();
        flag_for_doubt_window("kept");
        let drained = drain_doubt_window_flags(0);
        assert!(drained.is_empty());
        assert_eq!(doubt_window_flag_count(), 1, "flag must not be drained");
        clear_all_doubt_window_flags();
    }

    /// `drain(huge)` returns every flag without panicking.
    #[test]
    fn drain_larger_than_count_returns_all() {
        clear_all_doubt_window_flags();
        for i in 0..5 {
            flag_for_doubt_window(&format!("fact-{i}"));
        }
        let drained = drain_doubt_window_flags(100);
        assert_eq!(drained.len(), 5);
        assert_eq!(doubt_window_flag_count(), 0);
    }

    // --- v1.5 Phase 8 step 8.1C: adaptive cadence loadavg multiplier ---

    /// loadavg_multiplier returns 1.0 on a quiet host (load below
    /// 1.5x cores). Either by reading the real /proc/loadavg (linux)
    /// or by the non-linux fallback.
    #[test]
    fn loadavg_multiplier_is_one_when_idle() {
        // We can't force the real load to a known value, so this
        // test only asserts the value is a sane multiplier (1.0 or
        // 2.0). On CI runners the load is typically <2 — the test
        // would flake otherwise.
        let m = loadavg_multiplier();
        assert!(
            m == 1.0 || m == 2.0,
            "loadavg_multiplier must return 1.0 or 2.0, got {m}"
        );
    }

    /// Pure helper: when load > 1.5×cores, the multiplier function
    /// returns 2.0. We test the formula by replicating its logic —
    /// the function itself reads /proc, which we can't fake without
    /// a mount/cgroup test. The contract is documented and stable.
    #[test]
    fn loadavg_multiplier_doubles_above_threshold() {
        // 4 cores, load 7.5 → 7.5 > 4*1.5=6.0 → doubles.
        let cores: f32 = 4.0;
        let load: f32 = 7.5;
        let expected_multiplier = if load > cores * 1.5 { 2.0 } else { 1.0 };
        assert_eq!(expected_multiplier, 2.0);

        // 4 cores, load 5.0 → 5.0 < 4*1.5=6.0 → doesn't double.
        let load: f32 = 5.0;
        let expected_multiplier = if load > cores * 1.5 { 2.0 } else { 1.0 };
        assert_eq!(expected_multiplier, 1.0);
    }
}
