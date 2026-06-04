//! v1.5 Phase 7 — generalised external-signal model.
//!
//! Replaces the v1.4 `external_signals: Vec<String>` payload (where
//! the strings were source URLs and the count was the only signal)
//! with a typed `Vec<ExternalSignal>` that carries:
//!
//! - the signal *type* (test_passed / code_compiled / user_confirmed /
//!   cited_by) — each weighted differently in §6 confidence_score;
//! - the signal *success* boolean — failures count negatively;
//! - the signal *source* — used for idempotency (re-posting the same
//!   (memory, type, source) does not double-count);
//! - the signal *timestamp* — used by the error-rate guardrail in
//!   Step 7.3 to decide doubt-window promotion.
//!
//! The scoring formula (§7 of the v1.5 plan):
//!
//! ```text
//! external_signal_score(fact) =
//!     sum over signal in fact.external_signals of:
//!         signal_type_weight(signal.type) *
//!         (if signal.success then 1.0 else SIGNAL_FAILURE_PENALTY)
//! ```
//!
//! `cited_by` carries an additional self-citation guard handled in
//! `compute_external_signal_score()` below: a `cited_by` signal only
//! counts when the *citing* memory has its own `confidence_score`
//! above `CITED_BY_MIN_CITING_CONFIDENCE`. This prevents low-quality
//! chains from boosting each other.

use serde::{Deserialize, Serialize};

/// Per-type weight applied to a positive (success=true) signal.
/// Multiplied by `SIGNAL_FAILURE_PENALTY` for failures so a failed
/// signal is counted negatively (real evidence the claim is wrong)
/// rather than ignored.
pub const TEST_PASSED_WEIGHT: f32 = 1.0;
pub const CODE_COMPILED_WEIGHT: f32 = 0.3;
pub const USER_CONFIRMED_WEIGHT: f32 = 0.7;
pub const CITED_BY_WEIGHT: f32 = 0.2;

/// Multiplier applied to a per-type weight when `success = false`.
/// Negative because a failing test is real evidence the claim is
/// wrong, not just absence of evidence. -0.5 is the v1.5 plan §7
/// anchor — a failure counts as half a positive's worth of evidence
/// pulling the other way.
pub const SIGNAL_FAILURE_PENALTY: f32 = -0.5;

/// Minimum citing-memory confidence_score below which a `cited_by`
/// signal is ignored. Prevents low-confidence chains from boosting
/// each other via mutual citation. The threshold is conservative —
/// at 0.5 a citing memory has to be at least "neutral" in the
/// confidence-score distribution.
pub const CITED_BY_MIN_CITING_CONFIDENCE: f32 = 0.5;

/// Signal types accepted by `mind_outcome`. The set is intentionally
/// closed in v1.5 — community contributions to add a variant land in
/// v1.6. Closed-set keeps schema validation tight and makes the
/// `external_signal_score` formula auditable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeSignal {
    /// A test exercising this fact passed (or failed if `success=false`).
    /// Weighted strongest — a passing test is the cleanest deterministic
    /// signal we have.
    TestPassed,
    /// Code mentioning this fact compiled cleanly. Weaker than
    /// `test_passed` because compilation only proves shape, not
    /// behaviour.
    CodeCompiled,
    /// A human user explicitly confirmed (or denied) this fact. Weighted
    /// above `code_compiled` because the user is the eventual ground
    /// truth, but below `test_passed` because humans are noisier than
    /// deterministic signals.
    UserConfirmed,
    /// Another memory in the store cites this one. Weighted lowest
    /// because it's a soft signal subject to self-citation loops —
    /// only counts when the citing memory's own confidence is above
    /// `CITED_BY_MIN_CITING_CONFIDENCE`.
    CitedBy,
}

impl OutcomeSignal {
    /// Per-type slot weight applied to a successful signal.
    pub const fn weight(self) -> f32 {
        match self {
            Self::TestPassed => TEST_PASSED_WEIGHT,
            Self::CodeCompiled => CODE_COMPILED_WEIGHT,
            Self::UserConfirmed => USER_CONFIRMED_WEIGHT,
            Self::CitedBy => CITED_BY_WEIGHT,
        }
    }

    /// Parse a snake_case identifier into a known signal type. Returns
    /// `None` for unknown strings — `mind_outcome` rejects them with an
    /// explicit error so schema drift is caught at the boundary, not
    /// silently logged.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "test_passed" => Some(Self::TestPassed),
            "code_compiled" => Some(Self::CodeCompiled),
            "user_confirmed" => Some(Self::UserConfirmed),
            "cited_by" => Some(Self::CitedBy),
            _ => None,
        }
    }

    /// All variants in display order. Used by the CLI for error
    /// messages enumerating valid signal_types.
    pub const ALL: [Self; 4] = [
        Self::TestPassed,
        Self::CodeCompiled,
        Self::UserConfirmed,
        Self::CitedBy,
    ];
}

/// One external-signal entry inside a fact's payload. The full
/// signal log is `Vec<ExternalSignal>` and is stored as JSON inside
/// the existing Qdrant payload field — Phase 7 reuses the v1.4
/// `external_signals` slot rather than introducing a new schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExternalSignal {
    /// Signal type — see `OutcomeSignal`.
    pub signal_type: OutcomeSignal,
    /// Whether the signal was positive (test passed, code compiled,
    /// user confirmed) or negative (test failed, etc).
    pub success: bool,
    /// Source identifier for idempotency. Examples:
    /// `"ci.github.com/run/12345"`, `"user-mad"`, `"cargo-test"`.
    /// Two outcomes with identical (signal_type, source) on the same
    /// memory deduplicate to one — only the latest is kept.
    pub source: String,
    /// RFC3339 timestamp of when the signal arrived. Used by the
    /// Step 7.3 error-rate guardrail to decide doubt-window
    /// promotion based on recent failures.
    pub ts: String,
}

/// Compute the external-signal score for a fact, given its full
/// signal log. Phase 7 step 7.2 wires this into `entrenchment` and
/// `weight_new` in `duel.rs`.
///
/// The `citing_confidence_lookup` is invoked for each `cited_by`
/// signal to fetch the citing memory's confidence. Pure-function
/// callers (tests, sweep tooling) inject a stub closure; production
/// callers wire it to `crate::knowledge::confidence_score_of(id)`.
///
/// Returns the summed, per-type-weighted score. Can be negative when
/// failures dominate — that is the point.
pub fn compute_external_signal_score<F>(
    signals: &[ExternalSignal],
    mut citing_confidence_lookup: F,
) -> f32
where
    F: FnMut(&str) -> Option<f32>,
{
    let mut score = 0.0;
    for sig in signals {
        if sig.signal_type == OutcomeSignal::CitedBy {
            // Self-citation guard: skip when the citing memory's own
            // confidence is unknown or below threshold. `source` for
            // a cited_by signal is the citing memory's id.
            let citing_conf = citing_confidence_lookup(&sig.source);
            if citing_conf.is_none_or(|c| c < CITED_BY_MIN_CITING_CONFIDENCE) {
                continue;
            }
        }
        let term = if sig.success {
            sig.signal_type.weight()
        } else {
            sig.signal_type.weight() * SIGNAL_FAILURE_PENALTY
        };
        score += term;
    }
    score
}

/// v1.5 Phase 7 step 7.3: error-rate guardrail threshold.
/// Number of recent failed `test_passed` signals that flips a fact
/// into the doubt window. Three failures means the claim is
/// consistently contradicted by deterministic signals — stronger
/// evidence than any conversational repetition.
pub const ERROR_RATE_FAIL_THRESHOLD: usize = 3;

/// v1.5 Phase 7 step 7.3: lookback window for the guardrail.
/// 7 days mirrors the Step 6.2 install-mode auto-detect window —
/// "recent enough to act on" without overreacting to a single bad
/// CI run from a year ago.
pub const ERROR_RATE_WINDOW_DAYS: i64 = 7;

/// v1.5 Phase 7 step 7.3: pure error-rate check.
///
/// Returns true when the signal log contains at least
/// `ERROR_RATE_FAIL_THRESHOLD` failed `test_passed` entries with
/// timestamps within `ERROR_RATE_WINDOW_DAYS` of `now`. Callers
/// (mind_outcome handler, background re-test loop) react by forcing
/// the fact into the doubt window — never delete; Mechanism 1
/// invariant says the loser keeps its trace.
pub fn should_promote_to_doubt_window(
    signals: &[ExternalSignal],
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    let cutoff = now - chrono::Duration::days(ERROR_RATE_WINDOW_DAYS);
    let recent_failures = signals
        .iter()
        .filter(|s| s.signal_type == OutcomeSignal::TestPassed && !s.success)
        .filter_map(|s| chrono::DateTime::parse_from_rfc3339(&s.ts).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .filter(|dt| *dt >= cutoff)
        .count();
    recent_failures >= ERROR_RATE_FAIL_THRESHOLD
}

/// v1.5 Phase 7 step 7.1: high-level mind_outcome handler.
///
/// Reads the existing signal log on `memory_id`, appends `new_signal`,
/// dedups by (signal_type, source) keeping the latest timestamp, and
/// persists the result. Returns a human-readable summary used by the
/// CLI / MCP responses.
///
/// Idempotent on (memory_id, signal_type, source): re-posting the
/// same triple updates the existing entry rather than appending a
/// duplicate.
pub async fn record(
    config: &crate::config::MindConfig,
    memory_id: &str,
    new_signal: ExternalSignal,
) -> anyhow::Result<String> {
    let mut existing = crate::storage::read_external_signals(config, memory_id).await?;
    let type_name = serde_json::to_string(&new_signal.signal_type)
        .unwrap_or_else(|_| "<unknown>".to_string())
        .trim_matches('"')
        .to_string();
    existing.push(new_signal.clone());
    let deduped = dedup_keep_latest(existing);
    crate::storage::write_external_signals(config, memory_id, &deduped).await?;

    // v1.5 Phase 7 step 7.3 — error-rate guardrail. After every
    // outcome write we re-check whether the recent failure count
    // crossed the threshold. If yes, the fact gets flagged for the
    // doubt window via the Phase 3 registry. Doing this check after
    // write (not before) means a single `mind_outcome` call can
    // both record AND promote, eliminating a stale-read window.
    let guardrail_msg = if should_promote_to_doubt_window(&deduped, chrono::Utc::now()) {
        crate::doubt::flag_for_doubt_window(memory_id);
        format!(
            " ⚠ guardrail triggered: ≥{ERROR_RATE_FAIL_THRESHOLD} failed test_passed signals in last {ERROR_RATE_WINDOW_DAYS}d — flagged for doubt window."
        )
    } else {
        String::new()
    };

    Ok(format!(
        "Recorded {type_name} (success={}) on {memory_id} from source '{}' — {} distinct signal(s) now logged.{guardrail_msg}",
        new_signal.success,
        new_signal.source,
        deduped.len(),
    ))
}

/// Deduplicate by (signal_type, source) keeping the *latest* entry by
/// timestamp. Used by `mind_outcome` to keep the per-fact signal log
/// from growing unbounded when the same CI run reposts. Stable order
/// otherwise — equal-timestamp duplicates keep insertion order.
pub fn dedup_keep_latest(signals: Vec<ExternalSignal>) -> Vec<ExternalSignal> {
    use std::collections::HashMap;
    // Index of (signal_type, source) → position of the latest in `out`.
    let mut latest_idx: HashMap<(OutcomeSignal, String), usize> = HashMap::new();
    let mut out: Vec<ExternalSignal> = Vec::with_capacity(signals.len());
    for sig in signals {
        let key = (sig.signal_type, sig.source.clone());
        match latest_idx.get(&key) {
            Some(&i) if out[i].ts >= sig.ts => {
                // existing entry is at least as new; skip.
            }
            Some(&i) => {
                out[i] = sig;
            }
            None => {
                latest_idx.insert(key, out.len());
                out.push(sig);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(t: OutcomeSignal, success: bool, source: &str, ts: &str) -> ExternalSignal {
        ExternalSignal {
            signal_type: t,
            success,
            source: source.to_string(),
            ts: ts.to_string(),
        }
    }

    /// Anchor weights are pinned to the v1.5 plan §7 numbers. If
    /// someone re-tunes them, this test catches the drift so the
    /// release notes mention it.
    #[test]
    fn weights_match_v15_plan_anchors() {
        assert_eq!(OutcomeSignal::TestPassed.weight(), 1.0);
        assert_eq!(OutcomeSignal::UserConfirmed.weight(), 0.7);
        assert_eq!(OutcomeSignal::CodeCompiled.weight(), 0.3);
        assert_eq!(OutcomeSignal::CitedBy.weight(), 0.2);
    }

    /// Ordering invariant from v1.5 plan §7 commentary:
    /// test_passed > user_confirmed > code_compiled > cited_by.
    /// This holds even if the exact anchors get re-tuned.
    #[test]
    fn weight_ordering_test_passed_dominates() {
        let test = OutcomeSignal::TestPassed.weight();
        let user = OutcomeSignal::UserConfirmed.weight();
        let code = OutcomeSignal::CodeCompiled.weight();
        let cited = OutcomeSignal::CitedBy.weight();
        assert!(test > user, "test_passed must outweigh user_confirmed");
        assert!(user > code, "user_confirmed must outweigh code_compiled");
        assert!(code > cited, "code_compiled must outweigh cited_by");
    }

    /// snake_case serde roundtrip. `mind_outcome` payloads use the
    /// snake_case names verbatim — schema validation depends on this.
    #[test]
    fn signal_type_round_trips_through_json() {
        for variant in OutcomeSignal::ALL {
            let json = serde_json::to_string(&variant).unwrap();
            let back: OutcomeSignal = serde_json::from_str(&json).unwrap();
            assert_eq!(variant, back);
        }
    }

    /// Unknown strings are rejected by `parse` — schema drift caught
    /// at the boundary.
    #[test]
    fn parse_rejects_unknown_signal_types() {
        assert!(OutcomeSignal::parse("nonsense").is_none());
        assert!(OutcomeSignal::parse("").is_none());
    }

    /// Successful signals sum to their per-type weights — no
    /// citing-confidence lookup needed when no cited_by present.
    #[test]
    fn score_is_sum_of_successful_weights() {
        let signals = vec![
            sig(OutcomeSignal::TestPassed, true, "ci-1", "2026-06-04T00:00:00Z"),
            sig(OutcomeSignal::UserConfirmed, true, "user-mad", "2026-06-04T00:00:00Z"),
        ];
        let score = compute_external_signal_score(&signals, |_| None);
        assert!((score - 1.7).abs() < 1e-5, "expected 1.7, got {score}");
    }

    /// A failed signal pulls negative (success = false → weight ×
    /// SIGNAL_FAILURE_PENALTY = weight × -0.5).
    #[test]
    fn failed_signal_subtracts_half_weight() {
        let signals = vec![sig(
            OutcomeSignal::TestPassed,
            false,
            "ci-1",
            "2026-06-04T00:00:00Z",
        )];
        let score = compute_external_signal_score(&signals, |_| None);
        assert!((score - (-0.5)).abs() < 1e-5, "expected -0.5, got {score}");
    }

    /// cited_by with a high-confidence citing memory counts; with a
    /// low-confidence citing memory it does not. Self-citation guard.
    #[test]
    fn cited_by_self_citation_guard_blocks_low_confidence_citers() {
        let signals = vec![sig(
            OutcomeSignal::CitedBy,
            true,
            "citer-id",
            "2026-06-04T00:00:00Z",
        )];

        // High-confidence citer: counts.
        let score_high = compute_external_signal_score(&signals, |id| {
            if id == "citer-id" { Some(0.8) } else { None }
        });
        assert!(
            (score_high - CITED_BY_WEIGHT).abs() < 1e-5,
            "high-confidence cited_by should contribute weight: got {score_high}"
        );

        // Low-confidence citer: blocked.
        let score_low = compute_external_signal_score(&signals, |id| {
            if id == "citer-id" { Some(0.3) } else { None }
        });
        assert_eq!(score_low, 0.0, "low-confidence cited_by should be blocked");

        // Unknown citer: blocked (conservative default).
        let score_unknown = compute_external_signal_score(&signals, |_| None);
        assert_eq!(score_unknown, 0.0, "unknown citer should be blocked");
    }

    /// dedup keeps the latest by ts when (type, source) collide.
    #[test]
    fn dedup_keeps_latest_by_timestamp() {
        let signals = vec![
            sig(OutcomeSignal::TestPassed, true, "ci-1", "2026-06-04T00:00:00Z"),
            sig(OutcomeSignal::TestPassed, false, "ci-1", "2026-06-04T01:00:00Z"),
            sig(OutcomeSignal::TestPassed, true, "ci-1", "2026-06-03T00:00:00Z"),
        ];
        let deduped = dedup_keep_latest(signals);
        assert_eq!(deduped.len(), 1);
        assert!(!deduped[0].success, "latest entry (01:00) is the failure");
        assert_eq!(deduped[0].ts, "2026-06-04T01:00:00Z");
    }

    /// dedup preserves entries with distinct (type, source) keys.
    #[test]
    fn dedup_preserves_distinct_keys() {
        let signals = vec![
            sig(OutcomeSignal::TestPassed, true, "ci-1", "2026-06-04T00:00:00Z"),
            sig(OutcomeSignal::TestPassed, true, "ci-2", "2026-06-04T00:00:00Z"),
            sig(OutcomeSignal::UserConfirmed, true, "user-mad", "2026-06-04T00:00:00Z"),
        ];
        let deduped = dedup_keep_latest(signals);
        assert_eq!(deduped.len(), 3, "distinct keys must not collapse");
    }

    // --- v1.5 Phase 7 step 7.3: error-rate guardrail ---

    fn now_for_test() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-06-04T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    /// Exactly 3 recent failed test_passed signals → promote.
    /// The threshold is inclusive (>=).
    #[test]
    fn guardrail_promotes_at_threshold() {
        let signals = vec![
            sig(OutcomeSignal::TestPassed, false, "ci-1", "2026-06-04T11:00:00Z"),
            sig(OutcomeSignal::TestPassed, false, "ci-2", "2026-06-04T10:00:00Z"),
            sig(OutcomeSignal::TestPassed, false, "ci-3", "2026-06-04T09:00:00Z"),
        ];
        assert!(should_promote_to_doubt_window(&signals, now_for_test()));
    }

    /// 2 recent failed test_passed signals → no promotion (just below
    /// threshold, signal still ambiguous).
    #[test]
    fn guardrail_does_not_promote_below_threshold() {
        let signals = vec![
            sig(OutcomeSignal::TestPassed, false, "ci-1", "2026-06-04T11:00:00Z"),
            sig(OutcomeSignal::TestPassed, false, "ci-2", "2026-06-04T10:00:00Z"),
        ];
        assert!(!should_promote_to_doubt_window(&signals, now_for_test()));
    }

    /// Failed test_passed signals older than the window do not count.
    /// 30 days back is well outside the 7-day window.
    #[test]
    fn guardrail_ignores_signals_outside_window() {
        let signals = vec![
            sig(OutcomeSignal::TestPassed, false, "ci-1", "2026-05-05T11:00:00Z"),
            sig(OutcomeSignal::TestPassed, false, "ci-2", "2026-05-05T10:00:00Z"),
            sig(OutcomeSignal::TestPassed, false, "ci-3", "2026-05-05T09:00:00Z"),
        ];
        assert!(!should_promote_to_doubt_window(&signals, now_for_test()));
    }

    /// Failed user_confirmed or code_compiled signals do NOT trigger
    /// the guardrail — only test_passed counts. User opinion and
    /// compile success are noisier signals; only deterministic test
    /// failures pass the bar for forced doubt-window promotion.
    #[test]
    fn guardrail_only_counts_test_passed_failures() {
        let signals = vec![
            sig(OutcomeSignal::UserConfirmed, false, "user-1", "2026-06-04T11:00:00Z"),
            sig(OutcomeSignal::UserConfirmed, false, "user-2", "2026-06-04T10:00:00Z"),
            sig(OutcomeSignal::UserConfirmed, false, "user-3", "2026-06-04T09:00:00Z"),
            sig(OutcomeSignal::CodeCompiled, false, "cargo-1", "2026-06-04T11:00:00Z"),
            sig(OutcomeSignal::CodeCompiled, false, "cargo-2", "2026-06-04T10:00:00Z"),
            sig(OutcomeSignal::CodeCompiled, false, "cargo-3", "2026-06-04T09:00:00Z"),
        ];
        assert!(!should_promote_to_doubt_window(&signals, now_for_test()));
    }

    /// Successful test_passed signals do NOT trigger the guardrail —
    /// only `success = false` failures count. A passing test confirms
    /// the fact, the opposite of what the guardrail watches for.
    #[test]
    fn guardrail_only_counts_failures_not_successes() {
        let signals = vec![
            sig(OutcomeSignal::TestPassed, true, "ci-1", "2026-06-04T11:00:00Z"),
            sig(OutcomeSignal::TestPassed, true, "ci-2", "2026-06-04T10:00:00Z"),
            sig(OutcomeSignal::TestPassed, true, "ci-3", "2026-06-04T09:00:00Z"),
        ];
        assert!(!should_promote_to_doubt_window(&signals, now_for_test()));
    }

    /// Mixed log: 3 recent failures + 100 older successes → promote.
    /// The guardrail does not "net out" successes against failures
    /// inside the window; the §3 Mechanism 2 contract is that recent
    /// failure evidence triggers a re-test pass regardless of
    /// historical successes. If the historical pattern is real, the
    /// re-test will resurface it.
    #[test]
    fn guardrail_ignores_historical_successes_when_recent_failures_exist() {
        let mut signals = vec![
            sig(OutcomeSignal::TestPassed, false, "ci-1", "2026-06-04T11:00:00Z"),
            sig(OutcomeSignal::TestPassed, false, "ci-2", "2026-06-04T10:00:00Z"),
            sig(OutcomeSignal::TestPassed, false, "ci-3", "2026-06-04T09:00:00Z"),
        ];
        for i in 0..100 {
            signals.push(sig(
                OutcomeSignal::TestPassed,
                true,
                &format!("history-ci-{i}"),
                "2026-05-05T00:00:00Z",
            ));
        }
        assert!(should_promote_to_doubt_window(&signals, now_for_test()));
    }
}
