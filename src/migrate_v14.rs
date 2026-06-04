//! v1.4 Phase 1 migration scripts: prepare the existing base for the
//! validity / relevance model.
//!
//! Three operations, each idempotent and read-only by default:
//!
//! - **`dependants`** — for every fact, count how many memories
//!   semantically depend on it (cosine ≥ threshold against the fact's
//!   subject+predicate+object vector). Prints a distribution histogram;
//!   `--apply` writes a `dependants_count` payload field per fact.
//!   The histogram is the gate to Phase 2: it dictates the shape of the
//!   entrenchment formula (linear vs logarithmic) based on whether the
//!   real-world distribution is fat-tailed or uniform.
//!
//! - **`cardinality`** — inspect every distinct predicate and propose a
//!   cardinality based on observed usage. Writes proposals to a local
//!   JSON file for the user to review. Auto-applying would be exactly
//!   the "recalibrate someone else's beliefs without consent" failure
//!   mode the synthesis warns about; we surface for explicit acceptance.
//!
//! - **`confirmations`** — backfill `confirmations_count` for memories
//!   that have a derivable signal (procedure_outcome links,
//!   multi-source provenance). Memories without such signal stay at 0.
//!
//! Privacy: all three operations process the author's real memory base.
//! Output histograms and proposal files **may contain references to real
//! content**. They land in `$MGIMIND_HOME/migration/`, never in the
//! public repo. Phase 4 will publish anonymous percentile summaries
//! when reporting calibration choices.

use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;

use crate::config::MindConfig;
use crate::knowledge::{Cardinality, Fact};

// ===== Pure helpers (no async, no Qdrant) — testable in isolation =====

/// Histogram bucket summary: (min, p10, p50, p90, max, mean, count).
///
/// Computed in one pass over a sorted slice. Used by all three migration
/// commands to print the distribution of whatever they backfilled.
#[derive(Debug, Clone, PartialEq)]
pub struct DistributionSummary {
    pub min: u32,
    pub p10: u32,
    pub p50: u32,
    pub p90: u32,
    pub max: u32,
    pub mean: f32,
    pub count: usize,
}

impl DistributionSummary {
    pub fn from_counts(counts: &[u32]) -> Self {
        if counts.is_empty() {
            return Self {
                min: 0,
                p10: 0,
                p50: 0,
                p90: 0,
                max: 0,
                mean: 0.0,
                count: 0,
            };
        }
        let mut sorted = counts.to_vec();
        sorted.sort_unstable();
        let n = sorted.len();
        let percentile = |p: f32| -> u32 {
            let idx = ((p / 100.0) * (n as f32 - 1.0)).round() as usize;
            sorted[idx.min(n - 1)]
        };
        let sum: u64 = sorted.iter().map(|&v| v as u64).sum();
        Self {
            min: sorted[0],
            p10: percentile(10.0),
            p50: percentile(50.0),
            p90: percentile(90.0),
            max: sorted[n - 1],
            mean: sum as f32 / n as f32,
            count: n,
        }
    }

    /// Render the summary as a 3-line human-readable block. The output is
    /// safe for public display — it contains only counts and percentiles,
    /// no memory content.
    pub fn render(&self, label: &str) -> String {
        format!(
            "{label}: n={} min={} p10={} p50={} p90={} max={} mean={:.1}",
            self.count, self.min, self.p10, self.p50, self.p90, self.max, self.mean,
        )
    }

    /// Suggest the entrenchment formula shape based on the distribution.
    /// Fat-tailed (p90 >> p50) → logarithmic. Uniform → linear. This is
    /// the Phase 1 → Phase 2 gate: the formula shape is dictated by data,
    /// not chosen a priori.
    pub fn recommended_formula_shape(&self) -> &'static str {
        if self.count == 0 || self.p50 == 0 {
            "no signal yet — defer formula choice until more data"
        } else if (self.p90 as f32) / (self.p50.max(1) as f32) > 5.0 {
            "fat-tailed distribution → use logarithmic entrenchment (log2(1 + dependants))"
        } else if self.p90 == self.p50 {
            "constant distribution → entrenchment dominated by other signals; consider flat weighting"
        } else {
            "moderate spread → linear entrenchment is reasonable starting point"
        }
    }
}

/// Cardinality inference: given the set of distinct objects observed for a
/// `(subject, predicate)` group, propose a cardinality.
///
/// Heuristic (synthesis §4):
/// - Every subject has ≤ 1 distinct object → propose `Single`.
/// - At least 20% of subjects have ≥ 2 distinct objects → propose `Multi`.
/// - Between → propose `Multi` with a "review" hint (safe default).
///
/// This is a *proposal* — the user reviews via the JSON file before
/// committing to the cardinality registry. The function is pure so it
/// can be unit-tested without a Qdrant connection.
///
/// Phase 1.2 (`run_cardinality_inference`) wires this into the actual
/// walk; allowed-dead until then so the helper lands in a separate
/// bisectable commit alongside the walk.
#[allow(dead_code)]
pub fn propose_cardinality(
    objects_per_subject: &[Vec<String>],
) -> CardinalityProposal {
    if objects_per_subject.is_empty() {
        return CardinalityProposal {
            proposed: Cardinality::Multi,
            confidence: ProposalConfidence::Low,
            reason: "no observations".to_string(),
        };
    }
    let n_subjects = objects_per_subject.len();
    let multi_subjects = objects_per_subject
        .iter()
        .filter(|objs| objs.len() >= 2)
        .count();
    let multi_ratio = multi_subjects as f32 / n_subjects as f32;
    if multi_subjects == 0 {
        CardinalityProposal {
            proposed: Cardinality::Single,
            confidence: ProposalConfidence::High,
            reason: format!(
                "every subject has ≤ 1 distinct object across {n_subjects} subjects"
            ),
        }
    } else if multi_ratio >= 0.20 {
        CardinalityProposal {
            proposed: Cardinality::Multi,
            confidence: ProposalConfidence::High,
            reason: format!(
                "{multi_subjects}/{n_subjects} subjects ({:.0}%) have ≥ 2 distinct objects",
                multi_ratio * 100.0
            ),
        }
    } else {
        CardinalityProposal {
            proposed: Cardinality::Multi,
            confidence: ProposalConfidence::Low,
            reason: format!(
                "{multi_subjects}/{n_subjects} subjects ({:.0}%) have ≥ 2 distinct objects — below 20% threshold; defaulting to Multi for review",
                multi_ratio * 100.0
            ),
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct CardinalityProposal {
    #[serde(serialize_with = "serialize_cardinality")]
    pub proposed: Cardinality,
    pub confidence: ProposalConfidence,
    pub reason: String,
}

#[allow(dead_code)]
fn serialize_cardinality<S>(c: &Cardinality, ser: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    let s = match c {
        Cardinality::Single => "single",
        Cardinality::TemporalSingle => "temporal-single",
        Cardinality::Multi => "multi",
    };
    ser.serialize_str(s)
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub enum ProposalConfidence {
    High,
    Low,
}

// ===== Qdrant-talking functions (async; not unit-tested without a live db) =====

/// Walk all facts and count dependants per fact.
///
/// **Dependant definition** (Phase 1 operational): memory `M` depends on
/// fact `F` if cosine(embed(M.content), embed(F.subject + " " + F.predicate
/// + " " + F.object)) ≥ `threshold`. This is the conservative line in
/// MiniLM space at our chunking; 0.7 is the default. Phase 4 may revise
/// the threshold based on calibration.
///
/// Returns a map `fact_id → dependants_count` and the distribution summary
/// printed at the end of the run.
///
/// **Cost.** For each fact: one embedding inference + one vector search
/// over the memories collection. On a ~12k-memory base with ~50 facts:
/// 50 inferences (warm, ~30ms each) + 50 HNSW searches (~10ms each) =
/// ~2s wall-time on CPU MiniLM INT8. Negligible.
///
/// **Idempotency.** Run as many times as needed. With `apply=false` it is
/// read-only. With `apply=true` it overwrites the `dependants_count`
/// payload field; previous values are replaced cleanly.
pub async fn run_dependants(
    config: &MindConfig,
    threshold: f32,
    apply: bool,
) -> Result<(HashMap<String, u32>, DistributionSummary)> {
    // Step 1: enumerate every active fact in the knowledge graph.
    let facts = crate::knowledge::list_all_facts(config).await?;
    if facts.is_empty() {
        eprintln!("  no facts in the knowledge graph yet — nothing to count.");
        return Ok((HashMap::new(), DistributionSummary::from_counts(&[])));
    }
    eprintln!("  scanning {} facts...", facts.len());

    // Step 2: for each fact, build the canonical text and ask the existing
    // hybrid search (dense + sparse + optional reranker) for a wide candidate
    // set. We then count how many of those candidates clear the threshold.
    //
    // Cost: one embedding inference per fact + one HNSW query per fact. On a
    // ~12k-memory base with ~50 facts that is ~50 * (30ms embed + 10ms HNSW)
    // = ~2 seconds on CPU MiniLM INT8.
    //
    // Hybrid score is dense + sparse, so the threshold is calibrated against
    // that combined scale (not pure cosine). The default 0.7 was chosen for
    // pure cosine; for hybrid it tends to be slightly higher. Phase 4 may
    // revise once we see the real distribution.
    const PROBE_LIMIT: usize = 256;
    let mut counts: HashMap<String, u32> = HashMap::with_capacity(facts.len());

    for f in &facts {
        let canonical = format!("{} {} {}", f.subject, f.predicate, f.object);
        let hits = crate::storage::search(config, &canonical, None, PROBE_LIMIT, 2).await?;
        let count = hits.iter().filter(|h| h.score >= threshold).count() as u32;
        counts.insert(f.id.clone(), count);
    }

    let count_vec: Vec<u32> = counts.values().copied().collect();
    let summary = DistributionSummary::from_counts(&count_vec);

    // Step 3 (optional): persist the counts back into each fact's payload.
    // Phase 2 reads `dependants_count` directly from the payload at duel
    // time, so this materialises the cache the Phase 2 hot path relies on.
    if apply {
        eprintln!("  writing dependants_count back to {} facts...", counts.len());
        let mut written = 0usize;
        for (id, n) in counts.iter() {
            crate::knowledge::set_fact_payload_field(
                config,
                id,
                "dependants_count",
                n.to_string(),
            )
            .await?;
            written += 1;
        }
        eprintln!("  wrote dependants_count to {written} facts.");
    }

    Ok((counts, summary))
}

/// Walk all distinct `(subject, predicate)` groupings in the knowledge
/// graph and propose a cardinality per predicate.
///
/// Output: a JSON file at the given path with one entry per predicate.
/// User reviews, edits if needed, and runs the proposals back through
/// `mind_predicate(action="register")` (or a future bulk-apply command).
pub async fn run_cardinality_inference(
    config: &MindConfig,
    output: PathBuf,
) -> Result<usize> {
    // Step 1: enumerate every valid fact, group into (predicate → subject →
    // [objects]).
    let facts = crate::knowledge::list_all_facts(config).await?;
    if facts.is_empty() {
        eprintln!("  no facts in the knowledge graph — nothing to propose.");
        return Ok(0);
    }
    eprintln!("  inspecting {} facts...", facts.len());
    let grouped = group_facts_by_predicate(&facts);
    eprintln!("  {} distinct predicates observed.", grouped.len());

    // Step 2: for every predicate, run the heuristic against the observed
    // objects-per-subject and produce a proposal.
    let mut proposals: HashMap<String, CardinalityProposal> = HashMap::new();
    for (pred, observations) in &grouped {
        proposals.insert(pred.clone(), propose_cardinality(observations));
    }

    // Step 3: serialise to JSON for user review. The reviewer either edits
    // the file in place or runs each proposal through
    // `mind_predicate(action="register")`. Auto-applying is deliberately not
    // offered — recalibrating predicate cardinality without consent is the
    // exact failure mode the synthesis §10 question 6 names.
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).map_err(anyhow::Error::from)?;
    }
    let payload = serde_json::to_vec_pretty(&proposals).map_err(anyhow::Error::from)?;
    std::fs::write(&output, payload).map_err(anyhow::Error::from)?;
    eprintln!("  wrote {} proposals to {}", proposals.len(), output.display());

    // Step 4: print a one-line tally per confidence level so the user knows
    // what shape the review will be without opening the file.
    let high = proposals
        .values()
        .filter(|p| p.confidence == ProposalConfidence::High)
        .count();
    let low = proposals.len() - high;
    eprintln!(
        "  proposal summary: {} high-confidence, {} low-confidence (review the JSON)",
        high, low
    );

    Ok(proposals.len())
}

/// Backfill `confirmations_count` for memories with a derivable signal.
///
/// Two signal sources for v1.4 Phase 1:
/// - linked `mind_procedure_outcome(worked=true)` events
/// - provenance entries with two or more distinct origin URLs
///
/// Memories without either stay at `confirmations_count = 0`. Honest
/// over-fitting risk: backfilling synthetic confirmations from old data
/// would contaminate the Phase 2 calibration baseline. We leave them at
/// zero and let them accumulate going forward.
pub async fn run_confirmations(
    config: &MindConfig,
    apply: bool,
) -> Result<(usize, DistributionSummary)> {
    let _ = (config, apply); // implementation lands in step 1.3
    Ok((0, DistributionSummary::from_counts(&[])))
}

// Used in step 1.2 to render the cardinality proposal file. Kept here
// so the Phase 1 helpers live in one module.
#[allow(dead_code)]
pub(crate) fn group_facts_by_predicate(facts: &[Fact]) -> HashMap<String, Vec<Vec<String>>> {
    let mut by_predicate: HashMap<String, HashMap<String, Vec<String>>> = HashMap::new();
    for f in facts {
        let by_subject = by_predicate.entry(f.predicate.clone()).or_default();
        by_subject
            .entry(f.subject.clone())
            .or_default()
            .push(f.object.clone());
    }
    by_predicate
        .into_iter()
        .map(|(pred, by_subject)| {
            let objects_per_subject: Vec<Vec<String>> = by_subject.into_values().collect();
            (pred, objects_per_subject)
        })
        .collect()
}

// ===== Tests for the pure helpers =====
//
// These tests cover the formula-shape decision (DistributionSummary) and
// the cardinality-inference heuristic. They are the spec the Phase 1 CLI
// commands have to respect; if any of them fails or has to be edited,
// the migration is operating on the wrong axis.

#[cfg(test)]
mod tests {
    use super::*;

    // --- DistributionSummary ---

    #[test]
    fn empty_distribution_is_safe() {
        let s = DistributionSummary::from_counts(&[]);
        assert_eq!(s.count, 0);
        assert_eq!(s.min, 0);
        assert_eq!(s.max, 0);
        assert!(s.recommended_formula_shape().contains("no signal"));
    }

    #[test]
    fn single_value_distribution_yields_constant() {
        let s = DistributionSummary::from_counts(&[7, 7, 7, 7]);
        assert_eq!(s.min, 7);
        assert_eq!(s.p50, 7);
        assert_eq!(s.p90, 7);
        assert_eq!(s.max, 7);
        assert_eq!(s.mean, 7.0);
        assert!(s.recommended_formula_shape().contains("constant"));
    }

    #[test]
    fn uniform_distribution_recommends_linear() {
        // ten evenly-spaced values; p90/p50 ratio is ~2x, below the 5x
        // fat-tail threshold, so linear entrenchment is the recommendation.
        let s = DistributionSummary::from_counts(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        assert!((s.mean - 5.5).abs() < 1e-3);
        assert!(s.recommended_formula_shape().contains("linear"));
    }

    #[test]
    fn fat_tail_distribution_recommends_logarithmic() {
        // Aggressive power-law: 80% of facts have 1 dependant, 20% have 100.
        // p50=1 (in the first half), p90=100 (the tail dominates the top
        // decile), ratio = 100 — far above the 5x threshold so the
        // recommendation must be logarithmic.
        let mut data: Vec<u32> = vec![1; 80];
        data.extend(std::iter::repeat(100).take(20));
        let s = DistributionSummary::from_counts(&data);
        assert_eq!(s.max, 100);
        assert!(
            (s.p90 as f32) / (s.p50.max(1) as f32) > 5.0,
            "test data should be fat-tailed (p90={}, p50={})",
            s.p90,
            s.p50
        );
        let rec = s.recommended_formula_shape();
        assert!(
            rec.contains("logarithmic"),
            "fat tail should recommend log: got {rec}"
        );
    }

    #[test]
    fn percentile_math_handles_small_n() {
        // n=3 should still compute well-defined percentiles, not
        // overrun the slice. The percentile rounding uses
        // round((p/100) * (n-1)), which for p10 and n=3 gives idx=0.
        let s = DistributionSummary::from_counts(&[10, 20, 30]);
        assert_eq!(s.min, 10);
        assert_eq!(s.max, 30);
        assert_eq!(s.p50, 20);
    }

    #[test]
    fn render_format_is_grep_friendly() {
        // The output goes into terminal logs and possibly into a public
        // summary; the format must be machine-readable enough to grep
        // for the percentiles.
        let s = DistributionSummary::from_counts(&[1, 2, 3]);
        let rendered = s.render("dependants");
        assert!(rendered.contains("n=3"));
        assert!(rendered.contains("p50=2"));
        assert!(rendered.contains("max=3"));
    }

    // --- Cardinality inference ---

    #[test]
    fn no_observations_proposes_multi_with_low_confidence() {
        let p = propose_cardinality(&[]);
        assert_eq!(p.proposed, Cardinality::Multi);
        assert_eq!(p.confidence, ProposalConfidence::Low);
    }

    #[test]
    fn every_subject_single_value_proposes_single_with_high_confidence() {
        // 3 subjects, each with exactly one distinct object. Classic
        // `primary_language` shape: one current value per subject.
        let observations = vec![
            vec!["Rust".to_string()],
            vec!["Python".to_string()],
            vec!["Go".to_string()],
        ];
        let p = propose_cardinality(&observations);
        assert_eq!(p.proposed, Cardinality::Single);
        assert_eq!(p.confidence, ProposalConfidence::High);
    }

    #[test]
    fn most_subjects_multi_value_proposes_multi_with_high_confidence() {
        // 5 subjects, 4 of them have ≥ 2 distinct objects → 80% multi.
        // Classic `uses_language` shape: a person can use several.
        let observations = vec![
            vec!["Rust".into(), "Go".into()],
            vec!["Python".into(), "TypeScript".into()],
            vec!["Java".into(), "Kotlin".into()],
            vec!["Swift".into(), "Objective-C".into()],
            vec!["Single".into()],
        ];
        let p = propose_cardinality(&observations);
        assert_eq!(p.proposed, Cardinality::Multi);
        assert_eq!(p.confidence, ProposalConfidence::High);
    }

    #[test]
    fn borderline_under_20_percent_proposes_multi_low_confidence() {
        // 10 subjects, only 1 has multiple objects = 10% < 20% threshold.
        // The safe default is Multi (don't fire false duels) but the
        // confidence flag tells the reviewer to look closely.
        let mut observations = vec![vec!["x".to_string()]; 9];
        observations.push(vec!["y".into(), "z".into()]);
        let p = propose_cardinality(&observations);
        assert_eq!(p.proposed, Cardinality::Multi);
        assert_eq!(p.confidence, ProposalConfidence::Low);
        assert!(p.reason.contains("10%"));
    }

    #[test]
    fn group_facts_by_predicate_separates_subjects() {
        // Two predicates, four facts. Group must put the right
        // (subject → objects) lists under each predicate.
        let facts = vec![
            Fact {
                id: "1".into(),
                subject: "alice".into(),
                predicate: "primary_language".into(),
                object: "Rust".into(),
                created_at: None,
                valid: true,
            },
            Fact {
                id: "2".into(),
                subject: "bob".into(),
                predicate: "primary_language".into(),
                object: "Python".into(),
                created_at: None,
                valid: true,
            },
            Fact {
                id: "3".into(),
                subject: "alice".into(),
                predicate: "uses_language".into(),
                object: "Rust".into(),
                created_at: None,
                valid: true,
            },
            Fact {
                id: "4".into(),
                subject: "alice".into(),
                predicate: "uses_language".into(),
                object: "Go".into(),
                created_at: None,
                valid: true,
            },
        ];
        let grouped = group_facts_by_predicate(&facts);
        assert_eq!(grouped.len(), 2);
        let primary = grouped.get("primary_language").unwrap();
        let uses = grouped.get("uses_language").unwrap();
        assert_eq!(primary.len(), 2); // alice, bob — two subjects
        assert_eq!(uses.len(), 1); // alice only — one subject
        // alice's `uses_language` should have both Rust and Go.
        assert_eq!(uses[0].len(), 2);
    }
}
