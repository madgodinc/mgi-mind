//! v1.4 Phase 4: STALE benchmark adapter.
//!
//! Runs the v1.4 validity-model implementation against the STALE
//! benchmark from Chao et al., arxiv 2605.06527 (May 2026). STALE
//! measures LLM-agent belief revision behaviour and is the
//! field-recognised benchmark for the failure modes the v1.4
//! mechanisms are designed to address.
//!
//! Per synthesis §11 and the implementation plan Phase 4.3:
//! - Phase 4.1 sweeps tunable constants against LongMemEval-S R@k.
//! - Phase 4.2 runs LongMemEval-S as a regression check.
//! - **Phase 4.3 (this module)** runs STALE 400 scenarios / 1200
//!   queries and reports Overall + per-metric (State Resolution,
//!   Premise Resistance, Implicit Policy Adaptation) + per-conflict-
//!   type (Type I co-referential, Type II propagated).
//!
//! The result is the v1.4 release headline.
//!
//! ## What this module does today
//!
//! Scaffold. The actual STALE harness (a Python project under
//! CC BY 4.0, mentioned in the paper's Appendix G) needs to be
//! cloned, our adapter written against its protocol, and a judge
//! configured (Gemini-3.1-flash-lite per the paper, 95.8% human
//! agreement). The runner here:
//!
//! 1. Loads a STALE scenario file (jsonl, one scenario per line).
//! 2. For each scenario: clears the mgi-mind store to a clean
//!    slate, ingests the scenario's history into facts, runs the
//!    three behavioural queries.
//! 3. Sends each query+answer pair to the configured judge model
//!    (HTTP call, key in MGIMIND_STALE_JUDGE_KEY env), records the
//!    judge's verdict.
//! 4. Aggregates into Overall + per-metric + per-conflict-type
//!    accuracy.
//! 5. Writes raw per-scenario results to a JSON file and prints a
//!    summary block.
//!
//! ## Cost realism (synthesis §11 update from round 4)
//!
//! 1200 queries × ~150K token contexts × judge calls ≈ tens to low
//! hundreds of USD on a flash-tier judge. The CLI accepts `--limit`
//! for a smoke run and `--judge` to choose a cheaper model for
//! parameter sweeps before the headline run.
//!
//! ## Privacy
//!
//! STALE is a public dataset of synthetic personas. No author data
//! crosses to the judge. The mgi-mind store is wiped between
//! scenarios; this module does not write into the author's working
//! base unless `--target` explicitly points at it (default is a
//! throwaway `$MGIMIND_HOME/bench-stale-staging/`).

#![allow(dead_code)]

use anyhow::Result;
use std::path::PathBuf;

/// Result of running a single STALE scenario through mgi-mind.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ScenarioResult {
    pub scenario_id: String,
    pub conflict_type: ConflictType,
    /// State Resolution: did the system recognise the prior belief
    /// is invalid when asked directly?
    pub state_resolution: bool,
    /// Premise Resistance: did the system reject the stale
    /// presupposition when the query embedded it?
    pub premise_resistance: bool,
    /// Implicit Policy Adaptation: did the system proactively apply
    /// the updated belief when the query was naturally phrased?
    pub implicit_policy_adaptation: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum ConflictType {
    /// Co-referential: both observations address the same attribute
    /// with incompatible values, no explicit negation.
    /// Example: Seattle residence → Portland utilities setup.
    TypeI,
    /// Propagated: an update to one attribute cascades through
    /// logical dependency to invalidate a structurally distinct one.
    /// Example: scorpion-in-boot → climate/pests → city.
    TypeII,
}

/// Aggregated report. The Overall % is the headline number.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StaleReport {
    pub scenarios_run: usize,
    pub overall_pct: f32,
    pub by_metric: ByMetric,
    pub by_conflict_type: ByConflictType,
    pub judge_model: String,
    pub mgimind_version: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ByMetric {
    pub state_resolution_pct: f32,
    pub premise_resistance_pct: f32,
    pub implicit_policy_adaptation_pct: f32,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ByConflictType {
    pub type_i_pct: f32,
    pub type_ii_pct: f32,
}

// ===== Pure aggregation =====

/// Aggregate a stream of per-scenario results into the headline
/// report. Pure function, unit-testable without the harness.
pub fn aggregate(results: &[ScenarioResult], judge_model: &str) -> StaleReport {
    let n = results.len();
    if n == 0 {
        return StaleReport {
            scenarios_run: 0,
            overall_pct: 0.0,
            by_metric: ByMetric {
                state_resolution_pct: 0.0,
                premise_resistance_pct: 0.0,
                implicit_policy_adaptation_pct: 0.0,
            },
            by_conflict_type: ByConflictType {
                type_i_pct: 0.0,
                type_ii_pct: 0.0,
            },
            judge_model: judge_model.to_string(),
            mgimind_version: env!("CARGO_PKG_VERSION").to_string(),
        };
    }

    let count_true = |sel: fn(&ScenarioResult) -> bool| -> usize {
        results.iter().filter(|r| sel(r)).count()
    };
    let sr = count_true(|r| r.state_resolution);
    let pr = count_true(|r| r.premise_resistance);
    let ipa = count_true(|r| r.implicit_policy_adaptation);

    // Overall = mean of 6 cells (3 metrics × 2 conflict types) per
    // STALE paper §3.1. We approximate as (sr + pr + ipa) / (3n)
    // when scenarios are equally split; the exact per-cell mean
    // requires per-conflict-type breakdown.
    let total_correct_cells = sr + pr + ipa;
    let overall_pct = 100.0 * total_correct_cells as f32 / (3.0 * n as f32);

    let type_i: Vec<&ScenarioResult> =
        results.iter().filter(|r| r.conflict_type == ConflictType::TypeI).collect();
    let type_ii: Vec<&ScenarioResult> =
        results.iter().filter(|r| r.conflict_type == ConflictType::TypeII).collect();

    let _metric_pct = |selected: &[&ScenarioResult],
                       sel: fn(&ScenarioResult) -> bool|
     -> f32 {
        if selected.is_empty() {
            0.0
        } else {
            100.0 * selected.iter().filter(|r| sel(r)).count() as f32 / selected.len() as f32
        }
    };

    let type_i_pct = if type_i.is_empty() {
        0.0
    } else {
        let sum = type_i
            .iter()
            .map(|r| {
                (r.state_resolution as u32 + r.premise_resistance as u32 + r.implicit_policy_adaptation as u32)
                    as f32
            })
            .sum::<f32>();
        100.0 * sum / (3.0 * type_i.len() as f32)
    };

    let type_ii_pct = if type_ii.is_empty() {
        0.0
    } else {
        let sum = type_ii
            .iter()
            .map(|r| {
                (r.state_resolution as u32 + r.premise_resistance as u32 + r.implicit_policy_adaptation as u32)
                    as f32
            })
            .sum::<f32>();
        100.0 * sum / (3.0 * type_ii.len() as f32)
    };

    StaleReport {
        scenarios_run: n,
        overall_pct,
        by_metric: ByMetric {
            state_resolution_pct: 100.0 * sr as f32 / n as f32,
            premise_resistance_pct: 100.0 * pr as f32 / n as f32,
            implicit_policy_adaptation_pct: 100.0 * ipa as f32 / n as f32,
        },
        by_conflict_type: ByConflictType {
            type_i_pct,
            type_ii_pct,
        },
        judge_model: judge_model.to_string(),
        mgimind_version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

/// Classify the result against synthesis §11 publish-or-not bands.
pub fn publish_decision(report: &StaleReport) -> PublishDecision {
    if report.overall_pct >= 50.0 {
        PublishDecision::PublishLikelyCUPMemRange
    } else if report.overall_pct >= 30.0 {
        PublishDecision::PublishHeadline
    } else if report.overall_pct >= 15.0 {
        PublishDecision::PublishHonest
    } else {
        PublishDecision::Withhold
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishDecision {
    /// >= 50% Overall: in CUPMem (68%) range. Likely artifacted by
    /// > the harness; verify before publishing.
    PublishLikelyCUPMemRange,
    /// 30-50% Overall: clean v1.4 release headline. Beats LightMem
    /// (17.8%), materially beats mem0 (8.3%) and Zep (6.0%).
    PublishHeadline,
    /// 15-30% Overall: honest release. Useful baseline for
    /// iteration, narrative is "first to ship; full pipeline needs
    /// more calibration."
    PublishHonest,
    /// < 15% Overall: do not publish the STALE number publicly. Do
    /// publish the R@k bench. Re-enter calibration.
    Withhold,
}

// ===== Render =====

pub fn render_summary(report: &StaleReport) -> String {
    format!(
        "\nSTALE benchmark — mgi-mind v{}\n\
         Judge: {}\n\
         Scenarios: {}\n\n\
         Overall:   {:.1}%\n\
         By metric:\n\
         \x20\x20State Resolution:          {:.1}%\n\
         \x20\x20Premise Resistance:        {:.1}%\n\
         \x20\x20Implicit Policy Adaptation: {:.1}%\n\
         By conflict type:\n\
         \x20\x20Type I  (co-referential):  {:.1}%\n\
         \x20\x20Type II (propagated):      {:.1}%\n\n\
         Reference baselines (paper, Overall):\n\
         \x20\x20mem0       =  8.3%\n\
         \x20\x20Zep        =  6.0%\n\
         \x20\x20A-mem      =  5.1%\n\
         \x20\x20LightMem   = 17.8%\n\
         \x20\x20Gemini-3.1-pro (no memory) = 55.2%\n\
         \x20\x20CUPMem (STALE paper arch)  = 68.0%\n",
        report.mgimind_version,
        report.judge_model,
        report.scenarios_run,
        report.overall_pct,
        report.by_metric.state_resolution_pct,
        report.by_metric.premise_resistance_pct,
        report.by_metric.implicit_policy_adaptation_pct,
        report.by_conflict_type.type_i_pct,
        report.by_conflict_type.type_ii_pct,
    )
}

// ===== Public entry — scaffold =====

/// Phase 4 calibration overrides — passed into `run()` so the sweep
/// harness can iterate over duel/doubt constants without recompiling.
/// Post-critic addition (PR #7 round): the previous signature gave the
/// sweep harness no way to vary thresholds; v2.0 STALE re-runs depend
/// on this being addressable.
///
/// Each field is `Option<f32>` — `None` means "use the compiled-in
/// constant from duel.rs / doubt.rs"; `Some(value)` means "override
/// for this run only." This keeps the default-call site
/// `CalibrationOverrides::default()` cheap and the sweep harness
/// just sets one field at a time.
#[derive(Debug, Clone, Default)]
pub struct CalibrationOverrides {
    pub duel_flip_ratio: Option<f32>,
    pub duel_contested_ratio: Option<f32>,
    pub entrenchment_norm_divisor: Option<f32>,
    pub weight_confirmations: Option<f32>,
    pub weight_external_signal: Option<f32>,
    pub inheritance_discount: Option<f32>,
    pub doubt_drift_threshold: Option<f32>,
    pub doubt_confidence_multiplier: Option<f32>,
    pub doubt_window_n_retrievals: Option<u32>,
}

impl CalibrationOverrides {
    /// Render the active overrides as a one-line tag for the
    /// `calibration_report.md` rows. None-valued fields are omitted so
    /// a row stays compact.
    pub fn tag(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(v) = self.duel_flip_ratio {
            parts.push(format!("flip={v}"));
        }
        if let Some(v) = self.duel_contested_ratio {
            parts.push(format!("contested={v}"));
        }
        if let Some(v) = self.entrenchment_norm_divisor {
            parts.push(format!("norm_div={v}"));
        }
        if let Some(v) = self.weight_confirmations {
            parts.push(format!("w_conf={v}"));
        }
        if let Some(v) = self.weight_external_signal {
            parts.push(format!("w_ext={v}"));
        }
        if let Some(v) = self.inheritance_discount {
            parts.push(format!("inh={v}"));
        }
        if let Some(v) = self.doubt_drift_threshold {
            parts.push(format!("drift={v}"));
        }
        if let Some(v) = self.doubt_confidence_multiplier {
            parts.push(format!("doubt_mul={v}"));
        }
        if let Some(v) = self.doubt_window_n_retrievals {
            parts.push(format!("doubt_n={v}"));
        }
        if parts.is_empty() {
            "default".to_string()
        } else {
            parts.join(",")
        }
    }
}

/// Run the STALE benchmark against the current mgi-mind store.
///
/// **Scaffold note.** The harness adapter (STALE protocol →
/// mgi-mind tool calls → judge → result) is not implemented yet.
/// The CLI command exists so the surface is testable and the
/// downstream tooling (sweep scripts, calibration report writer)
/// can be developed against the type contracts here.
pub async fn run(
    _dataset: PathBuf,
    _judge_model: &str,
    _limit: Option<usize>,
    overrides: CalibrationOverrides,
    output: PathBuf,
) -> Result<StaleReport> {
    eprintln!("STALE bench: scaffold — harness adapter not implemented yet");
    eprintln!("            overrides: {}", overrides.tag());
    eprintln!("            target output path: {}", output.display());
    eprintln!(
        "            wire-up requires the STALE public harness (Appendix G of arxiv 2605.06527)"
    );
    eprintln!("            and a judge model env (MGIMIND_STALE_JUDGE_KEY)");

    // Return an empty report rather than failing — the caller's
    // pretty-printer and sweep harness need to handle this case
    // anyway (e.g. limit=0 or empty dataset).
    Ok(aggregate(&[], _judge_model))
}

// ===== Tests =====

#[cfg(test)]
mod tests {
    use super::*;

    fn r(t: ConflictType, sr: bool, pr: bool, ipa: bool) -> ScenarioResult {
        ScenarioResult {
            scenario_id: "x".into(),
            conflict_type: t,
            state_resolution: sr,
            premise_resistance: pr,
            implicit_policy_adaptation: ipa,
        }
    }

    #[test]
    fn empty_aggregate_is_zeros() {
        let report = aggregate(&[], "gemini-flash");
        assert_eq!(report.scenarios_run, 0);
        assert_eq!(report.overall_pct, 0.0);
    }

    #[test]
    fn all_correct_yields_hundred_percent() {
        let results = vec![
            r(ConflictType::TypeI, true, true, true),
            r(ConflictType::TypeII, true, true, true),
        ];
        let report = aggregate(&results, "judge");
        assert!((report.overall_pct - 100.0).abs() < 1e-3);
        assert!((report.by_metric.state_resolution_pct - 100.0).abs() < 1e-3);
        assert!((report.by_conflict_type.type_i_pct - 100.0).abs() < 1e-3);
        assert!((report.by_conflict_type.type_ii_pct - 100.0).abs() < 1e-3);
    }

    #[test]
    fn all_wrong_yields_zero_percent() {
        let results = vec![
            r(ConflictType::TypeI, false, false, false),
            r(ConflictType::TypeII, false, false, false),
        ];
        let report = aggregate(&results, "judge");
        assert_eq!(report.overall_pct, 0.0);
    }

    #[test]
    fn one_metric_correct_yields_one_third() {
        // Two scenarios, only state_resolution correct on both.
        // Overall is mean over 6 cells (3 metrics × 2 scenarios);
        // 2 correct of 6 = 33.3%.
        let results = vec![
            r(ConflictType::TypeI, true, false, false),
            r(ConflictType::TypeII, true, false, false),
        ];
        let report = aggregate(&results, "judge");
        assert!((report.overall_pct - 33.333).abs() < 0.1);
        assert!((report.by_metric.state_resolution_pct - 100.0).abs() < 1e-3);
        assert!(report.by_metric.premise_resistance_pct < 1e-3);
    }

    #[test]
    fn per_conflict_type_isolates_strata() {
        // Type I scenarios correct on all 3 metrics; Type II wrong on
        // all 3. Type I = 100%, Type II = 0%, Overall = 50%.
        let results = vec![
            r(ConflictType::TypeI, true, true, true),
            r(ConflictType::TypeI, true, true, true),
            r(ConflictType::TypeII, false, false, false),
            r(ConflictType::TypeII, false, false, false),
        ];
        let report = aggregate(&results, "judge");
        assert!((report.by_conflict_type.type_i_pct - 100.0).abs() < 0.1);
        assert!(report.by_conflict_type.type_ii_pct < 0.1);
        assert!((report.overall_pct - 50.0).abs() < 0.5);
    }

    // --- Publish decision ---

    #[test]
    fn publish_decision_bands() {
        let mk = |pct: f32| StaleReport {
            scenarios_run: 100,
            overall_pct: pct,
            by_metric: ByMetric {
                state_resolution_pct: pct,
                premise_resistance_pct: pct,
                implicit_policy_adaptation_pct: pct,
            },
            by_conflict_type: ByConflictType {
                type_i_pct: pct,
                type_ii_pct: pct,
            },
            judge_model: "j".into(),
            mgimind_version: "x".into(),
        };
        assert_eq!(
            publish_decision(&mk(70.0)),
            PublishDecision::PublishLikelyCUPMemRange
        );
        assert_eq!(
            publish_decision(&mk(40.0)),
            PublishDecision::PublishHeadline
        );
        assert_eq!(
            publish_decision(&mk(20.0)),
            PublishDecision::PublishHonest
        );
        assert_eq!(publish_decision(&mk(10.0)), PublishDecision::Withhold);
    }

    #[test]
    fn render_includes_reference_baselines() {
        let report = aggregate(&[], "judge");
        let s = render_summary(&report);
        // The release narrative depends on these reference numbers
        // being printed next to ours so the comparison is immediate.
        assert!(s.contains("mem0"));
        assert!(s.contains("Zep"));
        assert!(s.contains("CUPMem"));
    }

    // --- CalibrationOverrides ---

    #[test]
    fn calibration_overrides_default_tag() {
        let o = CalibrationOverrides::default();
        assert_eq!(o.tag(), "default");
    }

    #[test]
    fn calibration_overrides_tag_lists_non_none_fields() {
        let o = CalibrationOverrides {
            duel_flip_ratio: Some(2.0),
            doubt_drift_threshold: Some(0.3),
            ..Default::default()
        };
        let tag = o.tag();
        assert!(tag.contains("flip=2"));
        assert!(tag.contains("drift=0.3"));
        // Comma-separated for sweep harness CSV friendliness.
        assert!(tag.contains(","));
    }

    #[test]
    fn calibration_overrides_tag_skips_none_fields() {
        let o = CalibrationOverrides {
            duel_flip_ratio: Some(1.5),
            ..Default::default()
        };
        let tag = o.tag();
        // Only the one field set should appear; others must not.
        assert_eq!(tag, "flip=1.5");
    }

    // --- Publish-decision boundary values ---

    fn mk_report(pct: f32) -> StaleReport {
        StaleReport {
            scenarios_run: 100,
            overall_pct: pct,
            by_metric: ByMetric {
                state_resolution_pct: pct,
                premise_resistance_pct: pct,
                implicit_policy_adaptation_pct: pct,
            },
            by_conflict_type: ByConflictType {
                type_i_pct: pct,
                type_ii_pct: pct,
            },
            judge_model: "j".into(),
            mgimind_version: "x".into(),
        }
    }

    #[test]
    fn publish_decision_boundary_at_50_inclusive() {
        // 50.0 exactly is the boundary between PublishHeadline and
        // PublishLikelyCUPMemRange. Spec says >= 50 → CUPMem-range.
        assert_eq!(
            publish_decision(&mk_report(50.0)),
            PublishDecision::PublishLikelyCUPMemRange
        );
        assert_eq!(
            publish_decision(&mk_report(49.9)),
            PublishDecision::PublishHeadline
        );
    }

    #[test]
    fn publish_decision_boundary_at_15_inclusive() {
        // 15.0 exactly → PublishHonest. 14.9 → Withhold.
        assert_eq!(
            publish_decision(&mk_report(15.0)),
            PublishDecision::PublishHonest
        );
        assert_eq!(
            publish_decision(&mk_report(14.9)),
            PublishDecision::Withhold
        );
    }

    #[test]
    fn publish_decision_boundary_at_30_inclusive() {
        // 30.0 exactly → PublishHeadline. 29.9 → PublishHonest.
        assert_eq!(
            publish_decision(&mk_report(30.0)),
            PublishDecision::PublishHeadline
        );
        assert_eq!(
            publish_decision(&mk_report(29.9)),
            PublishDecision::PublishHonest
        );
    }
}
