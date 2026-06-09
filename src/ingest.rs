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

/// How many semantic neighbors to pull for the v0.11 novelty check. A handful
/// is enough — the union of their tokens is the comparison set; pulling more
/// shifts the baseline toward "everything is similar to something", which is
/// the opposite of what we want.
const NOVELTY_NEIGHBORS: u64 = 3;

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
    run_ingest_authored(config, raw, candidates, library, None).await
}

/// Like `run_ingest` but tags every written memory/fact with the asserting
/// agent (multi-agent HTTP path). The plain `run_ingest` stays unattributed.
pub async fn run_ingest_authored(
    config: &MindConfig,
    raw: Option<&str>,
    candidates: Vec<Candidate>,
    library: &str,
    author: Option<&str>,
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
                if let Some(hit) = crate::secrets::scan(&content) {
                    report.skipped_secret += 1;
                    let label = hit.reason;
                    // Record the drop WITHOUT the content (it's a secret) — just
                    // the detector label, so the skip is auditable but nothing
                    // sensitive lands in the log.
                    crate::audit::record(
                        crate::audit::AuditEvent::new(
                            crate::audit::AuditOp::SkipSecret,
                            library.to_string(),
                            String::new(),
                        )
                        .actor("ingest")
                        .note(format!("secret-skipped ({label})")),
                    );
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
                let cheap_verdict = crate::relevance::check_cheap(&rcand, &gate_cfg);
                // Second-tier novelty check (v0.11). Only runs if cheap passed.
                // Pulls top-k semantic neighbors, tokenizes their content, and
                // computes the share of candidate tokens that are NEW relative
                // to the neighborhood. A low-novelty candidate adds no new
                // tokens — it's a paraphrase of what's already stored. Note
                // this is NOT cosine-noise filtering (that's invariant #4 — a
                // repeat IS a confidence signal); it's a *token-overlap* check
                // that detects "same words just rearranged".
                let novelty_verdict = if cheap_verdict.is_accept() {
                    match crate::storage::top_k_neighbor_content(
                        config,
                        Some(library),
                        &content,
                        NOVELTY_NEIGHBORS,
                    )
                    .await
                    {
                        Ok(neighbors) if !neighbors.is_empty() => {
                            let neighbor_tokens: Vec<String> = neighbors
                                .iter()
                                .flat_map(|n| crate::relevance::tokenize(n))
                                .collect();
                            let novelty =
                                crate::relevance::novelty_ratio(&content, &neighbor_tokens);
                            if novelty < gate_cfg.min_novelty {
                                crate::relevance::Verdict::Quarantine {
                                    reason: "low_novelty".into(),
                                }
                            } else {
                                crate::relevance::Verdict::Accept
                            }
                        }
                        // No neighbors yet (empty library, or query failed
                        // softly) — accept, novelty cannot be assessed.
                        _ => crate::relevance::Verdict::Accept,
                    }
                } else {
                    cheap_verdict
                };

                if let crate::relevance::Verdict::Quarantine { reason } = novelty_verdict {
                    // Re-assertion check: if the same content already lives in
                    // quarantine (deterministic id), this is the promotion
                    // signal — user is insistent, raise confidence.
                    let qid = crate::storage::quarantine_id_for(library, content.trim());
                    if crate::storage::promote_from_quarantine(config, &qid)
                        .await
                        .unwrap_or(false)
                    {
                        report.promoted += 1;
                        continue;
                    }
                    // Otherwise, quarantine the candidate (write with the flag,
                    // do not surface in ordinary search). add_quarantined already
                    // emits the single Quarantine audit event (content + reason),
                    // so the tally counts it without a second redundant emit here.
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
                    // This drop is UNRECOVERABLE (no quarantine row), and a
                    // correction to a stale fact is exactly the case most likely
                    // to read as a near-dup of the thing it means to replace. So
                    // log the dropped content + the score, making a "lost write"
                    // answerable from the audit log and giving us the data to
                    // decide later whether this threshold eats real corrections.
                    crate::audit::record(
                        crate::audit::AuditEvent::new(
                            crate::audit::AuditOp::SkipDup,
                            library.to_string(),
                            String::new(),
                        )
                        .actor("ingest")
                        .after(crate::storage::truncate_for_audit(&content))
                        .note(format!(
                            "near-dup skip (score {score:.3} ≥ {INGEST_DEDUP_THRESHOLD})"
                        )),
                    );
                    continue;
                }
                // add_memory also secret-scrubs and is idempotent on exact content.
                let n = crate::storage::add_memory_authored(
                    config,
                    library,
                    &content,
                    Some("ingest"),
                    author,
                )
                .await?;
                if n > 0 {
                    report.stored_memories += 1;
                    // Emit the dedicated Ingest op so "audit writes" can count
                    // genuine ingest stores apart from manual mind_add (which
                    // emits Add). add_memory_authored already logged the Add with
                    // the content; this carries no content, just the store signal.
                    crate::audit::record(
                        crate::audit::AuditEvent::new(
                            crate::audit::AuditOp::Ingest,
                            library.to_string(),
                            String::new(),
                        )
                        .actor("ingest")
                        .note("stored via ingest"),
                    );
                    // v1.4 Phase 5 step 5.5: fire-and-forget auto-extraction
                    // through a bounded mpsc queue (post-critic fix). The
                    // worker is a single dedicated task; bursts drop
                    // overflow rather than spawn unbounded futures.
                    #[cfg(feature = "extractor")]
                    if crate::extractor::is_llama_server_installed() {
                        crate::extractor::enqueue_auto_extract(config, &content);
                    }
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
                crate::knowledge::add_fact_authored(config, &subject, &predicate, &object, author)
                    .await?;
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
