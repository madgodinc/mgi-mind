//! Retrieval benchmark (phase Д1) — the honest, zero-API number.
//!
//! MGI-Mind has no generation layer ("you are the memory, not the assistant"), so
//! the native, on-brand metric is **retrieval recall R@k**: given a question, does
//! the gold evidence land in the top-k of the hybrid search? It is measured with
//! NO LLM and NO external API - directly comparable to other systems' retrieval
//! recall, and it does not lie about "runs locally, no keys".
//!
//! What this is NOT: QA accuracy (an LLM generates an answer, a judge-LLM scores
//! it). That needs paid API calls and measures "memory + someone else's LLM", not
//! the memory. Putting R@k next to another system's QA number is the apples-to-
//! oranges overclaim this project refuses to make. QA mode is a separate, clearly
//! labeled, opt-in flag (not in this zero-API core).
//!
//! Method (LongMemEval, session-level): for each question we ingest its own
//! haystack sessions into an isolated throwaway library (each session is one
//! memory tagged with its session id), run `mind_search`, collapse the ranked
//! results to distinct session ids, and check whether a gold `answer_session_id`
//! appears within the top k. Abstention questions (`_abs`, no evidence) are
//! excluded from the recall denominator and reported separately.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};

use crate::config::MindConfig;
use crate::storage;

/// Isolated library bench uses; cleared between questions so the live store never
/// accumulates bench points (at most one question's haystack exists at a time).
const BENCH_LIB: &str = "_bench";
/// How many ranked chunks to pull before collapsing to distinct sessions. Over-
/// fetch so we still get ~10 distinct sessions when a session spans many chunks.
const FETCH: usize = 40;
/// The k values reported.
const KS: [usize; 3] = [1, 5, 10];

#[derive(Debug, Deserialize)]
struct Turn {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct LongMemItem {
    question_id: String,
    #[serde(default)]
    question_type: String,
    question: String,
    #[serde(default)]
    haystack_session_ids: Vec<String>,
    #[serde(default)]
    haystack_sessions: Vec<Vec<Turn>>,
    #[serde(default)]
    answer_session_ids: Vec<String>,
}

impl LongMemItem {
    /// Abstention questions have no in-haystack evidence; they test "say you don't
    /// know", not retrieval, so they're outside the recall denominator.
    fn is_abstention(&self) -> bool {
        self.question_type.ends_with("_abs") || self.answer_session_ids.is_empty()
    }
}

/// Dedup a ranked list of session ids, preserving first-seen order. The retrieval
/// unit is the session, but the store is chunk-level, so several top chunks can
/// map to the same session; we collapse to the session ranking.
fn distinct_in_order(sources: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for s in sources {
        if seen.insert(s.clone()) {
            out.push(s.clone());
        }
    }
    out
}

/// True if any gold session appears within the first `k` distinct retrieved
/// sessions. Pure, so it is unit-tested without Qdrant.
fn recall_at_k(ranked_sessions: &[String], gold: &HashSet<String>, k: usize) -> bool {
    ranked_sessions.iter().take(k).any(|s| gold.contains(s))
}

#[derive(Default)]
struct Tally {
    total: usize,
    hits: HashMap<usize, usize>, // k -> hit count
}

impl Tally {
    fn record(&mut self, ranked: &[String], gold: &HashSet<String>) {
        self.total += 1;
        for k in KS {
            if recall_at_k(ranked, gold, k) {
                *self.hits.entry(k).or_insert(0) += 1;
            }
        }
    }
    fn recall(&self, k: usize) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        *self.hits.get(&k).unwrap_or(&0) as f64 / self.total as f64
    }
}

#[derive(Debug, serde::Serialize)]
struct ItemResult {
    question_id: String,
    question_type: String,
    gold: Vec<String>,
    retrieved: Vec<String>,
    hit_at: HashMap<String, bool>,
}

/// Run the LongMemEval retrieval benchmark. Zero-API: ingest -> search -> R@k.
pub async fn run_longmemeval(
    config: &MindConfig,
    path: &str,
    limit: Option<usize>,
    output: Option<&str>,
) -> Result<String> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read dataset at {path}"))?;
    let mut items: Vec<LongMemItem> = serde_json::from_str(&raw)
        .context("Failed to parse LongMemEval JSON (expected an array)")?;
    if let Some(n) = limit {
        items.truncate(n);
    }

    let mut overall = Tally::default();
    let mut by_type: HashMap<String, Tally> = HashMap::new();
    let mut abstentions = 0usize;
    let mut item_results: Vec<ItemResult> = Vec::new();

    let total = items.len();
    for (i, item) in items.iter().enumerate() {
        if item.is_abstention() {
            abstentions += 1;
            continue;
        }

        // Isolated, cleared library per question (no cross-haystack bleed, no live
        // store growth).
        let _ = storage::drop_library(config, BENCH_LIB).await;
        storage::create_library(config, BENCH_LIB).await?;

        // Build the full (text, session_id) list and ingest in ONE batched embed
        // pass. Previous per-session calls cost ~1.5-3s each on GPU (cuDNN warmup
        // per call dominated); batched embedding of all sessions for a question
        // runs as a single padded ONNX forward.
        let batch: Vec<(String, Option<String>)> = item
            .haystack_session_ids
            .iter()
            .zip(item.haystack_sessions.iter())
            .filter_map(|(sid, session)| {
                let text = session
                    .iter()
                    .map(|t| format!("{}: {}", t.role, t.content))
                    .collect::<Vec<_>>()
                    .join("\n");
                if text.trim().is_empty() {
                    None
                } else {
                    Some((text, Some(sid.clone())))
                }
            })
            .collect();
        let _ = storage::add_memories_batch(config, BENCH_LIB, &batch).await;

        let results = storage::search(config, &item.question, Some(BENCH_LIB), FETCH, 1).await?;
        let sources: Vec<String> = results.iter().filter_map(|r| r.source.clone()).collect();
        let ranked = distinct_in_order(&sources);
        let gold: HashSet<String> = item.answer_session_ids.iter().cloned().collect();

        overall.record(&ranked, &gold);
        by_type
            .entry(item.question_type.clone())
            .or_default()
            .record(&ranked, &gold);

        if output.is_some() {
            let hit_at = KS
                .iter()
                .map(|k| (format!("R@{k}"), recall_at_k(&ranked, &gold, *k)))
                .collect();
            item_results.push(ItemResult {
                question_id: item.question_id.clone(),
                question_type: item.question_type.clone(),
                gold: item.answer_session_ids.clone(),
                retrieved: ranked.iter().take(10).cloned().collect(),
                hit_at,
            });
        }

        if (i + 1) % 10 == 0 || i + 1 == total {
            eprintln!("  bench: {}/{} questions", i + 1, total);
        }
    }

    // Clean up the bench library.
    let _ = storage::drop_library(config, BENCH_LIB).await;

    if let Some(out) = output {
        let json = serde_json::to_string_pretty(&item_results)?;
        crate::util::atomic_write_str(std::path::Path::new(out), &json)?;
    }

    Ok(render_report(
        config,
        &overall,
        &by_type,
        abstentions,
        output,
    ))
}

fn render_report(
    config: &MindConfig,
    overall: &Tally,
    by_type: &HashMap<String, Tally>,
    abstentions: usize,
    output: Option<&str>,
) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    writeln!(s, "LongMemEval — retrieval recall (R@k), zero-API").unwrap();
    writeln!(
        s,
        "config: model={} dim={} rerank={} (sessions ranked by hybrid dense+sparse)",
        config.model_name, config.vector_size, config.rerank_enabled
    )
    .unwrap();
    writeln!(
        s,
        "scored: {} questions ({} abstention excluded)\n",
        overall.total, abstentions
    )
    .unwrap();

    writeln!(s, "Overall:").unwrap();
    for k in KS {
        writeln!(s, "  R@{:<2} = {:.1}%", k, overall.recall(k) * 100.0).unwrap();
    }

    if !by_type.is_empty() {
        writeln!(s, "\nBy question type:").unwrap();
        let mut types: Vec<&String> = by_type.keys().collect();
        types.sort();
        for t in types {
            let tally = &by_type[t];
            writeln!(
                s,
                "  {:<26} n={:<4} R@1={:.0}% R@5={:.0}% R@10={:.0}%",
                t,
                tally.total,
                tally.recall(1) * 100.0,
                tally.recall(5) * 100.0,
                tally.recall(10) * 100.0
            )
            .unwrap();
        }
    }
    if let Some(out) = output {
        writeln!(s, "\nRaw per-question results written to {out}").unwrap();
    }
    write!(
        s,
        "\nNote: R@k is retrieval recall (zero-API), NOT QA accuracy. Do not compare \
         it against another system's LLM-judged QA numbers."
    )
    .unwrap();
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinct_preserves_order() {
        let v = vec![
            "a".to_string(),
            "b".to_string(),
            "a".to_string(),
            "c".to_string(),
        ];
        assert_eq!(distinct_in_order(&v), vec!["a", "b", "c"]);
    }

    #[test]
    fn recall_respects_k() {
        let ranked: Vec<String> = ["x", "y", "gold", "z"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let gold: HashSet<String> = ["gold"].iter().map(|s| s.to_string()).collect();
        assert!(
            !recall_at_k(&ranked, &gold, 1),
            "gold is at rank 3, not in top-1"
        );
        assert!(!recall_at_k(&ranked, &gold, 2));
        assert!(recall_at_k(&ranked, &gold, 5), "gold is within top-5");
    }

    #[test]
    fn abstention_detected() {
        let abs = LongMemItem {
            question_id: "q".into(),
            question_type: "single-session-user_abs".into(),
            question: "?".into(),
            haystack_session_ids: vec![],
            haystack_sessions: vec![],
            answer_session_ids: vec![],
        };
        assert!(abs.is_abstention());
    }

    #[test]
    fn tally_computes_recall_fraction() {
        let mut t = Tally::default();
        let gold: HashSet<String> = ["g"].iter().map(|s| s.to_string()).collect();
        t.record(&["g".to_string()], &gold); // hit at all k
        t.record(&["x".to_string()], &gold); // miss at all k
        assert_eq!(t.total, 2);
        assert!((t.recall(1) - 0.5).abs() < 1e-9);
    }
}
