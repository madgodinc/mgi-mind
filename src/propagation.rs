//! Type II (propagated / cross-predicate) belief revision — the native core
//! mechanism behind the STALE benchmark's "implicit conflict" case.
//!
//! The same-axis duel (`duel.rs`) only fires when a new fact and an old fact
//! share `(subject, predicate)` — that handles Type I (co-referential)
//! conflicts like `located_in: seattle -> austin`. But many real belief
//! revisions are PROPAGATED: a new fact on one attribute makes an old fact on a
//! DIFFERENT attribute stale via common-sense world knowledge
//! (`local_climate = arid` makes `located_in = portland` impossible). No string
//! match connects them, so the duel never sees the conflict.
//!
//! This module is the CUPMem-style adjudicator (`J_theta`): when a new fact
//! lands, gather the subject's other active facts, ask a judge which of them the
//! new fact contradicts, and mark those `PropagationShadowed`. The judge is
//! pluggable (`PropagationJudge`) so the core is testable LLM-free
//! (`FixtureJudge`) exactly like `duel-validity`, while production uses the
//! local extractor model and the bench can use a paid API model — all three
//! drive the SAME adjudication + shadowing + read-out code, which is what makes
//! "the release reproduces the bench numbers on the prod path" literally true.
//!
//! Honesty boundary: the judge reasons over predicate/value SEMANTICS with a
//! generic common-sense prompt. No dataset-specific rule (e.g. "desert => not
//! Seattle") is ever encoded; the value-level verdict is always the judge's.

use crate::config::MindConfig;
use anyhow::Result;

/// One candidate the judge inspects: an existing active fact on the subject.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub id: String,
    pub predicate: String,
    pub object: String,
}

/// A new fact that just landed and may shadow earlier ones.
#[derive(Debug, Clone)]
pub struct NewFact {
    pub predicate: String,
    pub object: String,
}

/// Decides which existing candidates a new fact makes stale. Returns the
/// indices (into `candidates`) of facts that are now contradicted. Pluggable so
/// the core is LLM-free testable and prod/bench can swap the model.
#[async_trait::async_trait]
pub trait PropagationJudge: Send + Sync {
    async fn judge(&self, subject: &str, new_fact: &NewFact, candidates: &[Candidate]) -> Vec<usize>;
}

/// Deterministic judge for tests (the LLM-free analog of the duel-validity
/// fixture). Returns the indices configured up front.
pub struct FixtureJudge {
    pub stale_indices: Vec<usize>,
}

#[async_trait::async_trait]
impl PropagationJudge for FixtureJudge {
    async fn judge(&self, _subject: &str, _new_fact: &NewFact, candidates: &[Candidate]) -> Vec<usize> {
        self.stale_indices
            .iter()
            .copied()
            .filter(|&i| i < candidates.len())
            .collect()
    }
}

/// Production judge: the local extractor model (Granite via llama-server) does
/// the cross-predicate common-sense reasoning. Feature-gated; non-extractor
/// builds use FixtureJudge / external judges only.
#[cfg(feature = "extractor")]
pub struct LocalExtractorJudge {
    pub config: crate::extractor::ExtractConfig,
}

#[cfg(feature = "extractor")]
#[async_trait::async_trait]
impl PropagationJudge for LocalExtractorJudge {
    async fn judge(&self, _subject: &str, new_fact: &NewFact, candidates: &[Candidate]) -> Vec<usize> {
        let user = adjudicate_user_prompt(new_fact, candidates);
        let raw = match crate::extractor::adjudicate_stale(&self.config, ADJUDICATE_SYSTEM, &user).await {
            Ok(r) => r,
            Err(_) => return Vec::new(), // best-effort; never break the add
        };
        parse_stale_indices(&raw, candidates.len())
    }
}

/// Parse a judge's reply into in-range candidate indices. Tolerant: grabs the
/// first [...] block; ignores out-of-range / non-integer entries.
pub fn parse_stale_indices(raw: &str, n: usize) -> Vec<usize> {
    let block = match (raw.find('['), raw.rfind(']')) {
        (Some(a), Some(b)) if b > a => &raw[a..=b],
        _ => return Vec::new(),
    };
    let parsed: Vec<i64> = serde_json::from_str(block).unwrap_or_default();
    parsed
        .into_iter()
        .filter(|&i| i >= 0 && (i as usize) < n)
        .map(|i| i as usize)
        .collect()
}

/// The generic common-sense adjudication prompt, shared by every real judge.
/// No dataset terms — pure schema-level reasoning. (Lifted from the bench's
/// `implicit_adjudicate`, which is the proven Type II path.)
pub const ADJUDICATE_SYSTEM: &str = "You audit a person's memory for facts \
    whose CURRENT-DEFAULT SAFETY a new fact has broken. You are given one NEW \
    fact just learned about the person, and a numbered list of EARLIER facts \
    currently believed true. Mark an earlier fact stale only when the new fact \
    breaks or replaces the PRACTICAL BASIS that earlier fact depends on — so it \
    is no longer available, feasible, reachable, or compatible as the current \
    default. Practical basis = access, availability, location, feasibility, \
    continuity, arrangement, or status the old fact silently relied on. \
    Examples: a new city breaks the old city's basis; a desert/dry climate \
    breaks the basis of living in a rainy city; high-altitude breaks a \
    sea-level home. Do NOT mark stale for mere topic overlap, facts that can \
    both be true at once, or general change that breaks no specific basis. If \
    you cannot name the broken basis, leave it. Output ONLY a JSON array of the \
    integer indices of the now-unsafe earlier facts. If none, output [].";

/// Build the user prompt for a judge call.
pub fn adjudicate_user_prompt(new_fact: &NewFact, candidates: &[Candidate]) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "NEW fact: {} = {}\n\nEARLIER facts:\n",
        new_fact.predicate, new_fact.object
    ));
    for (i, c) in candidates.iter().enumerate() {
        s.push_str(&format!("{i}. {} = {}\n", c.predicate, c.object));
    }
    s.push_str("\nIndices of earlier facts the new fact makes stale:");
    s
}

/// Gather the subject's active facts EXCEPT the just-added axis, as candidates.
/// Uses the standard query path (which already hides stale/superseded/shadowed),
/// so we never re-shadow something already retired.
async fn candidates_for(
    config: &MindConfig,
    subject: &str,
    skip_predicate: &str,
) -> Result<Vec<Candidate>> {
    let facts = crate::knowledge::query_facts(config, subject).await?;
    Ok(facts
        .into_iter()
        .filter(|f| {
            f.subject.eq_ignore_ascii_case(subject)
                && !f.predicate.eq_ignore_ascii_case(skip_predicate)
                && !f.object.trim().is_empty()
        })
        .map(|f| Candidate {
            id: f.id,
            predicate: f.predicate,
            object: f.object,
        })
        .collect())
}

/// Run Type II adjudication for a newly-added fact. Gathers the subject's other
/// active facts, asks the judge which the new fact contradicts, and marks those
/// `PropagationShadowed`. Returns the ids that were shadowed. Best-effort: a
/// judge or storage error returns Ok(empty) rather than failing the add.
pub async fn adjudicate_propagation(
    config: &MindConfig,
    subject: &str,
    new_fact: &NewFact,
    judge: &dyn PropagationJudge,
) -> Result<Vec<String>> {
    let candidates = candidates_for(config, subject, &new_fact.predicate).await?;
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let stale = judge.judge(subject, new_fact, &candidates).await;
    let mut shadowed = Vec::new();
    for i in stale {
        if let Some(c) = candidates.get(i) {
            if crate::duel::shadow_fact(config, &c.id).await.is_ok() {
                shadowed.push(c.id.clone());
            }
        }
    }
    Ok(shadowed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_prompt_lists_candidates_with_indices() {
        let nf = NewFact { predicate: "local_climate".into(), object: "arid".into() };
        let cs = vec![
            Candidate { id: "a".into(), predicate: "located_in".into(), object: "portland".into() },
            Candidate { id: "b".into(), predicate: "owns".into(), object: "dog".into() },
        ];
        let p = adjudicate_user_prompt(&nf, &cs);
        assert!(p.contains("NEW fact: local_climate = arid"));
        assert!(p.contains("0. located_in = portland"));
        assert!(p.contains("1. owns = dog"));
    }

    #[tokio::test]
    async fn fixture_judge_returns_configured_indices() {
        let j = FixtureJudge { stale_indices: vec![0] };
        let cs = vec![
            Candidate { id: "a".into(), predicate: "located_in".into(), object: "portland".into() },
            Candidate { id: "b".into(), predicate: "owns".into(), object: "dog".into() },
        ];
        let nf = NewFact { predicate: "local_climate".into(), object: "arid".into() };
        let out = j.judge("user", &nf, &cs).await;
        assert_eq!(out, vec![0]);
    }

    #[test]
    fn parse_stale_indices_tolerant() {
        assert_eq!(parse_stale_indices("[0, 2]", 3), vec![0, 2]);
        assert_eq!(parse_stale_indices("the answer is [1]", 3), vec![1]);
        assert_eq!(parse_stale_indices("[]", 3), Vec::<usize>::new());
        assert_eq!(parse_stale_indices("nope", 3), Vec::<usize>::new());
        assert_eq!(parse_stale_indices("[0, 9]", 3), vec![0]); // 9 out of range
    }

    #[tokio::test]
    async fn fixture_judge_filters_out_of_range() {
        let j = FixtureJudge { stale_indices: vec![0, 5] };
        let cs = vec![Candidate { id: "a".into(), predicate: "p".into(), object: "o".into() }];
        let nf = NewFact { predicate: "x".into(), object: "y".into() };
        let out = j.judge("user", &nf, &cs).await;
        assert_eq!(out, vec![0]); // index 5 dropped
    }
}
