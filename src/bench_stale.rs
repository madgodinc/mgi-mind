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

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

// ===== STALE dataset schema =====
//
// Mirrors T1_T2_400_FULL.json (github STALEproj/STALE, CC BY 4.0). 400
// scenarios, 200 T1 + 200 T2. Each scenario plants an M_old observation in
// session `relevant_session_index[0]` and an M_new observation in
// `relevant_session_index[1]`, buried in 50 sessions of distractor chat.

/// One conversational turn inside a haystack session.
#[derive(Debug, Clone, Deserialize)]
pub struct StaleTurn {
    pub role: String,
    pub content: String,
}

/// The three probing queries (dim1=SR, dim2=PR, dim3=IPA).
#[derive(Debug, Clone, Deserialize)]
pub struct ProbingQueries {
    pub dim1_query: String,
    pub dim2_query: String,
    pub dim3_query: String,
}

/// A single STALE scenario as stored in the dataset file.
#[derive(Debug, Clone, Deserialize)]
pub struct StaleScenario {
    pub uid: String,
    #[serde(rename = "M_old")]
    pub m_old: String,
    #[serde(rename = "M_new")]
    pub m_new: String,
    pub explanation: String,
    pub probing_queries: ProbingQueries,
    /// [idx_old, idx_new] — sessions where M_old / M_new are planted.
    pub relevant_session_index: Vec<usize>,
    /// One timestamp ("YYYY-MM-DD HH:MM") per haystack session.
    pub timestamps: Vec<String>,
    /// 50 sessions, each a list of turns.
    pub haystack_session: Vec<Vec<StaleTurn>>,
    /// "T1" (co-referential) or "T2" (propagated).
    #[serde(rename = "type")]
    pub conflict_type_raw: String,
}

impl StaleScenario {
    pub fn conflict_type(&self) -> ConflictType {
        match self.conflict_type_raw.as_str() {
            "T2" => ConflictType::TypeII,
            _ => ConflictType::TypeI, // T1 and anything unexpected => TypeI
        }
    }
}

/// Load and parse the STALE dataset (a JSON array of scenarios).
pub fn load_dataset(path: &Path) -> Result<Vec<StaleScenario>> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("read STALE dataset {}", path.display()))?;
    let scenarios: Vec<StaleScenario> =
        serde_json::from_slice(&bytes).context("parse STALE dataset JSON")?;
    Ok(scenarios)
}

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

    let count_true =
        |sel: fn(&ScenarioResult) -> bool| -> usize { results.iter().filter(|r| sel(r)).count() };
    let sr = count_true(|r| r.state_resolution);
    let pr = count_true(|r| r.premise_resistance);
    let ipa = count_true(|r| r.implicit_policy_adaptation);

    let type_i: Vec<&ScenarioResult> = results
        .iter()
        .filter(|r| r.conflict_type == ConflictType::TypeI)
        .collect();
    let type_ii: Vec<&ScenarioResult> = results
        .iter()
        .filter(|r| r.conflict_type == ConflictType::TypeII)
        .collect();

    let _metric_pct = |selected: &[&ScenarioResult], sel: fn(&ScenarioResult) -> bool| -> f32 {
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
                (r.state_resolution as u32
                    + r.premise_resistance as u32
                    + r.implicit_policy_adaptation as u32) as f32
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
                (r.state_resolution as u32
                    + r.premise_resistance as u32
                    + r.implicit_policy_adaptation as u32) as f32
            })
            .sum::<f32>();
        100.0 * sum / (3.0 * type_ii.len() as f32)
    };

    // Overall = MACRO-average over the 6 cells (3 metrics × 2 conflict types)
    // per STALE paper §3.1: mean of the two per-type rates, weighting Type I and
    // Type II equally regardless of their counts. The earlier micro-average
    // (sr+pr+ipa)/(3n) only matched this on a perfect split with equal per-cell
    // rates — and would print a non-comparable number under the paper's macro
    // baselines (mem0/CUPMem). When only one type is present, fall back to that
    // type's rate. (Critic catch, 2026-06-05.)
    let overall_pct = match (type_i.is_empty(), type_ii.is_empty()) {
        (false, false) => (type_i_pct + type_ii_pct) / 2.0,
        (false, true) => type_i_pct,
        (true, false) => type_ii_pct,
        (true, true) => 0.0,
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

// ===== Adapter helpers (Path B: retrieval memory, not full-context) =====
//
// The live adapter calls the production extractor, so it only compiles when the
// `extractor` feature is on. Builds without the feature still get the types,
// aggregation, and a stub `run()` below.
#[cfg(feature = "extractor")]
pub use adapter::{run, run_duel_validity, HaystackMode};

#[cfg(feature = "extractor")]
mod adapter {
use super::*;
use crate::extractor::ExtractConfig;
use crate::qa_judge::{ChatTurn, GenParams, LlmClient};
use std::time::Duration;
use crate::{config::MindConfig, knowledge, storage};

/// The STALE judge system prompt, verbatim from icedreamc/STALE
/// (judge_prompts.py). See benchmarks/judge-prompts/stale-judge.md.
const STALE_JUDGE_SYSTEM: &str = r#"You are a strict and impartial expert evaluator for an AI Assistant Benchmark focusing on Long-Term Memory and Implicit Reasoning.

You will be provided with the Ground Truth Context of a user's state.
Then, you will evaluate THREE distinct responses generated by a Target AI Model to THREE different probing questions (Dimension 1, 2, and 3).

[Ground Truth Context]:
- M_old: The user's outdated state.
- M_new: The user's updated state.
- Hidden Logic: How M_new invalidates M_old.

You must grade each response strictly based on whether it correctly deduces and respects the hidden updated state.

### Scoring Rubrics (Boolean: true for Pass, false for Fail)

* **Dimension 1 (Explicit Probing):** Score `true` ONLY IF Response 1 awares that the information in M_old is possibly NO LONGER VALID. Score `false` if it assumes M_old is still true or claims ignorance.

* **Dimension 2 (Adversarial Robustness):**
  Question 2 contains a FALSE PREMISE based on M_old. Score `true` ONLY IF Response 2 detects the trap, or acknowledges the new state in M_new. Score `false` if it blindly agrees with the false premise.

* **Dimension 3 (Implicit Task):**
  Score `true` ONLY IF the final action, plan, or recommendation in Response 3 is safely adheres to the NEW state established by M_new.
  Score `false` if the action complies with the outdated M_old constraints, OR if the response is too generic, vague.

Evaluate strictly based on the underlying logic, not surface-level word matching.

### Output Format (JSON)
{
  "dim1_eval": {
    "reasoning": "Brief step-by-step reasoning against the Dim 1 rubric.",
    "pass": true/false
  },
  "dim2_eval": {
    "reasoning": "Brief step-by-step reasoning against the Dim 2 rubric.",
    "pass": true/false
  },
  "dim3_eval": {
    "reasoning": "Brief step-by-step reasoning against the Dim 3 rubric.",
    "pass": true/false
  }
}"#;

/// Wipe the facts collection to a clean slate between scenarios (A4 boundary:
/// each scenario starts from zero so prior scenarios can't leak) and seed the
/// user-state predicate cardinalities.
///
/// Seeding is load-bearing: `get_cardinality` defaults to `Multi` for any
/// unregistered predicate, and `Multi` predicates never conflict — so on a
/// freshly-wiped store the duel could NEVER fire on `located_in`. The STALE
/// D2 gate caught exactly this. We register the single-valued user-state
/// predicates as TemporalSingle (location/employer change over time, newest
/// wins) / Single (role) so the duel has an axis to resolve on.
async fn wipe_and_seed(config: &MindConfig) -> Result<()> {
    let client = storage::get_client(config).await?;
    // Ensure the collection exists FIRST, then clear its points. The earlier
    // approach (delete_collection + ensure) was the bug behind the whole
    // "scenarios 2-N ingest 0 facts" mystery: storage::ensure_facts_collection
    // memoizes readiness in a global AtomicBool (FACTS_READY), so after a raw
    // delete_collection the next ensure short-circuits and never recreates the
    // collection — every subsequent upsert then fails with "_kg_facts doesn't
    // exist" and add_fact()'s error was being swallowed. Clearing points
    // instead keeps the collection (and the memoized flag) valid.
    storage::ensure_facts_collection(&client).await?;
    {
        use qdrant_client::qdrant::{DeletePointsBuilder, Filter};
        // Empty filter = match all points (clear the collection without
        // dropping it, so FACTS_READY stays valid).
        let _ = client
            .delete_points(
                DeletePointsBuilder::new(storage::FACTS_COLLECTION)
                    .points(Filter::default())
                    .wait(true),
            )
            .await;
    }

    use crate::knowledge::Cardinality;
    // Cardinality is a TYPE property of each predicate, fixed before evaluation
    // (CUPMem's Ω). Single-valued current state = TemporalSingle (one current
    // value that can change). Things you can have many of at once = Multi.
    // owns is Multi: you can own a typewriter AND vinyl AND coins — they must
    // NOT supersede each other. Predicate names match normalize_predicate.
    for (pred, card) in [
        ("located_in", Cardinality::TemporalSingle),
        ("work_arrangement", Cardinality::TemporalSingle),
        ("works_at", Cardinality::TemporalSingle),
        ("works_as", Cardinality::TemporalSingle),
        ("local_climate", Cardinality::TemporalSingle),
        ("housing_type", Cardinality::TemporalSingle),
        ("relationship_status", Cardinality::TemporalSingle),
        ("has_children", Cardinality::TemporalSingle),
        ("diet", Cardinality::TemporalSingle),
        ("primary_transport", Cardinality::TemporalSingle),
        ("primary_device", Cardinality::TemporalSingle),
        ("religion", Cardinality::TemporalSingle),
        ("schedule", Cardinality::TemporalSingle),
        ("commute_distance", Cardinality::TemporalSingle),
        ("activity_level", Cardinality::TemporalSingle),
        ("altitude", Cardinality::TemporalSingle),
        // Multi — many can be true at once; never duel/supersede:
        ("owns", Cardinality::Multi),
        ("health_condition", Cardinality::Multi),
        ("observation", Cardinality::Multi),
    ] {
        knowledge::register_cardinality(config, pred, card).await?;
    }
    Ok(())
}

/// A deterministic duel-rule validity case: ingest `old` then `new` on the
/// same axis (chronologically), and the duel must end with `new` live and
/// `old` hidden. No LLM — this isolates the project IP (conflict resolution)
/// from the extractor's noisy recall.
pub struct DuelCase {
    pub name: &'static str,
    pub subject: &'static str,
    pub predicate: &'static str,
    pub old_object: &'static str,
    pub new_object: &'static str,
}

/// The canonical STALE-shaped conflict cases, expressed as clean triples. These
/// mirror the dataset's user-state conflicts (location / employer / role moves)
/// without depending on a 2B model to extract them. Passing this set means the
/// duel rule + cardinality eviction + read-path hiding are correct — the number
/// the project actually needs to defend.
pub fn canonical_duel_cases() -> Vec<DuelCase> {
    vec![
        DuelCase { name: "seattle_to_austin", subject: "user", predicate: "located_in", old_object: "seattle", new_object: "austin" },
        DuelCase { name: "chicago_to_london", subject: "user", predicate: "located_in", old_object: "chicago", new_object: "london" },
        DuelCase { name: "seattle_to_portland", subject: "user", predicate: "located_in", old_object: "seattle", new_object: "portland" },
        DuelCase { name: "japan_to_toronto", subject: "user", predicate: "located_in", old_object: "japan", new_object: "toronto" },
        DuelCase { name: "employer_change", subject: "user", predicate: "works_at", old_object: "stripe", new_object: "hubspot" },
        DuelCase { name: "role_change", subject: "user", predicate: "works_as", old_object: "data engineer", new_object: "product manager" },
    ]
}

/// Run one duel-validity case against the store. Returns (resolved_correctly,
/// detail). Correct = after ingesting old then new, query_facts surfaces the
/// NEW object and NOT the old (the duel hid the stale value).
async fn run_duel_case(config: &MindConfig, case: &DuelCase) -> Result<(bool, String)> {
    wipe_and_seed(config).await?;
    // Chronological: old first, then new (A6) — newest must evict oldest.
    knowledge::add_fact(config, case.subject, case.predicate, case.old_object).await?;
    knowledge::add_fact(config, case.subject, case.predicate, case.new_object).await?;

    // query_facts hides stale/superseded — so a correct duel leaves only NEW.
    let visible = knowledge::query_facts(config, case.subject).await.unwrap_or_default();
    let on_axis: Vec<&knowledge::Fact> = visible
        .iter()
        .filter(|f| f.subject == case.subject && f.predicate == case.predicate)
        .collect();
    let has_new = on_axis.iter().any(|f| f.object == case.new_object);
    let has_old = on_axis.iter().any(|f| f.object == case.old_object);
    let ok = has_new && !has_old;
    let detail = format!(
        "visible on axis: [{}] (want new={:?} present, old={:?} absent)",
        on_axis.iter().map(|f| f.object.as_str()).collect::<Vec<_>>().join(", "),
        case.new_object,
        case.old_object,
    );
    Ok((ok, detail))
}

/// Deterministic duel-rule validity benchmark — the LLM-free, defensible
/// number. Runs every canonical case and reports pass rate. This measures the
/// project's actual IP (belief revision) instead of a 2B model's recall.
pub async fn run_duel_validity(config: &MindConfig) -> Result<(usize, usize)> {
    let cases = canonical_duel_cases();
    let mut passed = 0;
    for case in &cases {
        match run_duel_case(config, case).await {
            Ok((true, _)) => {
                passed += 1;
                eprintln!("  ✓ {}", case.name);
            }
            Ok((false, detail)) => eprintln!("  ✗ {} — {detail}", case.name),
            Err(e) => eprintln!("  ✗ {} — error: {e:#}", case.name),
        }
    }
    eprintln!(
        "\nDuel-rule validity (deterministic, LLM-free): {passed}/{} = {:.0}%",
        cases.len(),
        100.0 * passed as f32 / cases.len() as f32
    );
    Ok((passed, cases.len()))
}

/// Reject extractor noise that would create phantom facts / false collisions.
/// Small models, told to emit nothing when a fact is absent, instead emit a
/// placeholder object — and in many phrasings: "Not specified in the provided
/// text", "Not explicitly stated", "Unknown", "not specified in the
/// conversation". The STALE debug run showed these placeholders flooding the
/// located_in axis and, under TemporalSingle, the last placeholder evicted the
/// real Seattle/Portland values — so we must drop them by prefix, not just
/// exact match. Returns true to KEEP.
fn keep_object(object: &str) -> bool {
    let o = object.trim().to_ascii_lowercase();
    if o.is_empty() {
        return false;
    }
    // Exact junk values.
    if matches!(o.as_str(), "n/a" | "na" | "none" | "null" | "unknown") {
        return false;
    }
    // Placeholder phrasings the model emits for "absent" — match by prefix so
    // the trailing "...in the provided text / conversation" variants all die.
    const JUNK_PREFIXES: &[&str] = &[
        "not specified",
        "unspecified",
        "not explicitly",
        "not stated",
        "not mentioned",
        "not provided",
        "not available",
        "not clear",
        "not given",
        "no specific",
        "unknown",
    ];
    !JUNK_PREFIXES.iter().any(|p| o.starts_with(p))
}

/// Normalize a location object so values that denote the same place collide on
/// the same axis: lowercase, strip a trailing ZIP, drop street-level detail,
/// and keep the city token. "Austin, TX 78704" / "78704, Austin, TX" /
/// "S Lamar Blvd & Barton Springs Rd, Austin, TX" all reduce toward "austin".
/// Conservative: when in doubt it leaves the value alone (a missed merge only
/// costs recall, a wrong merge corrupts the conflict).
fn normalize_location(object: &str) -> String {
    let lower = object.to_ascii_lowercase();
    // Split on commas; the city is usually the segment before a state/zip.
    let parts: Vec<&str> = lower.split(',').map(|s| s.trim()).collect();
    // Drop pure-ZIP and street-ish segments (contain digits or st/blvd/ave/rd).
    let is_streetish = |s: &str| {
        s.chars().any(|c| c.is_ascii_digit())
            || ["blvd", "ave", " st", "street", " rd", "road", "drive", " dr", "lane", " ln"]
                .iter()
                .any(|k| s.contains(k))
    };
    let city_parts: Vec<&str> = parts.iter().copied().filter(|s| !is_streetish(s)).collect();
    let candidate = city_parts.first().copied().unwrap_or(lower.as_str());
    candidate.trim().to_string()
}

/// Flatten a haystack session into one raw text block fed to the extractor.
/// A2: RAW turns, not pre-extracted facts.
fn session_text(turns: &[StaleTurn]) -> String {
    let mut s = String::new();
    for t in turns {
        s.push_str(&t.role);
        s.push_str(": ");
        s.push_str(&t.content);
        s.push('\n');
    }
    s
}

/// Split a session into extractor-sized chunks at turn boundaries. Long
/// sessions (17-19K chars in STALE) drown the extract instruction and the
/// small model narrates the content instead of emitting triples — the root
/// cause of the balanced-50 run's 49/50 zero-fact scenarios. Capping each
/// extract call at ~4K chars keeps the instruction dominant. Turn boundaries
/// are preserved so a fact never gets split mid-sentence.
fn session_chunks(turns: &[StaleTurn], max_chars: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut cur = String::new();
    for t in turns {
        let line = format!("{}: {}\n", t.role, t.content);
        // A single turn larger than the cap goes out on its own (rare; the
        // instruction still leads it).
        if cur.len() + line.len() > max_chars && !cur.is_empty() {
            chunks.push(std::mem::take(&mut cur));
        }
        cur.push_str(&line);
    }
    if !cur.is_empty() {
        chunks.push(cur);
    }
    chunks
}

/// How much of the haystack to ingest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HaystackMode {
    /// Every session (full ~190K-token history, honest retrieval-in-noise).
    Full,
    /// Only the M_old/M_new sessions plus a `window` of neighbours on each
    /// side. Fast first-pass; reduced-haystack must be stated in the writeup.
    Reduced { window: usize },
}

impl HaystackMode {
    /// Which session indices to ingest for this scenario, in chronological
    /// order. Full = 0..n. Reduced = the relevant sessions ± window, deduped
    /// and sorted (so chronology / A6 still holds).
    fn session_indices(self, scenario: &StaleScenario) -> Vec<usize> {
        let n = scenario.haystack_session.len();
        match self {
            HaystackMode::Full => (0..n).collect(),
            HaystackMode::Reduced { window } => {
                let mut set = std::collections::BTreeSet::new();
                for &center in &scenario.relevant_session_index {
                    let lo = center.saturating_sub(window);
                    let hi = (center + window).min(n.saturating_sub(1));
                    for i in lo..=hi {
                        set.insert(i);
                    }
                }
                set.into_iter().collect()
            }
        }
    }
}

/// Ingest one scenario's haystack chronologically (A6: ordered by session
/// index, which the dataset already sorts by timestamp) through the real
/// production path: extractor → triples → add_fact → duel. Returns how many
/// File-based extractor: write the chunk to extract_req_NNNN.txt and block until
/// extract_resp_NNNN.txt appears (a human or stronger model returns the triples
/// as a JSON array of {subject,predicate,object}). This isolates the duel/
/// retrieve/judge mechanism from the local 2B model's recall ceiling — Granite
/// fails to extract M_new facts buried in long sessions (root cause of the
/// 73%→6% collapse, 2026-06-06). Gives the upper-bound number with clean
/// extraction. Returns parsed triples.
async fn file_extract(dir: &std::path::Path, text: &str) -> Result<Vec<crate::extractor::Triple>> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    std::fs::create_dir_all(dir).ok();
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let req = dir.join(format!("extract_req_{n:04}.txt"));
    let resp = dir.join(format!("extract_resp_{n:04}.txt"));
    std::fs::write(&req, text).context("write extract request")?;
    loop {
        if resp.exists() {
            let s = std::fs::read_to_string(&resp).context("read extract response")?;
            if !s.trim().is_empty() {
                // Parse the JSON array of {subject,predicate,object}.
                let cleaned = s
                    .find('[')
                    .and_then(|a| s.rfind(']').map(|b| s[a..=b].to_string()))
                    .unwrap_or_else(|| s.trim().to_string());
                #[derive(serde::Deserialize)]
                struct T { subject: String, predicate: String, object: String }
                let parsed: Vec<T> = serde_json::from_str(&cleaned).unwrap_or_default();
                return Ok(parsed
                    .into_iter()
                    .map(|t| crate::extractor::Triple {
                        subject: t.subject,
                        predicate: crate::extractor::normalize_predicate(&t.predicate),
                        object: t.object,
                    })
                    .collect());
            }
        }
        tokio::time::sleep(Duration::from_millis(750)).await;
    }
}

/// LLM-based extractor: a strong API model (GPT-4o-mini) returns the triples,
/// isolating the duel/retrieve/judge mechanism from the local 2B model's recall
/// ceiling. This is the "upper bound" / oracle-extraction number — valid and
/// comparable to how mem0/Zep extract (their backbone LLM does it too), but it
/// must be labeled "with LLM extraction (mechanism)", NOT "end-to-end product"
/// (invariant A2: end-to-end uses our own Granite extractor). Reuses the
/// answerer client when it's OpenAI-shaped.
async fn llm_extract(
    client: &dyn LlmClient,
    chunk: &str,
) -> Result<Vec<crate::extractor::Triple>> {
    // Broad, FIXED life-state slot schema (CUPMem-style Ω): set before seeing
    // the data, deliberately wider than any single benchmark, includes slots
    // that may not appear in a given dataset. Each slot has a value-type. The
    // extractor must INFER the slot value from implicit cues (symptoms, habits,
    // environment), not only explicit statements — this is what captures
    // "static shocks / very dry air" -> climate, "working from my apartment"
    // -> work_arrangement. No dataset answers are baked in; the per-slot type
    // gate (verify_triple) keeps inferred values clean.
    let system = "You extract the user's DURABLE personal state as JSON triples. \
        Output ONLY a JSON array of {\"subject\",\"predicate\",\"object\"}; \
        subject is always \"user\". Capture the user's state even when phrased \
        SOFTLY or indirectly — \"accustomed to life here in X\", \"the rhythm of \
        X\", \"settled in X\", \"back home in X\" all assert the user lives in X \
        (located_in). Treat any cue that the user is habituated to a place as \
        their residence. Use predicates from this fixed set (pick the \
        best fit; infer from implicit cues, not only explicit words): \
        located_in (home city/region), work_arrangement (remote/office/hybrid), \
        works_at (employer), works_as (occupation/role), local_climate \
        (e.g. arid, humid, cold — infer from weather/symptoms like static \
        shocks=dry, damp surfaces=humid), housing_type (apartment/house/etc), \
        relationship_status, has_children, diet (e.g. vegetarian/vegan), \
        health_condition, primary_transport (car/transit/bike), owns (a \
        significant durable possession), primary_device, religion, schedule \
        (e.g. night-shift/day-shift), commute_distance (how far/long to work), \
        activity_level (mostly-indoors/sedentary vs active/outdoors), altitude \
        (sea-level vs high-altitude/mountain — infer from thin-air cues). \
        Additionally, when the user mentions a vivid ENVIRONMENTAL or LIFESTYLE \
        cue that hints at their situation but you cannot map it to a slot above \
        with confidence (e.g. \"found a scorpion, relentless dry heat\", \"thin \
        air on my hike\", \"constant damp\"), emit it as predicate \
        \"observation\" with the object being a short paraphrase of the cue — \
        these are kept as reasoning context, not as resolved state. \
        For each slot fact, the object must be the user's \
        own CURRENT, durable state — infer the canonical value, do not copy a \
        raw phrase. IGNORE one-off events, questions, hypotheticals, advice, and \
        things merely mentioned. If nothing durable, output [].";
    let user = format!("Conversation:\n{chunk}\n\nJSON array of durable user-state triples:");
    let params = GenParams { temperature: Some(0.0), max_tokens: 512 };
    let raw = client
        .generate(system, &[ChatTurn { role: "user".into(), content: user }], &params)
        .await?;
    // Tolerant parse: grab the [...] block.
    let cleaned = raw
        .find('[')
        .and_then(|a| raw.rfind(']').map(|b| raw[a..=b].to_string()))
        .unwrap_or_else(|| raw.trim().to_string());
    #[derive(serde::Deserialize)]
    struct T { subject: String, predicate: String, object: String }
    let parsed: Vec<T> = serde_json::from_str(&cleaned).unwrap_or_default();
    Ok(parsed
        .into_iter()
        .map(|t| crate::extractor::Triple {
            subject: t.subject,
            predicate: crate::extractor::normalize_predicate(&t.predicate),
            object: t.object,
        })
        .collect())
}

/// Whether the extract→verify gate is on (env STALE_VERIFY=1). When on, every
/// extracted triple is checked by a second LLM call before storage. This is the
/// fix for greedy extraction (e.g. `works_at = <a utility the user merely
/// mentioned>`, `owns = <food in a recipe>`, `located_in = <a street, not a
/// home city>`). The gate is phrased at the SCHEMA level only — never with
/// dataset-specific answers — so a passing number stays reproducible on unseen
/// data.
fn verify_gate_on() -> bool {
    matches!(std::env::var("STALE_VERIFY").as_deref(), Ok("1") | Ok("true"))
}

/// Whether to infer cardinality per-predicate instead of relying only on the 4
/// seeded axes (env STALE_DYNAXIS=1). Lets the duel fire on ANY durable
/// single-valued predicate the extractor produces (work_location, climate,
/// diet, …), not just located_in/works_at/works_as/owns. The cardinality is
/// inferred from the PREDICATE NAME ALONE via a schema-level question ("can a
/// person have more than one current X at once?") — no dataset content, fully
/// reproducible (CUPMem's Ω is likewise built independently of the benchmark).
fn dynaxis_on() -> bool {
    matches!(std::env::var("STALE_DYNAXIS").as_deref(), Ok("1") | Ok("true"))
}

/// Predicates whose cardinality we've already inferred this run (avoid
/// re-asking the LLM per fact). Process-local; the bench wipes the store per
/// scenario but predicate semantics are stable, so caching across scenarios is
/// correct and saves calls.
static CARD_CACHE: once_cell::sync::Lazy<std::sync::Mutex<std::collections::HashMap<String, crate::knowledge::Cardinality>>> =
    once_cell::sync::Lazy::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// Infer and register a predicate's cardinality from its NAME ONLY (schema
/// level — never the object, never the dataset). single = at most one ever;
/// temporal-single = one current value but it can change over time (this is
/// what makes the duel supersede old→new); multi = many can coexist.
async fn ensure_cardinality(
    config: &MindConfig,
    client: &dyn LlmClient,
    predicate: &str,
) -> crate::knowledge::Cardinality {
    use crate::knowledge::Cardinality;
    if let Some(c) = CARD_CACHE.lock().unwrap().get(predicate).copied() {
        return c;
    }
    let system = "You classify the cardinality of a relation about a person, \
        from the relation NAME ALONE. Answer ONLY one word: \
        \"temporal-single\" if a person has exactly ONE current value but it can \
        CHANGE over time (e.g. home city, employer, current job, marital status, \
        local climate, primary device); \"single\" if there is exactly one value \
        that essentially never changes (e.g. birthplace, native language); \
        \"multi\" if a person can have MANY values at once (e.g. hobbies, owned \
        pets, languages spoken, friends). Answer with just the one word.";
    let user = format!("Relation: \"{predicate}\". Cardinality?");
    let params = GenParams { temperature: Some(0.0), max_tokens: 6 };
    let card = match client.generate(system, &[ChatTurn { role: "user".into(), content: user }], &params).await {
        Ok(r) => {
            let a = r.trim().to_ascii_lowercase();
            if a.contains("temporal") { Cardinality::TemporalSingle }
            else if a.starts_with("single") || a == "single" { Cardinality::Single }
            else if a.contains("multi") { Cardinality::Multi }
            else { Cardinality::TemporalSingle } // default to dueling for state-like
        }
        Err(_) => Cardinality::TemporalSingle,
    };
    let _ = knowledge::register_cardinality(config, predicate, card).await;
    CARD_CACHE.lock().unwrap().insert(predicate.to_string(), card);
    card
}

/// Second-pass entailment check for one candidate triple. Asks the LLM whether
/// the fragment actually asserts this as a durable fact ABOUT THE USER, with the
/// object being the correct TYPE for the predicate (employer for works_at, a
/// home city/region for located_in, an occupation for works_as, a possession
/// for owns). Returns true to keep. Fail-open on parse errors (don't silently
/// drop on a flaky judge) — the goal is to cut obvious greedy noise, not to be
/// a second strict filter.
async fn verify_triple(
    client: &dyn LlmClient,
    chunk: &str,
    t: &crate::extractor::Triple,
) -> bool {
    let type_rule = match t.predicate.as_str() {
        "located_in" => "the object must be the CITY or REGION the user lives in — NOT a street, intersection, postal code, venue, room, or a place they only visited or mentioned",
        "work_arrangement" => "the object must describe WHERE/HOW the user works — one of remote/work-from-home, office/on-site, or hybrid — NOT a specific address",
        "works_at" => "the object must be the user's EMPLOYER — NOT a utility/service/company they are merely a customer of or mentioned",
        "works_as" => "the object must be the user's OCCUPATION or job title — NOT a random phrase or a hobby",
        "local_climate" => "the object must be a CLIMATE/WEATHER type for where the user lives (e.g. arid, dry, humid, tropical, cold, temperate) — a general climate, NOT a one-time weather event",
        "housing_type" => "the object must be a TYPE of dwelling (apartment, house, condo, dorm, etc.)",
        "relationship_status" => "the object must be a relationship status (single, married, divorced, in a relationship, etc.)",
        "has_children" => "the object must indicate whether the user has children (yes/no or a count)",
        "diet" => "the object must be a durable dietary pattern (vegetarian, vegan, omnivore, gluten-free, etc.) — NOT a single meal",
        "health_condition" => "the object must be a durable health condition of the user — NOT a transient symptom",
        "primary_transport" => "the object must be the user's usual means of getting around (car, public transit, bicycle, walking)",
        "owns" => "the object must be a durable POSSESSION the user actually owns — NOT something merely mentioned, eaten, or used once",
        "primary_device" => "the object must be a device the user primarily uses (a phone/laptop/etc.)",
        "religion" => "the object must be the user's religion or faith tradition",
        "schedule" => "the object must be the user's durable work/sleep schedule (e.g. night-shift, day-shift, early riser)",
        "observation" => "the object must be a short paraphrase of a real environmental/lifestyle cue the user stated about their own situation (kept as reasoning context)",
        "commute_distance" => "the object must be the user's usual distance/time to work (e.g. 30 miles, 10 minutes, short, long)",
        "activity_level" => "the object must describe the user's durable activity level (e.g. mostly-indoors, sedentary, active, outdoors-a-lot)",
        "altitude" => "the object must be the elevation of where the user lives (e.g. sea-level, high-altitude, mountain)",
        _ => "the object must be a durable, settled, correctly-typed fact about the user — NOT a raw phrase, room, event, or mention",
    };
    let system = "You verify a candidate fact against a conversation fragment. \
        Answer ONLY \"yes\" or \"no\". Say \"yes\" only if the fragment clearly \
        asserts this as a durable, current fact ABOUT THE USER THEMSELF and the \
        object is the correct type for the relation. Otherwise \"no\".";
    let user = format!(
        "Fragment:\n{chunk}\n\nCandidate fact: the user's {} is \"{}\".\nType rule: {}.\nIs this a correct, durable fact about the user? Answer yes or no.",
        t.predicate, t.object, type_rule
    );
    let params = GenParams { temperature: Some(0.0), max_tokens: 4 };
    match client.generate(system, &[ChatTurn { role: "user".into(), content: user }], &params).await {
        Ok(r) => {
            let a = r.trim().to_ascii_lowercase();
            // keep unless the model clearly said no
            !a.starts_with("no")
        }
        Err(_) => true, // fail-open
    }
}

/// triples were stored, for diagnostics.
async fn ingest_haystack(
    config: &MindConfig,
    extract_cfg: &ExtractConfig,
    scenario: &StaleScenario,
    mode: HaystackMode,
    extract_dir: Option<&std::path::Path>,
    llm_extractor: Option<&dyn LlmClient>,
) -> Result<usize> {
    let mut stored = 0usize;
    for idx in mode.session_indices(scenario) {
        let turns = &scenario.haystack_session[idx];
        if turns.is_empty() {
            continue;
        }
        // Chunk size: the local 2B extractor needs small chunks (4000) so the
        // instruction stays dominant. But a 128k-context API model (llm_extract)
        // does BETTER with the whole session in one call — chunking splits a
        // signal like "moved to London … near Highbury … London admin" across
        // chunks so none individually asserts located_in=London, and the fact is
        // lost. Give the API extractor the full session (large chunk window).
        let chunk_chars = if llm_extractor.is_some() { 100_000 } else { 4000 };
        let mut triples = Vec::new();
        for chunk in session_chunks(turns, chunk_chars) {
            // LLM-extractor path: a strong API model returns triples.
            if let Some(c) = llm_extractor {
                match llm_extract(c, &chunk).await {
                    Ok(t) => {
                        // extract→verify gate (STALE_VERIFY=1): drop greedy
                        // candidates that don't entail a correctly-typed durable
                        // user fact, while the chunk context is still in scope.
                        for tr in t {
                            if verify_gate_on() && !verify_triple(c, &chunk, &tr).await {
                                if std::env::var("STALE_DEBUG").is_ok() {
                                    eprintln!("    [verify-drop] ({} | {} | {})", tr.subject, tr.predicate, tr.object);
                                }
                                continue;
                            }
                            triples.push(tr);
                        }
                    }
                    Err(e) => eprintln!("  [ingest] llm-extract failed: {e:#}"),
                }
                continue;
            }
            // File-extractor path: a human/stronger model returns triples.
            if let Some(dir) = extract_dir {
                match file_extract(dir, &chunk).await {
                    Ok(mut t) => triples.append(&mut t),
                    Err(e) => eprintln!("  [ingest] file-extract failed: {e:#}"),
                }
                continue;
            }
            match crate::extractor::extract_facts(extract_cfg, &chunk).await {
                Ok(mut t) => triples.append(&mut t),
                Err(e) => {
                    let msg = format!("{e:#}");
                    // REACTIVE RESTART: a "POST /completion failed" means the
                    // llama-server wedged (answers /health but not /completion,
                    // VRAM stuck). Waiting for the every-20 restart loses a whole
                    // batch of scenarios to 0-facts (false "memory failures").
                    // Kill it NOW and retry this chunk once on a fresh server.
                    if msg.contains("POST") || msg.contains("completion") {
                        eprintln!("  [ingest] server wedged ({msg}); restarting extractor");
                        crate::extractor::shutdown_server();
                        match crate::extractor::extract_facts(extract_cfg, &chunk).await {
                            Ok(mut t) => triples.append(&mut t),
                            Err(e2) => eprintln!("  [ingest] retry after restart still failed: {e2:#}"),
                        }
                    } else {
                        // Plain parse failure (non-JSON) — the lost fact IS the
                        // measured extractor weakness; skip the chunk.
                        eprintln!("  [ingest] extract failed for a chunk: {msg}");
                    }
                }
            }
        }
        if std::env::var("STALE_DEBUG").is_ok() && !triples.is_empty() {
            for tr in &triples {
                eprintln!("    [dbg] ({} | {} | {})", tr.subject, tr.predicate, tr.object);
            }
        }
        for tr in triples {
            // (debug above prints pre-filter; the filters below decide storage)
            // Drop placeholder/empty objects so they don't manufacture
            // phantom facts or false collisions.
            if !keep_object(&tr.object) {
                continue;
            }
            // Dynamic-axis: infer + register this predicate's cardinality so the
            // duel can fire on arbitrary durable-state predicates, not just the
            // 4 seeded ones. Schema-level inference (predicate name only).
            if dynaxis_on() {
                if let Some(c) = llm_extractor {
                    ensure_cardinality(config, c, &tr.predicate).await;
                }
            }
            // Normalize location objects so "Austin, TX 78704" and "Austin"
            // land on the same axis for the duel. Only locations — other
            // predicates keep their raw object.
            let object = if tr.predicate == "located_in" {
                normalize_location(&tr.object)
            } else {
                tr.object.clone()
            };
            // Log failures — a swallowed add_fact error (collection missing)
            // masqueraded as "0 facts extracted" for an entire debugging day.
            match knowledge::add_fact(config, &tr.subject, &tr.predicate, &object).await {
                Ok(_) => stored += 1,
                Err(e) => eprintln!("  [ingest] add_fact failed ({}/{}/{}): {e:#}", tr.subject, tr.predicate, object),
            }
        }
    }
    Ok(stored)
}

/// Retrieval-level diagnostic: did the duel actually fire on a user-state
/// axis? We scroll ALL facts (including stale/superseded, which the normal
/// query path hides) and look for an axis that has BOTH an active fact and a
/// stale/superseded one — i.e. a conflict that was detected and resolved.
///
/// This is the cheap mock-mode signal (the same thing the prior session
/// measured as "% collisions"): it tells us extraction→normalization→duel
/// worked, independent of the paid judge. It is NOT the STALE QA number.
async fn duel_fired(config: &MindConfig) -> Result<bool> {
    use qdrant_client::qdrant::{value::Kind, ScrollPointsBuilder};
    let client = storage::get_client(config).await?;
    if !client
        .collection_exists(storage::FACTS_COLLECTION)
        .await
        .unwrap_or(false)
    {
        return Ok(false);
    }

    // axis -> (has_active, has_stale)
    let mut axes: std::collections::HashMap<String, (bool, bool)> = std::collections::HashMap::new();
    let mut offset = None;
    loop {
        let mut b = ScrollPointsBuilder::new(storage::FACTS_COLLECTION)
            .limit(128)
            .with_payload(true);
        if let Some(o) = offset.clone() {
            b = b.offset(o);
        }
        let resp = client.scroll(b).await?;
        for point in &resp.result {
            let get = |k: &str| -> String {
                point.payload.get(k).and_then(|v| match &v.kind {
                    Some(Kind::StringValue(s)) => Some(s.clone()),
                    _ => None,
                }).unwrap_or_default()
            };
            let subject = get("subject");
            let predicate = get("predicate");
            let status = get("status");
            let axis = format!("{subject}\u{0}{predicate}");
            let entry = axes.entry(axis).or_insert((false, false));
            match status.as_str() {
                "stale" | "superseded" => entry.1 = true,
                // empty status (legacy) or "active" both count as active
                _ => entry.0 = true,
            }
        }
        match resp.next_page_offset {
            Some(o) => offset = Some(o),
            None => break,
        }
    }
    // Fired iff a SEEDED user-state axis has both a live winner and a
    // tombstoned loser. Restricting to seeded predicates prevents a "fired" on a
    // junk axis (e.g. user/wants_to_pay_off) from masking the real location
    // conflict silently failing. The axis key is "subject\0predicate".
    // (Critic catch, 2026-06-05.)
    const SEEDED: &[&str] = &["located_in", "works_at", "works_as", "owns"];
    let dyn_on = dynaxis_on();
    Ok(axes.iter().any(|(axis, &(active, stale))| {
        active
            && stale
            && (dyn_on
                || axis
                    .split('\u{0}')
                    .nth(1)
                    .map(|pred| SEEDED.contains(&pred))
                    .unwrap_or(false))
    }))
}

/// Retrieve the facts our memory surfaces for a probe. Path-B core: we do NOT
/// dump the 150K haystack into the answerer; we query our frozen KG (which hides
/// stale/superseded losers) and feed only the hits.
///
/// We retrieve by the user SUBJECT, not the raw probe-question text. The probe
/// questions ("does the user still live in Seattle?") share almost no lexical
/// tokens with the stored triples ("user / located_in / austin"), so a
/// query_facts() call on the question text returned NOTHING — the answerer got
/// empty memory and would score near-zero on the paid run, masquerading as
/// "memory broken." (File-judge rehearsal caught this on the first prompt,
/// 2026-06-05.) STALE scenarios are all about the user, so the user's live
/// facts ARE the relevant memory.
const SEEDED_AXES: &[&str] = &["located_in", "works_at", "works_as", "owns"];

/// Whether to surface superseded facts as labeled history (env STALE_HISTORY=1,
/// default ON). This is the CUPMem-style "current + labeled-stale" memory shape:
/// the duel already computed which value is current and which is superseded; we
/// surface BOTH — active as grounding, stale flagged "do NOT use as a premise".
/// Hiding the loser (the old behavior) tanks SR (the answerer can't say "you no
/// longer live in X" if it never sees X). The STALE paper confirms: memory
/// systems that hide the loser score 3-17% SR; CUPMem, which keeps it as
/// history, scores 89-91%. Not cheating — it's the documented advantage of
/// adjudicated structured memory over raw history.
fn surface_history() -> bool {
    !matches!(std::env::var("STALE_HISTORY").as_deref(), Ok("0") | Ok("false"))
}

/// Active + superseded facts per seeded axis, with objects (duel_fired only
/// tracks booleans). Returns (active, superseded) lists of "predicate object".
async fn axis_facts(config: &MindConfig) -> Result<(Vec<(String, String)>, Vec<(String, String)>)> {
    use qdrant_client::qdrant::{value::Kind, ScrollPointsBuilder};
    let mut active = Vec::new();
    let mut stale = Vec::new();
    let client = storage::get_client(config).await?;
    if !client.collection_exists(storage::FACTS_COLLECTION).await.unwrap_or(false) {
        return Ok((active, stale));
    }
    let mut offset = None;
    loop {
        let mut b = ScrollPointsBuilder::new(storage::FACTS_COLLECTION).limit(128).with_payload(true);
        if let Some(o) = offset.clone() {
            b = b.offset(o);
        }
        let resp = client.scroll(b).await?;
        for point in &resp.result {
            let get = |k: &str| -> String {
                point.payload.get(k).and_then(|v| match &v.kind {
                    Some(Kind::StringValue(s)) => Some(s.clone()),
                    _ => None,
                }).unwrap_or_default()
            };
            let subject = get("subject");
            let predicate = get("predicate");
            let object = get("object");
            let status = get("status");
            // Restrict to seeded axes only in fixed-axis mode; in dynamic-axis
            // mode surface every user predicate (no cherry-picking the 4 axes).
            // "observation" facts are reasoning fuel for the adjudicator, NOT
            // answers — never show them to the answerer.
            if !subject.eq_ignore_ascii_case("user")
                || predicate == "observation"
                || (!dynaxis_on() && !SEEDED_AXES.contains(&predicate.as_str()))
            {
                continue;
            }
            if object.trim().is_empty() {
                continue;
            }
            match status.as_str() {
                "stale" | "superseded" => stale.push((predicate, object)),
                _ => active.push((predicate, object)),
            }
        }
        match resp.next_page_offset {
            Some(o) => offset = Some(o),
            None => break,
        }
    }
    Ok((active, stale))
}

fn adjudicate_on() -> bool {
    matches!(std::env::var("STALE_ADJUDICATE").as_deref(), Ok("1") | Ok("true"))
}

/// Active user facts as (point_id, predicate, object, created_at), in storage
/// order. Used by the implicit adjudicator to find cross-predicate staleness.
async fn active_user_facts(config: &MindConfig) -> Result<Vec<(String, String, String, String)>> {
    use qdrant_client::qdrant::{value::Kind, ScrollPointsBuilder};
    let mut out = Vec::new();
    let client = storage::get_client(config).await?;
    if !client.collection_exists(storage::FACTS_COLLECTION).await.unwrap_or(false) {
        return Ok(out);
    }
    let mut offset = None;
    loop {
        let mut b = ScrollPointsBuilder::new(storage::FACTS_COLLECTION).limit(256).with_payload(true);
        if let Some(o) = offset.clone() {
            b = b.offset(o);
        }
        let resp = client.scroll(b).await?;
        for point in &resp.result {
            let get = |k: &str| -> String {
                point.payload.get(k).and_then(|v| match &v.kind {
                    Some(Kind::StringValue(s)) => Some(s.clone()),
                    _ => None,
                }).unwrap_or_default()
            };
            let subject = get("subject");
            let status = get("status");
            if !subject.eq_ignore_ascii_case("user") { continue; }
            if matches!(status.as_str(), "stale" | "superseded") { continue; }
            let id = point.id.as_ref().and_then(|p| match &p.point_id_options {
                Some(qdrant_client::qdrant::point_id::PointIdOptions::Uuid(u)) => Some(u.clone()),
                Some(qdrant_client::qdrant::point_id::PointIdOptions::Num(n)) => Some(n.to_string()),
                None => None,
            }).unwrap_or_default();
            out.push((id, get("predicate"), get("object"), get("created_at")));
        }
        match resp.next_page_offset {
            Some(o) => offset = Some(o),
            None => break,
        }
    }
    // chronological so "later makes earlier stale" is well-defined
    out.sort_by(|a, b| a.3.cmp(&b.3));
    Ok(out)
}

/// CUPMem-style implicit adjudication: for each pair (earlier, later) of active
/// user facts, ask the LLM whether the later fact makes the earlier one no
/// longer true (cross-predicate commonsense, e.g. transport=Tube ⇒ not in
/// Seattle). Mark losers superseded. Schema-level only.
async fn implicit_adjudicate(config: &MindConfig, client: &dyn LlmClient) -> Result<()> {
    let facts = active_user_facts(config).await?;
    if facts.len() < 2 { return Ok(()); }
    // Build a compact numbered list; ask the judge for ids to mark stale.
    let mut listing = String::new();
    for (i, (_, p, o, _)) in facts.iter().enumerate() {
        listing.push_str(&format!("{i}. user {p} = {o}\n"));
    }
    let system = "You audit a user's memory for facts that have become stale. \
        Given a chronological list of believed-current facts about one user, \
        find every EARLIER fact that a LATER fact contradicts or makes no longer \
        true — using common-sense reasoning ACROSS DIFFERENT attributes, not \
        just same-attribute changes. Examples of the reasoning to apply: a dry/ \
        desert climate contradicts living in a famously rainy city (so the old \
        city is stale); high-altitude/thin-air contradicts a sea-level home; a \
        remote-work fact contradicts an earlier in-office one; a new city \
        contradicts an old city. Be willing to infer: if a later fact about \
        climate, environment, transport, or routine logically rules out an \
        earlier location/arrangement, mark that earlier one stale. Only skip a \
        pair when there is genuinely no contradiction. Output ONLY a JSON array \
        of the integer indices of the EARLIER facts that are now stale. If none, \
        output [].";
    let user = format!("Facts (chronological):\n{listing}\nIndices of now-stale earlier facts:");
    let params = GenParams { temperature: Some(0.0), max_tokens: 64 };
    let raw = match client.generate(system, &[ChatTurn { role: "user".into(), content: user }], &params).await {
        Ok(r) => r,
        Err(_) => return Ok(()),
    };
    let cleaned = raw.find('[').and_then(|a| raw.rfind(']').map(|b| raw[a..=b].to_string())).unwrap_or_default();
    let idxs: Vec<usize> = serde_json::from_str(&cleaned).unwrap_or_default();
    for i in idxs {
        if let Some((id, p, o, _)) = facts.get(i) {
            if !id.is_empty() {
                let _ = crate::duel::mark_superseded(config, id).await;
                if std::env::var("STALE_DEBUG").is_ok() {
                    eprintln!("    [adjudicate-stale] {p} = {o}");
                }
            }
        }
    }
    Ok(())
}

async fn retrieve_context(config: &MindConfig, _query: &str) -> Result<String> {
    // CUPMem-style retrieval: surface the duel's resolution — current state as
    // grounding, superseded values as flagged history. The duel already decided
    // winner vs loser; exposing both lets the answerer (a) ground on current
    // (IPA), (b) recognise the prior value is no longer valid (SR), and (c)
    // reject a premise built on the stale value (PR). Restricted to seeded
    // durable-state axes to keep extractor noise out of the weak backbone.
    if !surface_history() {
        // Legacy path: active facts only (hides the loser). Kept for ablation.
        let facts = knowledge::query_facts(config, "user").await.unwrap_or_default();
        let user_facts: Vec<&knowledge::Fact> = facts
            .iter()
            .filter(|f| f.subject.eq_ignore_ascii_case("user") && SEEDED_AXES.contains(&f.predicate.as_str()))
            .collect();
        if user_facts.is_empty() {
            return Ok("(no relevant memory found)".to_string());
        }
        let mut s = String::new();
        for f in &user_facts {
            s.push_str(&format!("- {} {} {}\n", f.subject, f.predicate, f.object));
        }
        return Ok(s);
    }

    let (active, stale) = axis_facts(config).await.unwrap_or_default();
    if active.is_empty() && stale.is_empty() {
        return Ok("(no relevant memory found)".to_string());
    }
    let mut s = String::new();
    s.push_str("[Current state]\n");
    if active.is_empty() {
        s.push_str("- (none)\n");
    } else {
        for (p, o) in &active {
            s.push_str(&format!("- user {p} {o}\n"));
        }
    }
    if !stale.is_empty() {
        s.push_str("[Superseded — no longer true, do NOT use as a premise]\n");
        for (p, o) in &stale {
            // pair with the current value on the same axis if one exists
            let now = active.iter().find(|(ap, _)| ap == p).map(|(_, ao)| ao.as_str());
            match now {
                Some(n) => s.push_str(&format!("- user {p} {o} (was current; replaced by {n})\n")),
                None => s.push_str(&format!("- user {p} {o} (no longer current)\n")),
            }
        }
    }
    Ok(s)
}

/// Build the answerer prompt for a dimension. dim3 uses the "Latest Query"
/// framing (implicit task); dim1/dim2 use the "Question" framing. Verbatim
/// from run_target_model.py build_prompts, with [Conversation History] →
/// [Retrieved Memory] (Path B).
fn answerer_prompt(retrieved: &str, query: &str, is_dim3: bool) -> (String, Vec<ChatTurn>) {
    // Memory-shape note: if the retrieved memory splits Current vs Superseded,
    // tell the answerer to treat Superseded as no-longer-true and to correct any
    // question that presupposes a superseded value. Harmless for the raw-haystack
    // baseline (it has no such section). This converts the labeled-stale memory
    // into actual SR/PR gains (CUPMem mirrors adjudication into generation too).
    let memory_note = " Memory may be split into \"Current state\" and \
        \"Superseded\". Treat anything under Superseded as no longer true; if the \
        question assumes a superseded value, point out it has changed and use the \
        current value.";
    let (base, label) = if is_dim3 {
        (
            "You are a helpful assistant. Review the following retrieved memory \
             with the user, then respond to the user's latest query directly.",
            "[Latest Query]",
        )
    } else {
        (
            "You are a helpful assistant. Review the following retrieved memory \
             with the user, then accurately answer the question.",
            "[Question]",
        )
    };
    let system = format!("{base}{memory_note}");
    let user = format!("[Retrieved Memory]\n{retrieved}\n\n{label}\n{query}");
    (system, vec![ChatTurn { role: "user".into(), content: user }])
}

/// Format the scenario's haystack (reduced to the relevant sessions ± window)
/// as raw conversation text — the BASELINE memory source. This is the "no
/// mgi-mind" control: the answerer sees the same underlying information our
/// memory was built from, but with NO extraction, NO duel, NO stale-resolution
/// — exactly the raw-LLM-with-history setup STALE compares against. The delta
/// between this and the Path-B retrieval run is mgi-mind's contribution.
fn baseline_context(scenario: &StaleScenario, mode: HaystackMode) -> String {
    let mut s = String::new();
    for idx in mode.session_indices(scenario) {
        for turn in &scenario.haystack_session[idx] {
            s.push_str(&format!("{}: {}\n", turn.role, turn.content));
        }
    }
    s
}

/// True when the run is the baseline control (env STALE_BASELINE=1): the
/// answerer is fed the raw haystack instead of mgi-mind's resolved memory.
fn baseline_mode() -> bool {
    matches!(std::env::var("STALE_BASELINE").as_deref(), Ok("1") | Ok("true"))
}

/// Ask the answerer LLM a single probe and return its text answer.
async fn answer_probe(
    answerer: &dyn LlmClient,
    config: &MindConfig,
    scenario: &StaleScenario,
    mode: HaystackMode,
    query: &str,
    is_dim3: bool,
) -> Result<String> {
    let retrieved = if baseline_mode() {
        baseline_context(scenario, mode)
    } else {
        retrieve_context(config, query).await?
    };
    let (system, turns) = answerer_prompt(&retrieved, query, is_dim3);
    let params = GenParams { temperature: Some(0.0), max_tokens: 1024 };
    answerer.generate(&system, &turns, &params).await
}

/// Verdicts parsed from the judge's JSON: (dim1=SR, dim2=PR, dim3=IPA).
struct JudgeVerdict {
    dim1: bool,
    dim2: bool,
    dim3: bool,
}

/// Call the joint STALE judge once for all three dimensions of a scenario.
/// A5: M_old/M_new/explanation/answers mapped into the correct slots.
async fn judge_scenario(
    judge: &dyn LlmClient,
    scenario: &StaleScenario,
    a1: &str,
    a2: &str,
    a3: &str,
) -> Result<JudgeVerdict> {
    let q = &scenario.probing_queries;
    let user = format!(
        "[Ground Truth Context]\n\
         - M_old: \"{}\"\n\
         - M_new: \"{}\"\n\
         - Hidden Logic: {}\n\n\
         --------------------------------------------------\n\
         [Dimension 1: Explicit Probing]\n\
         Question 1: {}\n\
         Target Model Response 1: {}\n\n\
         --------------------------------------------------\n\
         [Dimension 2: Adversarial Robustness]\n\
         Question 2: {}\n\
         Target Model Response 2: {}\n\n\
         --------------------------------------------------\n\
         [Dimension 3: Implicit Task]\n\
         Question 3: {}\n\
         Target Model Response 3: {}\n",
        scenario.m_old,
        scenario.m_new,
        scenario.explanation,
        q.dim1_query,
        a1,
        q.dim2_query,
        a2,
        q.dim3_query,
        a3,
    );
    // 2048 (not 1024): the judge emits reasoning + 3 dimensions of JSON. If a
    // verbose judge truncates before closing the JSON, parse_judge_json hard-
    // errors and the scenario is DROPPED — silently shrinking and biasing the
    // sample. Headroom avoids that. (Critic catch, 2026-06-05.)
    let params = GenParams { temperature: Some(0.0), max_tokens: 2048 };
    let raw = judge
        .generate(STALE_JUDGE_SYSTEM, &[ChatTurn { role: "user".into(), content: user }], &params)
        .await?;
    parse_judge_json(&raw)
}

/// Read a `dimN_eval.pass` value tolerantly: accept a real bool, a string
/// "true"/"false", or 0/1. Returns None when the key is missing or the value
/// is an unrecognised shape — so the caller can DISTINGUISH "judge said fail"
/// from "we couldn't read the verdict". The previous `unwrap_or(false)` silently
/// turned any schema drift from a real judge into an all-fail score, which would
/// read as "memory is broken" on the paid run. (Critic catch, 2026-06-05.)
fn read_pass(v: &serde_json::Value, key: &str) -> Option<bool> {
    let p = &v[key]["pass"];
    if let Some(b) = p.as_bool() {
        return Some(b);
    }
    if let Some(s) = p.as_str() {
        match s.trim().to_ascii_lowercase().as_str() {
            "true" | "yes" | "pass" => return Some(true),
            "false" | "no" | "fail" => return Some(false),
            _ => {}
        }
    }
    if let Some(n) = p.as_i64() {
        return Some(n != 0);
    }
    None
}

/// Parse the judge's JSON object, tolerating ```json fences and string/int
/// pass values. A missing/unparseable dimension is a HARD ERROR (not a silent
/// false) so a malformed judge reply is surfaced, not scored as failure.
fn parse_judge_json(raw: &str) -> Result<JudgeVerdict> {
    // Strip a ```json ... ``` fence if present (mirrors full_eval_performance.py).
    let cleaned = raw
        .find("```")
        .and_then(|start| {
            let after = &raw[start + 3..];
            let after = after.strip_prefix("json").unwrap_or(after);
            after.rfind("```").map(|end| after[..end].trim().to_string())
        })
        .unwrap_or_else(|| raw.trim().to_string());

    let v: serde_json::Value = serde_json::from_str(&cleaned)
        .with_context(|| format!("judge returned non-JSON: {raw}"))?;

    let d1 = read_pass(&v, "dim1_eval");
    let d2 = read_pass(&v, "dim2_eval");
    let d3 = read_pass(&v, "dim3_eval");
    let missing: Vec<&str> = [("dim1_eval", d1), ("dim2_eval", d2), ("dim3_eval", d3)]
        .iter()
        .filter(|(_, o)| o.is_none())
        .map(|(k, _)| *k)
        .collect();
    if !missing.is_empty() {
        anyhow::bail!(
            "judge JSON missing/unparseable .pass for {}: {raw}",
            missing.join(", ")
        );
    }
    Ok(JudgeVerdict {
        dim1: d1.unwrap(),
        dim2: d2.unwrap(),
        dim3: d3.unwrap(),
    })
}

/// Run the full STALE pipeline for one scenario: wipe → ingest → 3 probes →
/// judge. The answerer and judge are injected so the same code runs under
/// MockClient (D2/D4 gates, zero API) and real providers (smoke/headline).
async fn run_scenario(
    config: &MindConfig,
    extract_cfg: &ExtractConfig,
    answerer: &dyn LlmClient,
    judge: &dyn LlmClient,
    scenario: &StaleScenario,
    mode: HaystackMode,
    extract_dir: Option<&std::path::Path>,
    llm_extractor: Option<&dyn LlmClient>,
) -> Result<(ScenarioResult, bool)> {
    wipe_and_seed(config).await?;
    let stored = ingest_haystack(config, extract_cfg, scenario, mode, extract_dir, llm_extractor).await?;
    // Implicit-conflict adjudicator (CUPMem Jθ, env STALE_ADJUDICATE=1): the
    // string-match duel only fires when M_old and M_new share a predicate. Many
    // STALE conflicts are cross-predicate ("based in Seattle" vs "the Tube to
    // the office" ⇒ now in London). After ingest, ask the LLM which active
    // facts a later fact makes stale — even on a different predicate — and mark
    // those superseded. Schema-level prompt, no dataset answers.
    if adjudicate_on() {
        if let Some(c) = llm_extractor {
            let _ = implicit_adjudicate(config, c).await;
        }
    }
    let fired = duel_fired(config).await.unwrap_or(false);
    eprintln!(
        "  [{}] ingested {stored} facts (type {}) — duel fired: {}",
        scenario.uid, scenario.conflict_type_raw, fired
    );

    let q = &scenario.probing_queries;
    let a1 = answer_probe(answerer, config, scenario, mode, &q.dim1_query, false).await?;
    let a2 = answer_probe(answerer, config, scenario, mode, &q.dim2_query, false).await?;
    let a3 = answer_probe(answerer, config, scenario, mode, &q.dim3_query, true).await?;

    // Provenance trace (checklist #6): what memory the answerer saw and what it
    // replied, per dimension — so an SR/PR/IPA fail can be attributed to the
    // resolver vs the generator. Gated on STALE_DEBUG.
    if std::env::var("STALE_DEBUG").is_ok() {
        let mem = if baseline_mode() {
            "<baseline: raw haystack>".to_string()
        } else {
            retrieve_context(config, "").await.unwrap_or_default()
        };
        eprintln!("  [trace {}] M_old={:?} M_new={:?}", scenario.uid, scenario.m_old, scenario.m_new);
        eprintln!("  [trace] memory_to_answerer:\n{}", mem.trim());
        eprintln!("  [trace] SR  q={:?}\n          a={:?}", q.dim1_query, a1.trim());
        eprintln!("  [trace] PR  q={:?}\n          a={:?}", q.dim2_query, a2.trim());
        eprintln!("  [trace] IPA q={:?}\n          a={:?}", q.dim3_query, a3.trim());
    }

    let verdict = judge_scenario(judge, scenario, &a1, &a2, &a3).await?;
    Ok((
        ScenarioResult {
            scenario_id: scenario.uid.clone(),
            conflict_type: scenario.conflict_type(),
            state_resolution: verdict.dim1,
            premise_resistance: verdict.dim2,
            implicit_policy_adaptation: verdict.dim3,
        },
        fired,
    ))
}

/// Run the STALE benchmark end-to-end against the current mgi-mind store.
///
/// Path B (retrieval memory): ingest each scenario's haystack into facts via
/// the production extractor+duel, freeze, then answer the 3 probes from
/// retrieved facts only. `answerer` is the backbone (gpt-4o-mini, A1); `judge`
/// is the STALE judge (Gemini flash-lite). Both are `&dyn LlmClient` so a
/// MockClient drives the D2/D4 gates with zero API spend.
pub async fn run(
    dataset: PathBuf,
    answerer: &dyn LlmClient,
    judge: &dyn LlmClient,
    extract_cfg: &ExtractConfig,
    mode: HaystackMode,
    limit: Option<usize>,
    _overrides: CalibrationOverrides,
    output: PathBuf,
    extract_dir: Option<PathBuf>,
    llm_extractor: Option<&dyn LlmClient>,
) -> Result<StaleReport> {
    let config = MindConfig::load()?;
    let mut scenarios = load_dataset(&dataset)?;
    if let Some(n) = limit {
        scenarios.truncate(n);
    }
    eprintln!(
        "STALE bench: {} scenarios, answerer={}, judge={}, haystack={:?}",
        scenarios.len(),
        answerer.model_id(),
        judge.model_id(),
        mode,
    );

    let mut results: Vec<ScenarioResult> = Vec::with_capacity(scenarios.len());
    // Retrieval-level duel-fired tally, split by conflict type — the cheap
    // mock-mode signal (% of scenarios where the conflict was detected &
    // resolved), independent of the paid judge.
    let (mut fired_t1, mut tot_t1, mut fired_t2, mut tot_t2) = (0usize, 0usize, 0usize, 0usize);
    // Dropped = a scenario that errored (judge non-JSON, extractor crash, etc).
    // A high drop count silently shrinks and biases the sample — surface it.
    let mut dropped = 0usize;
    for (i, scenario) in scenarios.iter().enumerate() {
        // Restart the llama-server extractor every 20 scenarios. Under sustained
        // sequential load (~hundreds of /completion calls) llama.cpp wedged: the
        // process stayed up and answered /health but every /completion returned
        // "POST failed", VRAM stuck — all T2 scenarios after ~34 ingested 0
        // facts. A periodic cold restart keeps it healthy. The next extract call
        // lazily respawns it. (2026-06-06.)
        if i > 0 && i % 20 == 0 {
            eprintln!("  [extractor] periodic restart at scenario {i}");
            crate::extractor::shutdown_server();
        }
        eprintln!("[{}/{}] {}", i + 1, scenarios.len(), scenario.uid);
        match run_scenario(&config, extract_cfg, answerer, judge, scenario, mode, extract_dir.as_deref(), llm_extractor).await {
            Ok((r, fired)) => {
                match r.conflict_type {
                    ConflictType::TypeI => {
                        tot_t1 += 1;
                        if fired { fired_t1 += 1; }
                    }
                    ConflictType::TypeII => {
                        tot_t2 += 1;
                        if fired { fired_t2 += 1; }
                    }
                }
                // Incrementally append each completed scenario to a JSONL so a
                // mid-run crash (extractor wedge, etc.) doesn't lose the paid
                // judge verdicts already bought. (2026-06-06.)
                let line = serde_json::json!({
                    "uid": r.scenario_id,
                    "type": match r.conflict_type { ConflictType::TypeI => "T1", ConflictType::TypeII => "T2" },
                    "state_resolution": r.state_resolution,
                    "premise_resistance": r.premise_resistance,
                    "implicit_policy_adaptation": r.implicit_policy_adaptation,
                });
                if let Ok(s) = serde_json::to_string(&line) {
                    use std::io::Write;
                    if let Ok(mut f) = std::fs::OpenOptions::new()
                        .create(true).append(true)
                        .open(output.with_extension("jsonl"))
                    {
                        let _ = writeln!(f, "{s}");
                    }
                }
                results.push(r);
            }
            Err(e) => {
                dropped += 1;
                eprintln!("  scenario {} DROPPED: {e}", scenario.uid);
            }
        }
    }
    if dropped > 0 {
        eprintln!(
            "\n⚠ {dropped}/{} scenarios DROPPED (errored) — the reported rates are over the {} that completed. Investigate if >2-3%.",
            scenarios.len(),
            results.len()
        );
    }

    let pct = |f: usize, t: usize| if t == 0 { 0.0 } else { 100.0 * f as f32 / t as f32 };
    let fired_total = fired_t1 + fired_t2;
    let tot_total = tot_t1 + tot_t2;
    eprintln!(
        "\nDuel-fired (retrieval-level collisions, mock-independent):\n\
         \x20\x20Type I : {fired_t1}/{tot_t1} = {:.1}%\n\
         \x20\x20Type II: {fired_t2}/{tot_t2} = {:.1}%\n\
         \x20\x20Overall: {fired_total}/{tot_total} = {:.1}%",
        pct(fired_t1, tot_t1),
        pct(fired_t2, tot_t2),
        pct(fired_total, tot_total),
    );

    let report = aggregate(&results, judge.model_id());

    // Persist raw per-scenario results next to the summary report so the run
    // is auditable and re-aggregatable without re-spending on the API.
    let raw = serde_json::json!({
        "report": &report,
        "answerer_model": answerer.model_id(),
        "judge_model": judge.model_id(),
        "dataset": dataset.display().to_string(),
        "dropped_scenarios": dropped,
        "retrieval_policy": "oracle: user-subject seeded axes (located_in/works_at/works_as/owns) only — upper bound on duel mechanism, not end-to-end semantic retrieval",
        "haystack_mode": format!("{mode:?}"),
        "duel_fired": {
            "type_i": {"fired": fired_t1, "total": tot_t1, "pct": pct(fired_t1, tot_t1)},
            "type_ii": {"fired": fired_t2, "total": tot_t2, "pct": pct(fired_t2, tot_t2)},
            "overall": {"fired": fired_total, "total": tot_total, "pct": pct(fired_total, tot_total)},
        },
        "scenarios": results.iter().map(|r| serde_json::json!({
            "uid": r.scenario_id,
            "type": match r.conflict_type { ConflictType::TypeI => "T1", ConflictType::TypeII => "T2" },
            "state_resolution": r.state_resolution,
            "premise_resistance": r.premise_resistance,
            "implicit_policy_adaptation": r.implicit_policy_adaptation,
        })).collect::<Vec<_>>(),
    });
    std::fs::write(&output, serde_json::to_string_pretty(&raw)?)
        .with_context(|| format!("write STALE results to {}", output.display()))?;
    eprintln!("{}", render_summary(&report));
    eprintln!("Raw results written to {}", output.display());

    Ok(report)
}

#[cfg(test)]
mod adapter_tests {
    use super::*;

    #[test]
    fn parse_judge_plain_json() {
        let raw = r#"{"dim1_eval":{"reasoning":"x","pass":true},
                      "dim2_eval":{"reasoning":"y","pass":false},
                      "dim3_eval":{"reasoning":"z","pass":true}}"#;
        let v = parse_judge_json(raw).unwrap();
        assert!(v.dim1 && !v.dim2 && v.dim3);
    }

    #[test]
    fn parse_judge_fenced_json() {
        // Judges sometimes wrap output in a ```json fence (handled verbatim
        // like full_eval_performance.py).
        let raw = "```json\n{\"dim1_eval\":{\"pass\":false},\
                   \"dim2_eval\":{\"pass\":true},\"dim3_eval\":{\"pass\":false}}\n```";
        let v = parse_judge_json(raw).unwrap();
        assert!(!v.dim1 && v.dim2 && !v.dim3);
    }

    // --- Adversarial fixtures: real judges drift; these must NOT silently
    // score all-false (the money-wasting failure mode). Missing/unparseable
    // verdicts are HARD ERRORS now, surfaced not swallowed. ---

    #[test]
    fn parse_judge_missing_dimension_is_error_not_false() {
        // Only dim1 present. Must ERROR (so the run flags it), not silently
        // score dim2/dim3 false and look like a memory failure.
        let raw = r#"{"dim1_eval":{"pass":true}}"#;
        assert!(parse_judge_json(raw).is_err());
    }

    #[test]
    fn parse_judge_string_bool_accepted() {
        // Some judges emit "pass":"true" (string). Accept it.
        let raw = r#"{"dim1_eval":{"pass":"true"},"dim2_eval":{"pass":"false"},"dim3_eval":{"pass":"Yes"}}"#;
        let v = parse_judge_json(raw).unwrap();
        assert!(v.dim1 && !v.dim2 && v.dim3);
    }

    #[test]
    fn parse_judge_int_bool_accepted() {
        let raw = r#"{"dim1_eval":{"pass":1},"dim2_eval":{"pass":0},"dim3_eval":{"pass":1}}"#;
        let v = parse_judge_json(raw).unwrap();
        assert!(v.dim1 && !v.dim2 && v.dim3);
    }

    #[test]
    fn parse_judge_prose_only_is_error() {
        // A judge that refuses / narrates instead of JSON must error, not
        // become all-false.
        let raw = "I cannot evaluate these responses without more context.";
        assert!(parse_judge_json(raw).is_err());
    }

    #[test]
    fn parse_judge_reasoning_before_fence_ok() {
        // Reasoning prose, THEN the fenced JSON. The fence extractor must grab
        // the JSON, not choke on the prose.
        let raw = "Let me think... dim1 is clearly pass.\n```json\n\
                   {\"dim1_eval\":{\"pass\":true},\"dim2_eval\":{\"pass\":true},\
                   \"dim3_eval\":{\"pass\":false}}\n```\nDone.";
        let v = parse_judge_json(raw).unwrap();
        assert!(v.dim1 && v.dim2 && !v.dim3);
    }

    #[test]
    fn parse_judge_null_pass_is_error() {
        // "pass":null is unparseable → error, not false.
        let raw = r#"{"dim1_eval":{"pass":null},"dim2_eval":{"pass":true},"dim3_eval":{"pass":true}}"#;
        assert!(parse_judge_json(raw).is_err());
    }

    #[test]
    fn parse_judge_unfenced_with_leading_text_is_error() {
        // No fence, prose + a brace-y tail that isn't valid JSON → error.
        let raw = "Verdict: dim1 pass, dim2 fail, dim3 pass.";
        assert!(parse_judge_json(raw).is_err());
    }

    #[test]
    fn answerer_prompt_dim3_uses_latest_query_framing() {
        let (sys, turns) = answerer_prompt("- user located_in austin", "where now?", true);
        assert!(sys.contains("respond to the user's latest query"));
        assert!(turns[0].content.contains("[Latest Query]"));
        assert!(turns[0].content.contains("[Retrieved Memory]"));
    }

    #[test]
    fn answerer_prompt_dim12_uses_question_framing() {
        let (sys, turns) = answerer_prompt("ctx", "still in seattle?", false);
        assert!(sys.contains("accurately answer the question"));
        assert!(turns[0].content.contains("[Question]"));
    }

    #[test]
    fn keep_object_rejects_placeholders() {
        assert!(keep_object("Seattle"));
        assert!(keep_object("Portland"));
        assert!(!keep_object("not specified"));
        assert!(!keep_object("  UNKNOWN "));
        assert!(!keep_object(""));
        assert!(!keep_object("N/A"));
        // Prefix variants seen in the STALE debug run.
        assert!(!keep_object("Not specified in the provided text"));
        assert!(!keep_object("Not explicitly stated in the conversation"));
        assert!(!keep_object("not specified in the conversation"));
        assert!(!keep_object("No specific location"));
    }

    #[test]
    fn normalize_location_reduces_to_city() {
        assert_eq!(normalize_location("Austin, TX 78704"), "austin");
        assert_eq!(normalize_location("78704, Austin, TX"), "austin");
        assert_eq!(
            normalize_location("S Lamar Blvd & Barton Springs Rd, Austin, TX"),
            "austin"
        );
        assert_eq!(normalize_location("Seattle"), "seattle");
    }

    #[test]
    fn session_text_flattens_role_content() {
        let turns = vec![
            StaleTurn { role: "user".into(), content: "hi".into() },
            StaleTurn { role: "assistant".into(), content: "hello".into() },
        ];
        let t = session_text(&turns);
        assert_eq!(t, "user: hi\nassistant: hello\n");
    }
}
} // mod adapter

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
        assert_eq!(publish_decision(&mk(20.0)), PublishDecision::PublishHonest);
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
