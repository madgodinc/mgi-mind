//! Procedural-memory benchmark (phase Д6).
//!
//! Measures recall@k for the procedural memory layer: given a dataset of
//! (failing error, fix) pairs, can the store surface the correct playbook
//! when shown the error signature again?
//!
//! Dataset format: JSONL, one record per line:
//! ```jsonl
//! {"error":"...", "fix":"...", "language":"rust|ts|py|...", "stratum":"compile|test|runtime", "id":"optional-stable-id"}
//! ```
//!
//! Protocol (zero-API, no LLM):
//! 1. For each pair, `mind_learn(error, fix, verified=false)` into an isolated
//!    bench library so this run doesn't pollute production storage.
//! 2. For each pair, `mind_recall(error)` and check whether the gold fix
//!    appears in the top-k results.
//! 3. Report overall R@1/R@5/R@10 and a per-stratum breakdown (per-language,
//!    per-error-type), since one number hides where the recall lives.
//!
//! The "learn-then-recall" protocol is intentionally simple. We're not yet
//! splitting train/test or measuring generalization across paraphrases — that
//! is harder and would require labeled paraphrase pairs. This first pass
//! measures whether the index can find what it just stored under the same
//! signature, which is the minimum bar.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use crate::config::MindConfig;
use crate::{procedure, storage};

/// One row of the input dataset.
#[derive(Debug, Clone, Deserialize)]
struct DatasetItem {
    error: String,
    fix: String,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    stratum: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    context: Option<String>,
}

/// Recall accumulator over a set of items.
#[derive(Debug, Default, Serialize, Clone)]
struct Tally {
    n: usize,
    hit_at_1: usize,
    hit_at_5: usize,
    hit_at_10: usize,
}

impl Tally {
    fn record(&mut self, gold_position: Option<usize>) {
        self.n += 1;
        if let Some(pos) = gold_position {
            if pos < 1 {
                self.hit_at_1 += 1;
            }
            if pos < 5 {
                self.hit_at_5 += 1;
            }
            if pos < 10 {
                self.hit_at_10 += 1;
            }
        }
    }

    fn recall(&self, k: usize) -> f32 {
        if self.n == 0 {
            return 0.0;
        }
        let hits = match k {
            1 => self.hit_at_1,
            5 => self.hit_at_5,
            10 => self.hit_at_10,
            _ => 0,
        };
        hits as f32 / self.n as f32
    }
}

/// Per-item result, written to the raw-output file when requested.
#[derive(Debug, Serialize)]
struct ItemResult {
    id: Option<String>,
    language: Option<String>,
    stratum: Option<String>,
    error: String,
    gold_fix: String,
    gold_position: Option<usize>,
    top: Vec<TopResult>,
}

#[derive(Debug, Serialize)]
struct TopResult {
    rank: usize,
    fix: String,
    score: f32,
    verified: bool,
}

const BENCH_LIB: &str = "bench-procedural-tmp";
const KS: &[usize] = &[1, 5, 10];

/// Load a JSONL dataset from disk.
fn load_jsonl(path: &Path) -> Result<Vec<DatasetItem>> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read procedural dataset at {}", path.display()))?;
    let mut items = Vec::new();
    for (i, line) in raw.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let item: DatasetItem = serde_json::from_str(line)
            .with_context(|| format!("Failed to parse line {} of dataset", i + 1))?;
        items.push(item);
    }
    Ok(items)
}

/// Run the procedural benchmark.
pub async fn run(
    config: &MindConfig,
    path: &str,
    limit: Option<usize>,
    output: Option<&str>,
) -> Result<String> {
    let path = Path::new(path);
    let mut items = load_jsonl(path)?;
    if let Some(n) = limit {
        items.truncate(n);
    }
    if items.is_empty() {
        anyhow::bail!("Procedural dataset is empty");
    }

    // Isolated, throwaway library. Drop first in case a prior aborted run
    // left it half-populated.
    let _ = storage::drop_library(config, BENCH_LIB).await;
    storage::create_library(config, BENCH_LIB).await?;

    // Phase 1: learn every (error, fix) into the temp library.
    let total = items.len();
    for (i, item) in items.iter().enumerate() {
        let _ = procedure::learn(
            config,
            &item.error,
            &item.fix,
            item.context.as_deref().unwrap_or(""),
            Some(BENCH_LIB),
            false,
        )
        .await;
        if (i + 1) % 50 == 0 || i + 1 == total {
            eprintln!("  bench-procedural: learned {}/{}", i + 1, total);
        }
    }

    // Phase 2: for each item, recall by error signature and find the gold
    // position. We match by exact-fix-string because the dataset's `fix`
    // field is the authoritative answer — the embedded text we stored
    // includes the same string.
    let mut overall = Tally::default();
    let mut by_language: BTreeMap<String, Tally> = BTreeMap::new();
    let mut by_stratum: BTreeMap<String, Tally> = BTreeMap::new();
    let mut by_lang_stratum: BTreeMap<String, Tally> = BTreeMap::new();
    let mut item_results: Vec<ItemResult> = Vec::new();

    for (i, item) in items.iter().enumerate() {
        let hits = storage::recall_procedures(
            config,
            Some(&procedure::normalize_error(&item.error)),
            item.context.as_deref(),
            10,
        )
        .await
        .unwrap_or_default();

        // Find the gold position by exact fix-string match. Multiple stored
        // fixes can share the same error signature in noisy datasets, so
        // we pick the first index whose `fix` matches our gold; if none,
        // the gold is not in the top-10 (None).
        let gold_position = hits.iter().position(|h| h.fix.trim() == item.fix.trim());
        overall.record(gold_position);

        if let Some(lang) = &item.language {
            by_language.entry(lang.clone()).or_default().record(gold_position);
        }
        if let Some(s) = &item.stratum {
            by_stratum.entry(s.clone()).or_default().record(gold_position);
        }
        if let (Some(lang), Some(s)) = (&item.language, &item.stratum) {
            let key = format!("{lang}/{s}");
            by_lang_stratum.entry(key).or_default().record(gold_position);
        }

        if output.is_some() {
            item_results.push(ItemResult {
                id: item.id.clone(),
                language: item.language.clone(),
                stratum: item.stratum.clone(),
                error: item.error.clone(),
                gold_fix: item.fix.clone(),
                gold_position,
                top: hits
                    .iter()
                    .take(10)
                    .enumerate()
                    .map(|(rank, h)| TopResult {
                        rank,
                        fix: h.fix.clone(),
                        score: h.score,
                        verified: h.verified,
                    })
                    .collect(),
            });
        }

        if (i + 1) % 50 == 0 || i + 1 == total {
            eprintln!("  bench-procedural: recall {}/{}", i + 1, total);
        }
    }

    // Cleanup the bench library so consecutive runs are isolated.
    let _ = storage::drop_library(config, BENCH_LIB).await;

    if let Some(out) = output {
        let json = serde_json::to_string_pretty(&item_results)?;
        crate::util::atomic_write_str(Path::new(out), &json)?;
    }

    Ok(render(
        config,
        &overall,
        &by_language,
        &by_stratum,
        &by_lang_stratum,
        output,
    ))
}

fn render(
    config: &MindConfig,
    overall: &Tally,
    by_language: &BTreeMap<String, Tally>,
    by_stratum: &BTreeMap<String, Tally>,
    by_lang_stratum: &BTreeMap<String, Tally>,
    output: Option<&str>,
) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    let _ = writeln!(s, "Procedural memory — recall@k (phase Д6, zero-API)");
    let _ = writeln!(
        s,
        "config: model={} dim={} rerank={}",
        config.model_name, config.vector_size, config.rerank_enabled
    );
    let _ = writeln!(s, "scored: {} pairs", overall.n);
    let _ = writeln!(s);
    let _ = writeln!(s, "Overall:");
    for k in KS {
        let _ = writeln!(s, "  R@{:<2} = {:.1}%", k, overall.recall(*k) * 100.0);
    }
    if !by_language.is_empty() {
        let _ = writeln!(s, "\nBy language:");
        let pad = by_language.keys().map(String::len).max().unwrap_or(0);
        for (k, v) in by_language {
            let _ = writeln!(
                s,
                "  {k:pad$}  n={:<4} R@1={:>5.1}% R@5={:>5.1}% R@10={:>5.1}%",
                v.n,
                v.recall(1) * 100.0,
                v.recall(5) * 100.0,
                v.recall(10) * 100.0
            );
        }
    }
    if !by_stratum.is_empty() {
        let _ = writeln!(s, "\nBy stratum (error type):");
        let pad = by_stratum.keys().map(String::len).max().unwrap_or(0);
        for (k, v) in by_stratum {
            let _ = writeln!(
                s,
                "  {k:pad$}  n={:<4} R@1={:>5.1}% R@5={:>5.1}% R@10={:>5.1}%",
                v.n,
                v.recall(1) * 100.0,
                v.recall(5) * 100.0,
                v.recall(10) * 100.0
            );
        }
    }
    if !by_lang_stratum.is_empty() {
        let _ = writeln!(s, "\nBy language × stratum:");
        let pad = by_lang_stratum.keys().map(String::len).max().unwrap_or(0);
        for (k, v) in by_lang_stratum {
            let _ = writeln!(
                s,
                "  {k:pad$}  n={:<4} R@1={:>5.1}% R@5={:>5.1}% R@10={:>5.1}%",
                v.n,
                v.recall(1) * 100.0,
                v.recall(5) * 100.0,
                v.recall(10) * 100.0
            );
        }
    }
    if let Some(out) = output {
        let _ = writeln!(s, "\nRaw per-pair results written to {out}");
    }
    s
}

// HashMap is currently unused at this scope but kept for forward compat: if we
// add a contingency-style breakdown (e.g. per error code), we'll want O(1)
// lookup rather than sorted iteration.
#[allow(dead_code)]
fn _hashmap_keepalive() -> HashMap<String, ()> {
    HashMap::new()
}
