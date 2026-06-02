//! Auto-extraction / auto-ingest (phase Д2, PR3).
//!
//! The system extracts memory from the stream instead of relying on a manual
//! `mind_add`. Judgment is pluggable, and the priority is INVERTED from the first
//! draft:
//!   1. Agent-driven (PRIMARY) — the agent is already a frontier LLM in the loop;
//!      it calls `mind_ingest` with candidates it already extracted. That is the
//!      "local judgment, no cloud" mode and the strongest one. In this mode the
//!      agent IS the significance gate (it only sends what is worth keeping).
//!   2. Heuristics (BACKSTOP) — for raw turns / dumb clients that paste a
//!      transcript. Marker-based (remember/always/never/"my X is"/decisions).
//!      Catches a slice without judgment, so it is a backstop, not the default.
//!   3. BYO-LLM — opt-in, off by default (deferred; would break LLM-free identity).
//!
//! Pipeline: capture -> extract -> secret-scrub -> dedup (near-dup) -> write.
//! Consolidation (PR2) is the separate, mandatory companion that controls bloat.
//! Memory/fact candidates are written here; procedure candidates are routed to
//! the procedural-memory module (Д6), learned unverified.

use anyhow::Result;
use serde::Deserialize;

use crate::config::MindConfig;

/// Skip writing a memory whose nearest existing neighbor is at least this similar
/// (near-dup). Slightly looser than consolidation's merge threshold: at ingest we
/// would rather not write a redundant memory in the first place.
const INGEST_DEDUP_THRESHOLD: f32 = 0.95;

/// A typed extraction candidate. Agent-driven mode sends these directly (tagged
/// JSON); the heuristic extractor produces them from raw text.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Candidate {
    /// An ordinary note to store in `library`.
    Memory { content: String },
    /// A knowledge-graph triple.
    Fact {
        subject: String,
        predicate: String,
        object: String,
    },
    /// An error->fix playbook, written via the procedural-memory module (Д6).
    Procedure {
        #[serde(default)]
        trigger_error: String,
        #[serde(default)]
        fix: String,
        #[serde(default)]
        context: String,
    },
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct IngestReport {
    pub considered: usize,
    pub stored_memories: usize,
    pub stored_facts: usize,
    pub skipped_dup: usize,
    pub skipped_secret: usize,
    pub stored_procedures: usize,
    /// Routed to the v0.11 quarantine layer (didn't pass the relevance gate
    /// but not dropped — kept retrievable for re-submission detection).
    pub quarantined: usize,
    /// Existing quarantined point promoted to normal memory because the user
    /// re-asserted it (the loop-breaker the critic flagged).
    pub promoted: usize,
}

impl IngestReport {
    pub fn render(&self) -> String {
        let mut s = format!(
            "Ingested: {} memory, {} fact(s) from {} candidate(s).",
            self.stored_memories, self.stored_facts, self.considered
        );
        if self.skipped_dup > 0 {
            s.push_str(&format!(
                "\nSkipped {} near-duplicate(s).",
                self.skipped_dup
            ));
        }
        if self.skipped_secret > 0 {
            s.push_str(&format!(
                "\nSkipped {} candidate(s) that looked like secrets (use the vault).",
                self.skipped_secret
            ));
        }
        if self.stored_procedures > 0 {
            s.push_str(&format!(
                "\nLearned {} procedure(s).",
                self.stored_procedures
            ));
        }
        if self.quarantined > 0 {
            s.push_str(&format!(
                "\nQuarantined {} candidate(s) below the relevance gate (re-assert to promote).",
                self.quarantined
            ));
        }
        if self.promoted > 0 {
            s.push_str(&format!(
                "\nPromoted {} quarantined entry/entries on re-assertion.",
                self.promoted
            ));
        }
        s
    }
}

/// Heuristic extractor (backstop). Pure and line-based: pulls candidates from raw
/// text using explicit markers. Conservative on purpose - it is a fallback for
/// non-agent input, not the primary path, so it favors precision over recall.
pub fn extract_heuristic(raw: &str) -> Vec<Candidate> {
    let mut out = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.len() < 4 {
            continue;
        }
        let lower = line.to_lowercase();

        // "my <key> is <value>" -> fact (user -> <key> -> <value>).
        if let Some(rest) = lower.strip_prefix("my ")
            && let Some(idx) = rest.find(" is ")
        {
            // Recover original-case slices by byte offsets into `line`.
            let key = line["my ".len().."my ".len() + idx].trim().to_string();
            let value = line["my ".len() + idx + " is ".len()..]
                .trim()
                .trim_end_matches(['.', '!'])
                .to_string();
            if !key.is_empty() && !value.is_empty() {
                out.push(Candidate::Fact {
                    subject: "user".to_string(),
                    predicate: key,
                    object: value,
                });
                continue;
            }
        }

        // Memory markers: keep the salient part as a note.
        let memory = if let Some(r) = lower.strip_prefix("remember that ") {
            Some(line[line.len() - r.len()..].trim().to_string())
        } else if let Some(r) = lower
            .strip_prefix("remember: ")
            .or_else(|| lower.strip_prefix("note that "))
            .or_else(|| lower.strip_prefix("note: "))
        {
            Some(line[line.len() - r.len()..].trim().to_string())
        } else if lower.starts_with("always ")
            || lower.starts_with("never ")
            || lower.starts_with("we decided ")
            || lower.starts_with("decision: ")
        {
            // A rule / decision: keep the whole line as the note.
            Some(line.to_string())
        } else {
            None
        };

        if let Some(content) = memory
            && content.len() >= 4
        {
            out.push(Candidate::Memory { content });
        }
    }
    out
}

/// Run the ingest pipeline. If `candidates` is empty and `raw` is present, the
/// heuristic extractor produces candidates (backstop mode); otherwise the
/// agent-supplied candidates are used (primary mode). Every candidate is
/// secret-scrubbed and (for memories) near-dup checked before writing.
pub async fn run_ingest(
    config: &MindConfig,
    raw: Option<&str>,
    candidates: Vec<Candidate>,
    library: &str,
) -> Result<IngestReport> {
    let candidates = if candidates.is_empty() {
        match raw {
            Some(r) => extract_heuristic(r),
            None => Vec::new(),
        }
    } else {
        candidates
    };

    let mut report = IngestReport {
        considered: candidates.len(),
        ..Default::default()
    };

    let gate_cfg = crate::relevance::GateConfig::default();

    for cand in candidates {
        match cand {
            Candidate::Memory { content } => {
                if crate::secrets::scan(&content).is_some() {
                    report.skipped_secret += 1;
                    continue;
                }

                // Relevance gate (v0.11). Cheap filters first: length, blacklists,
                // decision markers. A "Quarantine" verdict does NOT drop the
                // candidate — it routes to the quarantine layer so a future
                // re-assertion can promote it. Silently dropping is exactly the
                // user-loop the critic flagged.
                let rcand = crate::relevance::Candidate {
                    content: &content,
                    source: Some("ingest"),
                    tool_name: None,
                };
                if let crate::relevance::Verdict::Quarantine { reason } =
                    crate::relevance::check_cheap(&rcand, &gate_cfg)
                {
                    // Re-assertion check: if the same content already lives in
                    // quarantine (deterministic id), this is the promotion
                    // signal — user is insistent, raise confidence.
                    let qid =
                        crate::storage::quarantine_id_for(library, content.trim());
                    if crate::storage::promote_from_quarantine(config, &qid)
                        .await
                        .unwrap_or(false)
                    {
                        report.promoted += 1;
                        continue;
                    }
                    // Otherwise, quarantine the candidate (write with the flag,
                    // do not surface in ordinary search).
                    let _ = crate::storage::add_quarantined(
                        config,
                        library,
                        &content,
                        Some("ingest"),
                        &reason,
                    )
                    .await?;
                    report.quarantined += 1;
                    continue;
                }

                // Near-dup check (the missing audit #8 primitive): skip writing a
                // memory that already has a very similar neighbor.
                if let Ok(Some(score)) =
                    crate::storage::nearest_score(config, Some(library), &content).await
                    && score >= INGEST_DEDUP_THRESHOLD
                {
                    report.skipped_dup += 1;
                    continue;
                }
                // add_memory also secret-scrubs and is idempotent on exact content.
                let n =
                    crate::storage::add_memory(config, library, &content, Some("ingest")).await?;
                if n > 0 {
                    report.stored_memories += 1;
                }
            }
            Candidate::Fact {
                subject,
                predicate,
                object,
            } => {
                // A fact value that is itself a secret must not be stored as a fact.
                if crate::secrets::scan(&object).is_some() {
                    report.skipped_secret += 1;
                    continue;
                }
                crate::knowledge::add_fact(config, &subject, &predicate, &object).await?;
                report.stored_facts += 1;
            }
            Candidate::Procedure {
                trigger_error,
                fix,
                context,
            } => {
                if trigger_error.trim().is_empty() || fix.trim().is_empty() {
                    continue;
                }
                // Learned unverified (no truth signal at ingest time); surfaced
                // with low weight until a real signal confirms it (Д6).
                crate::procedure::learn(config, &trigger_error, &fix, &context, None, false)
                    .await?;
                report.stored_procedures += 1;
            }
        }
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heuristic_extracts_my_x_is_as_fact() {
        let c = extract_heuristic("My editor is Helix.");
        assert_eq!(
            c,
            vec![Candidate::Fact {
                subject: "user".into(),
                predicate: "editor".into(),
                object: "Helix".into(),
            }]
        );
    }

    #[test]
    fn heuristic_extracts_remember_as_memory() {
        let c = extract_heuristic("Remember that the staging DB is Postgres 16.");
        assert_eq!(
            c,
            vec![Candidate::Memory {
                content: "the staging DB is Postgres 16.".into()
            }]
        );
    }

    #[test]
    fn heuristic_keeps_rules_whole() {
        let c = extract_heuristic("Always run tests before pushing.");
        assert_eq!(
            c,
            vec![Candidate::Memory {
                content: "Always run tests before pushing.".into()
            }]
        );
    }

    #[test]
    fn heuristic_ignores_plain_chatter() {
        // No marker -> nothing extracted (precision over recall).
        assert!(extract_heuristic("how's the weather today?").is_empty());
    }

    #[test]
    fn candidate_deserializes_tagged_json() {
        let m: Candidate = serde_json::from_str(r#"{"type":"memory","content":"hi"}"#).unwrap();
        assert_eq!(
            m,
            Candidate::Memory {
                content: "hi".into()
            }
        );
        let f: Candidate = serde_json::from_str(
            r#"{"type":"fact","subject":"user","predicate":"likes","object":"rust"}"#,
        )
        .unwrap();
        assert_eq!(
            f,
            Candidate::Fact {
                subject: "user".into(),
                predicate: "likes".into(),
                object: "rust".into()
            }
        );
    }

    #[test]
    fn report_render_mentions_counts() {
        let r = IngestReport {
            considered: 3,
            stored_memories: 1,
            stored_facts: 1,
            skipped_dup: 1,
            ..Default::default()
        };
        let s = r.render();
        assert!(s.contains("1 memory"));
        assert!(s.contains("near-duplicate"));
    }
}
