#![allow(dead_code)]
// InstallMode::ALL is kept as the CLI enumeration of valid modes for
// `mgimind config install-mode --help`. Auto-detect routes use it
// transitively; production code reaches it via parse/default.

//! v1.5 Phase 6 — install-mode profile.
//!
//! Three install profiles select different anchors for the
//! `confidence_score` formula in §6 of the validity-model synthesis:
//!
//! - `ChatOnly` (default): single-user chat assistant memory. In this
//!   mode three of the four diversity axes go quiet (§5) — `dependants`
//!   carries the load.
//! - `DevWithCi`: there is a CI loop emitting `mind_outcome(test_passed)`
//!   signals. External signal becomes a strong anchor.
//!   ALSO COVERS the `cli.rs` / `mgimind doctor` reference profile.
//! - `MultiTenant`: multiple session agents, distinct viewpoints.
//!   `confirmations` becomes load-bearing because the same fact
//!   reported by independent agents is meaningful.
//!
//! Anchors are illustrative starting points, not the final formula —
//! see §6 of the synthesis. They are calibrated by the Phase 4 STALE
//! bench's `CalibrationOverrides` sweep.

use serde::{Deserialize, Serialize};

/// Per-mode confidence-score weight triple.
///
/// `dependants + confirmations + external = 1.0` for every mode by
/// construction; see the `weights_sum_to_one` test below.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WeightTriple {
    pub dependants: f32,
    pub confirmations: f32,
    pub external: f32,
}

/// Install profile selecting per-mode confidence-score anchors.
///
/// Default is `ChatOnly` because the most common mgi-mind deployment
/// is a single-user chat assistant memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[derive(Default)]
pub enum InstallMode {
    #[default]
    ChatOnly,
    DevWithCi,
    MultiTenant,
}


impl InstallMode {
    /// Per-mode anchors from §6 synthesis. Each mode's weights sum
    /// to 1.0 by construction.
    pub const fn weights(self) -> WeightTriple {
        match self {
            Self::ChatOnly => WeightTriple {
                dependants: 0.7,
                confirmations: 0.1,
                external: 0.2,
            },
            Self::DevWithCi => WeightTriple {
                dependants: 0.5,
                confirmations: 0.15,
                external: 0.35,
            },
            Self::MultiTenant => WeightTriple {
                dependants: 0.4,
                confirmations: 0.4,
                external: 0.2,
            },
        }
    }

    /// Human-readable name for `mgimind doctor` / config UI.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ChatOnly => "chat-only",
            Self::DevWithCi => "dev-with-ci",
            Self::MultiTenant => "multi-tenant",
        }
    }

    /// Parse from CLI / config string. Accepts the kebab-case names
    /// emitted by `as_str` plus a forgiving snake_case fallback.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().replace('_', "-").as_str() {
            "chat-only" | "chatonly" => Some(Self::ChatOnly),
            "dev-with-ci" | "devwithci" => Some(Self::DevWithCi),
            "multi-tenant" | "multitenant" => Some(Self::MultiTenant),
            _ => None,
        }
    }

    /// All three variants, in display order. Used by the CLI for
    /// `mgimind config install-mode --help` enumeration.
    pub const ALL: [Self; 3] = [Self::ChatOnly, Self::DevWithCi, Self::MultiTenant];
}

/// v1.5 Phase 6 step 6.2 — auto-detect heuristic.
///
/// Snapshot of the two counts that drive `recommend()`. Lives as a
/// struct so the heuristic stays pure (testable without touching
/// Qdrant or the filesystem), and so the wire-up code can collect
/// the counts however it likes — file scan, Qdrant query, mock.
#[derive(Debug, Clone, Copy)]
pub struct DetectInputs {
    /// Distinct `mind_outcome` / procedure-outcome events recorded in
    /// the last 7 days. A live CI loop typically emits ≥ 10/week.
    pub external_signal_count_last_7d: u32,
    /// Distinct session-agent names seen in the last 30 days. Three
    /// or more means at least two non-author agents have touched the
    /// store — a multi-tenant deployment signature.
    pub distinct_session_agents_last_30d: u32,
}

/// v1.5 Phase 6 step 6.2 thresholds. Chosen as conservative cliffs
/// rather than gradients because the cost of mis-classification is
/// silent quality drift (§10 q6). Each threshold needs strong
/// evidence — better to stay on the safe `ChatOnly` default.
pub const DEV_WITH_CI_SIGNAL_THRESHOLD: u32 = 10;
pub const MULTI_TENANT_AGENT_THRESHOLD: u32 = 3;

/// Pure recommendation function. Does NOT auto-apply — the caller
/// (CLI `doctor`, `serve` first-run) reports the recommendation to
/// the user, who explicitly sets it via `mgimind config install-mode`.
///
/// Ordering matters: a MultiTenant deployment with a CI loop should
/// classify as `MultiTenant` (because multi-tenant emphasises
/// `confirmations`, which is the dominant signal in that regime).
/// So `MultiTenant` is checked first.
pub fn recommend(inputs: DetectInputs) -> InstallMode {
    if inputs.distinct_session_agents_last_30d >= MULTI_TENANT_AGENT_THRESHOLD {
        return InstallMode::MultiTenant;
    }
    if inputs.external_signal_count_last_7d >= DEV_WITH_CI_SIGNAL_THRESHOLD {
        return InstallMode::DevWithCi;
    }
    InstallMode::ChatOnly
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Gate from v1.5 plan Phase 6 Step 6.1: weights sum to 1.0 ± 0.001
    /// for every mode. Catches typo drift if anchors are edited.
    #[test]
    fn weights_sum_to_one() {
        for mode in InstallMode::ALL {
            let w = mode.weights();
            let sum = w.dependants + w.confirmations + w.external;
            assert!(
                (sum - 1.0).abs() < 0.001,
                "{} weights sum to {sum}, not 1.0",
                mode.as_str()
            );
        }
    }

    /// Gate from v1.5 plan Phase 6 Step 6.1: TOML / JSON round-trip
    /// survives kebab-case serde rename.
    #[test]
    fn round_trips_through_json() {
        for mode in InstallMode::ALL {
            let json = serde_json::to_string(&mode).expect("serialize");
            let back: InstallMode = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(mode, back, "round-trip failed for {}", mode.as_str());
        }
    }

    /// Default is `ChatOnly` — the only safe pick for a fresh install
    /// where we have no telemetry yet. Phase 6 Step 6.2 detect heuristic
    /// only ever upgrades from this default.
    #[test]
    fn default_is_chat_only() {
        assert_eq!(InstallMode::default(), InstallMode::ChatOnly);
    }

    /// `parse` mirrors `as_str` and tolerates snake_case input from
    /// older configs that pre-date the kebab-case rename.
    #[test]
    fn parse_accepts_kebab_and_snake_case() {
        assert_eq!(
            InstallMode::parse("chat-only"),
            Some(InstallMode::ChatOnly)
        );
        assert_eq!(
            InstallMode::parse("chat_only"),
            Some(InstallMode::ChatOnly)
        );
        assert_eq!(
            InstallMode::parse("DEV-WITH-CI"),
            Some(InstallMode::DevWithCi)
        );
        assert_eq!(
            InstallMode::parse("multi_tenant"),
            Some(InstallMode::MultiTenant)
        );
        assert_eq!(InstallMode::parse("nonsense"), None);
    }

    /// ChatOnly anchors `dependants` highest (load-bearing in single-user
    /// default per §5); DevWithCi raises `external`; MultiTenant raises
    /// `confirmations`. This test pins the *ordering invariant*, not the
    /// exact numbers — so future calibration can move anchors without
    /// touching the test as long as the per-mode emphasis is preserved.
    #[test]
    fn per_mode_emphasis_preserved() {
        let chat = InstallMode::ChatOnly.weights();
        assert!(
            chat.dependants > chat.confirmations,
            "ChatOnly must emphasise dependants over confirmations"
        );
        assert!(
            chat.dependants > chat.external,
            "ChatOnly must emphasise dependants over external"
        );

        let dev = InstallMode::DevWithCi.weights();
        assert!(
            dev.external > InstallMode::ChatOnly.weights().external,
            "DevWithCi must raise external above ChatOnly"
        );

        let multi = InstallMode::MultiTenant.weights();
        assert!(
            multi.confirmations > InstallMode::ChatOnly.weights().confirmations,
            "MultiTenant must raise confirmations above ChatOnly"
        );
    }

    /// Step 6.2 gate: fresh install on empty base returns ChatOnly.
    /// The safe fallback whenever there's no telemetry to act on.
    #[test]
    fn recommend_fresh_install_is_chat_only() {
        let empty = DetectInputs {
            external_signal_count_last_7d: 0,
            distinct_session_agents_last_30d: 1,
        };
        assert_eq!(recommend(empty), InstallMode::ChatOnly);
    }

    /// Step 6.2 gate: 12 procedure-outcome events / week → DevWithCi.
    /// Threshold is 10; using 12 leaves headroom for off-by-one drift.
    #[test]
    fn recommend_ci_load_promotes_to_dev_with_ci() {
        let ci_load = DetectInputs {
            external_signal_count_last_7d: 12,
            distinct_session_agents_last_30d: 1,
        };
        assert_eq!(recommend(ci_load), InstallMode::DevWithCi);
    }

    /// Three+ distinct agents in a 30-day window is the multi-tenant
    /// signature — confirmations weight becomes load-bearing.
    #[test]
    fn recommend_multiple_agents_promotes_to_multi_tenant() {
        let multi_agent = DetectInputs {
            external_signal_count_last_7d: 0,
            distinct_session_agents_last_30d: 3,
        };
        assert_eq!(recommend(multi_agent), InstallMode::MultiTenant);
    }

    /// A multi-tenant deployment that ALSO has a CI loop classifies as
    /// MultiTenant. Reason: multi-tenant emphasises confirmations
    /// (independent agents reporting the same fact), which is the
    /// dominant signal in that regime — external signals are
    /// secondary. This is the ordering invariant from `recommend`.
    #[test]
    fn recommend_multi_tenant_beats_dev_with_ci_when_both_apply() {
        let both = DetectInputs {
            external_signal_count_last_7d: 50,
            distinct_session_agents_last_30d: 5,
        };
        assert_eq!(recommend(both), InstallMode::MultiTenant);
    }

    /// Edge: exactly at the DevWithCi threshold (10) — the documented
    /// promotion point. Catches accidental off-by-one in the
    /// `>=` comparison.
    #[test]
    fn recommend_dev_with_ci_threshold_is_inclusive() {
        let at_threshold = DetectInputs {
            external_signal_count_last_7d: DEV_WITH_CI_SIGNAL_THRESHOLD,
            distinct_session_agents_last_30d: 1,
        };
        assert_eq!(recommend(at_threshold), InstallMode::DevWithCi);

        let just_below = DetectInputs {
            external_signal_count_last_7d: DEV_WITH_CI_SIGNAL_THRESHOLD - 1,
            distinct_session_agents_last_30d: 1,
        };
        assert_eq!(recommend(just_below), InstallMode::ChatOnly);
    }
}
