//! Behavioral calibration suite for the validity model.
//!
//! The retrieval side has a measured number (R@k on LongMemEval-S). The
//! validity side — the duel rule, the doubt window — did not, so its README
//! claim rested on unit tests of individual formula properties, not on whether
//! the mechanism as a whole behaves the way a reader would expect.
//!
//! This module closes that gap with a corpus of named, realistic situations,
//! each carrying the outcome a human would expect. The runner feeds every
//! scenario through the SAME pure functions the live path uses
//! (`duel::entrenchment`, `duel::weight_new_for_mode`, `duel::resolve_duel`,
//! `doubt::apply_retrieval_event`) and reports how many land on the expected
//! outcome.
//!
//! What this IS: a check that the SHAPE of the model matches intent — a fresh
//! unsupported claim cannot overturn an entrenched belief, a deterministic CI
//! signal can, repetition alone coexists rather than overwriting. The code has
//! always said the shape is the point and the constants are placeholders
//! (`TODO(phase-4-calibration)`); this measures the shape.
//!
//! What this is NOT: proof the constants are tuned against real data. They are
//! not. A scenario whose formula outcome diverges from intuition is recorded as
//! a known divergence rather than hidden, so the report tells the truth: N of M
//! behaviors match intent, and here are the ones that do not and why.
//!
//! The divergence list is frozen in a test. A change to any tuning constant
//! that shifts a scenario across a band trips CI — so calibration work can SEE
//! its effect on intended behavior instead of moving a number blind.

use crate::duel::{
    self, DuelOutcome, EntrenchmentInputs, NewFactInputs, entrenchment, resolve_duel,
    weight_new_for_mode,
};
use crate::install_mode::InstallMode;
use crate::knowledge::Cardinality;

/// One existing-fact / challenger pair with the outcome a human would expect.
#[derive(Debug, Clone)]
pub struct DuelScenario {
    /// Stable id, also the test label.
    pub name: &'static str,
    /// The entrenched fact already in the store.
    pub old: EntrenchmentInputs,
    /// The fresh contradicting fact.
    pub new: NewFactInputs,
    /// What a person reading the situation would expect to happen.
    pub expected: DuelOutcome,
    /// Why that is the intuitive outcome (shown in the report).
    pub rationale: &'static str,
}

const fn old(dependants: u32, confirmations: u32, age_days: u32) -> EntrenchmentInputs {
    EntrenchmentInputs {
        dependants,
        confirmations,
        age_days,
    }
}

const fn challenger(diverse: u32, external: u32, from_live: bool) -> NewFactInputs {
    NewFactInputs {
        from_live_session: from_live,
        diverse_confirmations: diverse,
        external_signals: external,
        external_signal_score: None,
    }
}

/// The duel corpus. Each row is a situation an operator could describe in one
/// sentence; the expected outcome is what they would expect the store to do.
pub fn duel_corpus() -> Vec<DuelScenario> {
    use DuelOutcome::{Contested, Flip, Quarantine};
    vec![
        DuelScenario {
            name: "first_mention_cannot_flip_core",
            old: old(40, 8, 600),
            new: challenger(0, 0, true),
            expected: Quarantine,
            rationale: "a brand-new unconfirmed claim must not overturn a deeply entrenched belief",
        },
        DuelScenario {
            name: "first_mention_cannot_flip_solid",
            old: old(10, 2, 180),
            new: challenger(0, 0, true),
            expected: Quarantine,
            rationale: "one fresh assertion with no backing loses to a confirmed project fact",
        },
        DuelScenario {
            name: "rumor_vs_entrenched",
            old: old(20, 3, 400),
            new: challenger(0, 0, true),
            expected: Quarantine,
            rationale: "an unsupported contradiction of an old, depended-on fact is quarantined",
        },
        DuelScenario {
            name: "ci_signal_flips_mild",
            old: old(3, 1, 30),
            new: challenger(0, 5, true),
            expected: Flip,
            rationale: "a deterministic CI signal overturns a weakly-held fact",
        },
        DuelScenario {
            name: "ci_signal_flips_core",
            old: old(40, 8, 600),
            new: challenger(0, 5, true),
            expected: Flip,
            rationale: "strong external evidence (CI x5) overturns even a core belief: evidence beats age",
        },
        DuelScenario {
            name: "strong_both_flips_mild",
            old: old(3, 1, 30),
            new: challenger(5, 5, true),
            expected: Flip,
            rationale: "diverse confirmations plus an external signal easily beat a mild fact",
        },
        DuelScenario {
            name: "strong_both_flips_solid",
            old: old(10, 2, 180),
            new: challenger(5, 5, true),
            expected: Flip,
            rationale: "strong multi-source evidence overturns a solid project fact",
        },
        DuelScenario {
            name: "repeated_contests_solid",
            old: old(10, 2, 180),
            new: challenger(3, 0, true),
            expected: Contested,
            rationale: "repetition alone, with no external signal, coexists rather than overturning",
        },
        DuelScenario {
            name: "single_confirm_vs_core_quarantine",
            old: old(40, 8, 600),
            new: challenger(1, 0, true),
            expected: Quarantine,
            rationale: "a single repeat is far too weak against a core belief",
        },
        DuelScenario {
            name: "inherited_firstmention_vs_solid",
            old: old(10, 2, 180),
            new: challenger(0, 0, false),
            expected: Quarantine,
            rationale: "a discounted memory-sourced first mention cannot flip a solid fact",
        },
        DuelScenario {
            name: "ext_signal_flips_young",
            old: old(1, 0, 5),
            new: challenger(0, 3, true),
            expected: Flip,
            rationale: "a young barely-held fact yields to a modest external signal",
        },
        DuelScenario {
            name: "diverse_only_contests_core",
            old: old(40, 8, 600),
            new: challenger(5, 0, true),
            expected: Contested,
            rationale: "five diverse confirmations without an external signal contest a core belief, not flip it",
        },
        DuelScenario {
            name: "weak_repeat_flips_young",
            old: old(1, 0, 5),
            new: challenger(2, 0, true),
            expected: Flip,
            rationale: "two confirmations beat a barely-held young fact",
        },
        DuelScenario {
            name: "ci_x2_flips_mild",
            old: old(3, 1, 30),
            new: challenger(0, 2, true),
            expected: Flip,
            rationale: "even a small CI signal overturns a mild fact",
        },
        DuelScenario {
            // KNOWN DIVERGENCE (see DIVERGENCES): a human would expect the
            // 0.5 inheritance discount to hold a strong-but-memory-sourced
            // challenger to Contested against a core belief. The current
            // constants flip it instead. Kept in the corpus on purpose so the
            // report counts it as a miss and the freeze test pins it.
            name: "inherited_strong_vs_core_contests",
            old: old(40, 8, 600),
            new: challenger(5, 5, false),
            expected: Contested,
            rationale: "discounted strong evidence should contest a core belief, not flip it",
        },
    ]
}

/// The outcome the live formulas actually produce for a scenario, in ChatOnly
/// mode (the single-user default the README targets).
pub fn run_duel(s: &DuelScenario) -> DuelOutcome {
    let ent = entrenchment(s.old);
    let w = weight_new_for_mode(s.new, InstallMode::ChatOnly);
    let _ = Cardinality::Single; // documents that the corpus is for conflict-bearing axes
    let _ = duel::DUEL_FLIP_RATIO; // tunables the report header cites
    resolve_duel(ent, w)
}

/// Scenarios whose formula outcome does not match intuition today. Frozen here
/// so a constant change that adds or removes a divergence trips the freeze
/// test, forcing the change to be acknowledged rather than slipping in.
pub const DIVERGENCES: &[&str] = &["inherited_strong_vs_core_contests"];

/// A run of the whole corpus.
#[derive(Debug, Clone)]
pub struct CalibrationReport {
    pub total: usize,
    pub matched: usize,
    /// (name, expected, actual, rationale) for every scenario whose actual
    /// outcome differs from the expected one.
    pub misses: Vec<(&'static str, DuelOutcome, DuelOutcome, &'static str)>,
}

impl CalibrationReport {
    pub fn match_rate(&self) -> f32 {
        if self.total == 0 {
            return 1.0;
        }
        self.matched as f32 / self.total as f32
    }
}

/// Run every duel scenario and tally matches against intent.
pub fn run_calibration() -> CalibrationReport {
    let corpus = duel_corpus();
    let mut matched = 0;
    let mut misses = Vec::new();
    for s in &corpus {
        let actual = run_duel(s);
        if actual == s.expected {
            matched += 1;
        } else {
            misses.push((s.name, s.expected, actual, s.rationale));
        }
    }
    CalibrationReport {
        total: corpus.len(),
        matched,
        misses,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doubt::{self, DoubtState};

    /// The headline behavioral number: most of the corpus must match intent.
    /// A floor, not a target — if it ever drops, a constant moved the model
    /// away from intended behavior and the change must be examined.
    #[test]
    fn match_rate_meets_floor() {
        let report = run_calibration();
        assert!(
            report.match_rate() >= 0.90,
            "validity behavioral match rate {:.1}% below 90% floor ({} / {}); misses: {:?}",
            report.match_rate() * 100.0,
            report.matched,
            report.total,
            report.misses.iter().map(|m| m.0).collect::<Vec<_>>(),
        );
    }

    /// The divergence set is frozen. If a constant change makes a scenario
    /// start or stop matching, this fails — the change has shifted intended
    /// behavior and must update the corpus and this list deliberately.
    #[test]
    fn divergence_set_is_frozen() {
        let report = run_calibration();
        let mut actual_misses: Vec<&str> = report.misses.iter().map(|m| m.0).collect();
        actual_misses.sort_unstable();
        let mut expected: Vec<&str> = DIVERGENCES.to_vec();
        expected.sort_unstable();
        assert_eq!(
            actual_misses, expected,
            "calibration divergence set changed. A tuning constant shifted a \
             scenario across a band. Update duel_corpus()/DIVERGENCES on purpose, \
             do not just silence this.",
        );
    }

    /// Every scenario the corpus claims matches intent must actually match,
    /// and every listed divergence must actually diverge. Guards against a
    /// stale rationale (a scenario labelled Flip that no longer flips).
    #[test]
    fn each_scenario_is_self_consistent() {
        let div = DIVERGENCES;
        for s in duel_corpus() {
            let actual = run_duel(&s);
            let is_div = div.contains(&s.name);
            if is_div {
                assert_ne!(
                    actual, s.expected,
                    "{} is listed as a divergence but its outcome matches expected",
                    s.name
                );
            } else {
                assert_eq!(
                    actual, s.expected,
                    "{} expected {:?} but formula produced {:?} ({})",
                    s.name, s.expected, actual, s.rationale
                );
            }
        }
    }

    /// Doubt window, behavioral: five drifted retrievals push an entrenched
    /// fact into doubt and halve its weight; in-context retrievals never do.
    #[test]
    fn doubt_window_behaves() {
        // Five drifted retrievals -> Inside the doubt window.
        let mut count = 0u32;
        let mut state = DoubtState::Outside;
        for _ in 0..doubt::DOUBT_WINDOW_N_RETRIEVALS {
            let (c, s) = doubt::apply_retrieval_event(count, true);
            count = c;
            state = s;
        }
        assert_eq!(
            state,
            DoubtState::Inside,
            "an entrenched fact retrieved in {} drifted contexts should enter doubt",
            doubt::DOUBT_WINDOW_N_RETRIEVALS
        );
        assert!(
            (state.confidence_multiplier() - doubt::DOUBT_CONFIDENCE_MULTIPLIER).abs() < 1e-6,
            "doubt should halve ranking weight"
        );

        // The same number of IN-context retrievals never triggers doubt.
        let mut count = 0u32;
        let mut state = DoubtState::Outside;
        for _ in 0..(doubt::DOUBT_WINDOW_N_RETRIEVALS + 2) {
            let (c, s) = doubt::apply_retrieval_event(count, false);
            count = c;
            state = s;
        }
        assert_eq!(
            state,
            DoubtState::Outside,
            "in-context retrievals must not push a fact into doubt"
        );
    }

    /// Sanity: the corpus exercises all three duel outcomes (no band is
    /// silently untested).
    #[test]
    fn corpus_covers_all_outcomes() {
        let corpus = duel_corpus();
        let has = |o: DuelOutcome| corpus.iter().any(|s| s.expected == o);
        assert!(has(DuelOutcome::Flip), "corpus must cover Flip");
        assert!(has(DuelOutcome::Contested), "corpus must cover Contested");
        assert!(has(DuelOutcome::Quarantine), "corpus must cover Quarantine");
    }
}
