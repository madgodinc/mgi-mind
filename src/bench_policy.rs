//! Counterfactual A/B benchmark for the retrieval policy (phase Д6 / Active
//! Retrieval).
//!
//! The product hypothesis (see `AI_INSTRUCTIONS.md` / roadmap "Active
//! retrieval"): an agent that **searches before answering** about the past
//! beats an agent that answers from priors. We can't run a real LLM
//! comparison cheaply, so we settle for an objective lower bound: classify
//! each question with the same trigger table the AI is supposed to use,
//! then ask "what fraction of questions a no-policy agent would miss".
//!
//! The metric is recall-via-policy, not LLM accuracy. It quantifies
//! **structural enforcement value**, not generation quality: how much R@k
//! the policy "unlocks" by making the agent search at all.
//!
//! Inputs:
//!   * a raw.json produced by `mgimind bench` over LongMemEval (per-question
//!     `question_type`, `gold`, `retrieved`, `hit_at`).
//!
//! Output:
//!   * counts per priority bucket (P1 must-search, P2 should-search,
//!     P0 don't-care);
//!   * R@k for each bucket if the agent searches (our actual retrieval);
//!   * R@k for each bucket if the agent doesn't search at all (0% by
//!     definition, included for the A/B framing);
//!   * Δ = with-policy − without-policy per question type.
//!
//! Question-type → priority mapping (LongMemEval-S → trigger table):
//!   single-session-user        → P1  (named entity / "I told you about X")
//!   single-session-preference  → P1  (user's stored preference)
//!   single-session-assistant   → P1  (recall of agent's own past answer)
//!   knowledge-update           → P1  (fact that has been corrected — verify)
//!   multi-session              → P1  (by definition spans prior sessions)
//!   temporal-reasoning         → P2  (date/time reasoning; needs context)
//!
//! All current LongMemEval-S question types map to P1 or P2 — i.e. there is
//! **no** type the policy says "skip search". This is honest: the roadmap
//! deliberately removed the "Priority 0" tier (false negatives cost more
//! than false positives). The implication is the policy unlocks 100% of the
//! recall vs a no-search baseline on this corpus. A future dataset with
//! actual chit-chat questions ("hi", "thanks") would split P0 cleanly.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Deserialize)]
struct InputRow {
    question_id: String,
    question_type: String,
    #[serde(default)]
    gold: Vec<String>,
    #[serde(default)]
    retrieved: Vec<String>,
    #[serde(default)]
    hit_at: BTreeMap<String, bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
enum Priority {
    P1, // must-search
    P2, // should-search
    P0, // don't-search
}

fn classify(question_type: &str) -> Priority {
    match question_type {
        "single-session-user"
        | "single-session-preference"
        | "single-session-assistant"
        | "knowledge-update"
        | "multi-session" => Priority::P1,
        "temporal-reasoning" => Priority::P2,
        _ => Priority::P0,
    }
}

#[derive(Debug, Default, Serialize, Clone)]
struct Bucket {
    n: usize,
    hit_at_1: usize,
    hit_at_5: usize,
    hit_at_10: usize,
}

impl Bucket {
    fn record(&mut self, row: &InputRow) {
        self.n += 1;
        if *row.hit_at.get("R@1").unwrap_or(&false) {
            self.hit_at_1 += 1;
        }
        if *row.hit_at.get("R@5").unwrap_or(&false) {
            self.hit_at_5 += 1;
        }
        if *row.hit_at.get("R@10").unwrap_or(&false) {
            self.hit_at_10 += 1;
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

#[derive(Debug, Serialize)]
struct Report {
    total_questions: usize,
    priority_counts: BTreeMap<String, usize>,
    with_policy: PriorityBuckets,
    without_policy: PriorityBuckets,
    delta_r_at_5_pct: BTreeMap<String, f32>,
    notes: Vec<String>,
}

#[derive(Debug, Serialize)]
struct PriorityBuckets {
    p1: Bucket,
    p2: Bucket,
    p0: Bucket,
    overall: Bucket,
}

pub fn run(input_path: &Path) -> Result<String> {
    let raw = std::fs::read_to_string(input_path).with_context(|| {
        format!(
            "Failed to read bench raw output at {}",
            input_path.display()
        )
    })?;
    let items: Vec<InputRow> = serde_json::from_str(&raw)
        .context("Failed to parse bench raw output (expected an array of objects)")?;

    let mut with_policy = PriorityBuckets {
        p1: Bucket::default(),
        p2: Bucket::default(),
        p0: Bucket::default(),
        overall: Bucket::default(),
    };
    // The no-policy agent never searches. For the same hit_at counts to
    // populate, we need a different model: any P1/P2 question is a miss for
    // an agent that didn't run the retrieval at all. P0 we leave at 0 as
    // well (chit-chat, no recall expected). So the without_policy bucket
    // structurally stays at 0 hits — the value of this benchmark is the
    // gap.
    let mut without_policy = PriorityBuckets {
        p1: Bucket::default(),
        p2: Bucket::default(),
        p0: Bucket::default(),
        overall: Bucket::default(),
    };
    let mut by_type: BTreeMap<String, (Bucket, Bucket, Priority)> = BTreeMap::new();
    let mut priority_counts: BTreeMap<String, usize> = BTreeMap::new();

    for row in &items {
        let prio = classify(&row.question_type);
        let key = format!("{:?}", prio);
        *priority_counts.entry(key).or_insert(0) += 1;

        // With-policy: the agent searches and we credit the actual hit_at.
        match prio {
            Priority::P1 => with_policy.p1.record(row),
            Priority::P2 => with_policy.p2.record(row),
            Priority::P0 => with_policy.p0.record(row),
        }
        with_policy.overall.record(row);

        // Without-policy: no search → no hits. We still need n counted so
        // recall denominators match.
        let mut empty_row = InputRow {
            question_id: row.question_id.clone(),
            question_type: row.question_type.clone(),
            gold: row.gold.clone(),
            retrieved: row.retrieved.clone(),
            hit_at: BTreeMap::from([
                ("R@1".to_string(), false),
                ("R@5".to_string(), false),
                ("R@10".to_string(), false),
            ]),
        };
        match prio {
            Priority::P1 => without_policy.p1.record(&empty_row),
            Priority::P2 => without_policy.p2.record(&empty_row),
            Priority::P0 => without_policy.p0.record(&empty_row),
        }
        without_policy.overall.record(&empty_row);

        let entry = by_type.entry(row.question_type.clone()).or_insert((
            Bucket::default(),
            Bucket::default(),
            prio,
        ));
        entry.0.record(row);
        // The actual row contributes to with-policy; without-policy stays
        // empty.
        empty_row.hit_at.clear();
        entry.1.record(&empty_row);
    }

    let mut delta: BTreeMap<String, f32> = BTreeMap::new();
    let mut notes: Vec<String> = Vec::new();
    for (t, (w, wo, prio)) in &by_type {
        let d = (w.recall(5) - wo.recall(5)) * 100.0;
        delta.insert(format!("{} ({:?})", t, prio), d);
    }
    let overall_delta_at_5 =
        (with_policy.overall.recall(5) - without_policy.overall.recall(5)) * 100.0;
    notes.push(format!(
        "Overall ΔR@5 = +{overall_delta_at_5:.1} pct — this is the recall a no-search baseline would not have."
    ));
    if priority_counts.get("P0").copied().unwrap_or(0) == 0 {
        notes.push(
            "All questions in this dataset map to P1 or P2 (no chit-chat / no-search bucket). The policy unlocks 100% of recall here. A future dataset with explicit P0 chit-chat (\"hi\", \"thanks\") would split the gap cleanly.".to_string(),
        );
    }
    notes.push(
        "Caveat: this benchmark proves structural value of the trigger policy, not LLM accuracy. A real A/B with a generation step needs a like-for-like LLM-judged harness (see BENCHMARKS.md \"Like-for-like vs other systems\").".to_string(),
    );

    let report = Report {
        total_questions: items.len(),
        priority_counts,
        with_policy,
        without_policy,
        delta_r_at_5_pct: delta,
        notes,
    };

    // Renderable summary plus the raw JSON for downstream consumers.
    let json = serde_json::to_string_pretty(&report)?;
    let mut s = String::new();
    use std::fmt::Write;
    let _ = writeln!(
        s,
        "Counterfactual A/B — retrieval policy on / off (phase Д6, zero-API)"
    );
    let _ = writeln!(s, "total questions: {}", report.total_questions);
    for (k, v) in &report.priority_counts {
        let _ = writeln!(s, "  {k}: {v}");
    }
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "WITH policy (agent searches before answering):  R@5 = {:.1}%",
        report.with_policy.overall.recall(5) * 100.0
    );
    let _ = writeln!(
        s,
        "  P1 (must-search):   n={} R@5={:.1}%",
        report.with_policy.p1.n,
        report.with_policy.p1.recall(5) * 100.0
    );
    let _ = writeln!(
        s,
        "  P2 (should-search): n={} R@5={:.1}%",
        report.with_policy.p2.n,
        report.with_policy.p2.recall(5) * 100.0
    );
    let _ = writeln!(
        s,
        "  P0 (no-search):     n={} R@5={:.1}%",
        report.with_policy.p0.n,
        report.with_policy.p0.recall(5) * 100.0
    );
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "WITHOUT policy (agent never searches):           R@5 = {:.1}% (structural)",
        report.without_policy.overall.recall(5) * 100.0
    );
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "ΔR@5 = +{:.1} pct  ← recall unlocked by the policy",
        (report.with_policy.overall.recall(5) - report.without_policy.overall.recall(5)) * 100.0
    );
    let _ = writeln!(s, "\n--- raw json ---\n{}", json);
    for n in &report.notes {
        let _ = writeln!(s, "\nnote: {n}");
    }
    Ok(s)
}
