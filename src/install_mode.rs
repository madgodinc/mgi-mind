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
pub enum InstallMode {
    ChatOnly,
    DevWithCi,
    MultiTenant,
}

impl Default for InstallMode {
    fn default() -> Self {
        Self::ChatOnly
    }
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
}
