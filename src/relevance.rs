#![allow(dead_code)]
// AccessAction::reason() is the diagnostics surface for the relevance gate;
// `mgimind doctor` and a future viewer call it. Hot path uses the boolean
// gate without reading the reason string.

//! Relevance gate for ingest candidates (phase Д2, v0.11).
//!
//! The auto-ingest path (`mind_ingest`) needs a heuristic filter that catches
//! obvious noise BEFORE it lands in the searchable store. Without it,
//! agent-driven ingest is at the mercy of the agent's judgment alone, and the
//! heuristic-backstop path is even more vulnerable to dumping low-signal
//! turns into memory.
//!
//! The gate is intentionally cheap and explainable. No LLM, no embedding cost,
//! no per-candidate model run. Each filter returns a small enum saying "pass"
//! or "reject with this reason", and the caller decides whether to write to
//! memory normally, route to quarantine (v0.11), or skip entirely.
//!
//! The critic was explicit on what NOT to put here:
//!
//! - **No cosine-noise filter.** "Looks similar to something already stored" is
//!   NOT a relevance signal — frequent repetition of the same fact is a
//!   confidence signal, not a deduplication trigger. Consolidate handles dup
//!   storage; the gate handles input quality.
//! - **Novelty by tokens, not by NER.** A full NER+entity-diff pipeline is its
//!   own subproject and contradicts the thin-surface principle. We use the
//!   sparse (BM25) index that already exists in storage to ask "how many of
//!   the candidate's tokens are absent from the top-k neighbors?". High share
//!   of new tokens → new fact. Low share → paraphrase of stored content.
//!
//! Decision-markers are bilingual (RU+EN) from the start because mgi-mind's
//! content is mixed and an English-only marker list would silently fail on
//! ~half of typical Mad-flavored input.
//!
//! Filters are ordered by cost (length/blacklist first, novelty last) so the
//! cheap ones cut the candidate list before we touch any neighbor search.

use serde::Serialize;

/// Outcome of running a candidate through the gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum Verdict {
    /// The candidate looks like real memory worth surfacing. Caller writes it
    /// normally (no quarantine flag).
    Accept,
    /// The candidate may be useful but doesn't clearly pass. Caller writes it
    /// to quarantine — visible to re-submission checks but excluded from
    /// ordinary search. The `reason` is a short label like "too_short" or
    /// "blacklist_path".
    Quarantine { reason: String },
}

impl Verdict {
    pub fn is_accept(&self) -> bool {
        matches!(self, Verdict::Accept)
    }
    pub fn reason(&self) -> Option<&str> {
        match self {
            Verdict::Accept => None,
            Verdict::Quarantine { reason } => Some(reason),
        }
    }
}

/// Every `quarantine_reason` value this gate can emit, plus the bulk-migration
/// label written outside the gate. The single source of truth for surfaces that
/// tally quarantine by reason (e.g. the context digest) — keep it in sync with
/// the `Quarantine { reason: ... }` sites above and `ingest.rs`. A reason not in
/// this list still gets counted under "other" by such surfaces.
pub const KNOWN_REASONS: &[&str] = &[
    "too_short",
    "too_few_words",
    "too_long",
    "low_novelty",
    "blacklist_path",
    "blacklist_doc",
    "blacklist_tool",
    // Bulk-cleanup label from the 2026-05-29 migration sweep (Step 1), written
    // outside the gate by the vendored-doc quarantine pass — the bulk of the
    // current quarantine, so it must be named exactly or it all reads as "other".
    "migration-vendor-doc",
    // A near-duplicate at ingest, routed to quarantine instead of dropped, so a
    // wrongly-suppressed correction is recoverable (circle 1, write discipline).
    // Written outside the gate by the ingest near-dup path, named here so the
    // quarantine breakdown counts it instead of folding it into "other".
    "near_dup_drop",
];

/// Knobs for the gate. Defaults are conservative — tightening them later is
/// cheap, loosening them only matters once we have counterfactual numbers.
#[derive(Debug, Clone)]
pub struct GateConfig {
    /// Reject candidates shorter than this (characters, after trim).
    pub min_chars: usize,
    /// Reject candidates with fewer than this many word-like tokens.
    /// 3 catches "ok thanks" / "yep" / "hello" but lets "Aurora is alive" through.
    pub min_words: usize,
    /// Reject candidates longer than this in characters. Very long blobs are
    /// usually dumps of code or full files — they should go through
    /// `mind_provenance_add` (with origin/repo/line metadata), not generic
    /// auto-ingest. Default 8000 ≈ ~2k tokens.
    pub max_chars: usize,
    /// Source paths matching any of these substrings get quarantined.
    /// Defaults to common noise sources (lock files, build artifacts, IDE
    /// metadata, secrets-bearing dotfiles).
    pub blacklist_path_substrings: Vec<String>,
    /// Vendored-doc filename stems (readme, changelog, license, …). Matched
    /// against the source's FILENAME STEM only — a file named `README.md`
    /// matches, but a path containing "readme" elsewhere (e.g. a project named
    /// `readme-generator`) does NOT. Keeps a migration's dumped third-party docs
    /// out without quarantining the user's own files.
    pub blacklist_doc_stems: Vec<String>,
    /// Tool names matching any of these get quarantined. Defaults to read-only
    /// tools whose output is usually transient and rarely worth storing.
    pub blacklist_tool_names: Vec<String>,
    /// Decision markers — phrases that strongly indicate "the user wants this
    /// recorded". A hit overrides the novelty check (it's worth saving even
    /// if it overlaps with existing content). Bilingual: ru + en.
    pub decision_markers: Vec<String>,
    /// Novelty threshold: ratio of new tokens (vs union of top-k neighbors)
    /// below which the candidate is treated as a paraphrase and quarantined.
    /// Range 0.0..1.0. 0.3 = "at least 30% of tokens must be new".
    pub min_novelty: f32,
}

impl Default for GateConfig {
    fn default() -> Self {
        Self {
            min_chars: 12,
            min_words: 3,
            max_chars: 8_000,
            blacklist_path_substrings: vec![
                ".env".into(),
                "target/".into(),
                "node_modules/".into(),
                ".git/".into(),
                ".lock".into(),
                ".cache/".into(),
                "/tmp/".into(),
            ],
            // Vendored project docs: a 2026-05-29 migration dumped 7800+
            // CHANGELOG/README of OTHER repos into memory. Matched against the
            // FILENAME STEM only (not a path substring) so a project legitimately
            // named "license-checker" or a folder "readme-assets" is NOT
            // quarantined — only a file actually called README.md / LICENSE / etc.
            blacklist_doc_stems: vec![
                "readme".into(),
                "changelog".into(),
                "license".into(),
                "licence".into(),
                "code_of_conduct".into(),
                "contributing".into(),
                "authors".into(),
                "notice".into(),
            ],
            blacklist_tool_names: vec![
                // These tend to surface ephemeral info; user-curated memory
                // benefits more from being explicit than from auto-capturing
                // them.
                "ls".into(),
                "pwd".into(),
                "echo".into(),
            ],
            decision_markers: vec![
                // EN
                "remember".into(),
                "always".into(),
                "never".into(),
                "my X is".into(),
                "i decided".into(),
                "we decided".into(),
                "important".into(),
                "note that".into(),
                "todo".into(),
                "fix:".into(),
                // RU
                "запомни".into(),
                "помни".into(),
                "всегда".into(),
                "никогда".into(),
                "важно".into(),
                "решили".into(),
                "решил".into(),
                "не забудь".into(),
                "учти".into(),
                "обрати внимание".into(),
            ],
            min_novelty: 0.30,
        }
    }
}

/// Input to the gate. Source path / tool name are optional because the
/// agent-driven path may not have them.
#[derive(Debug, Clone)]
pub struct Candidate<'a> {
    pub content: &'a str,
    pub source: Option<&'a str>,
    pub tool_name: Option<&'a str>,
}

/// Run a candidate through the cheap filters (length, blacklist). Returns
/// `Accept` if the candidate is clearly fine OR clearly hits a decision
/// marker (decision wins over the cheap rejections). Returns `Quarantine`
/// with a reason on rejection.
///
/// This does NOT run the novelty check — that one needs a neighbor lookup
/// (the sparse index), so it lives in `check_novelty` and gets called by
/// `ingest` after the cheap filters have already pruned the candidate list.
pub fn check_cheap(candidate: &Candidate<'_>, cfg: &GateConfig) -> Verdict {
    let content = candidate.content.trim();

    // Decision markers short-circuit: if the user said "remember this", we
    // accept even short or duplicate-looking content. Case-insensitive.
    if has_decision_marker(content, &cfg.decision_markers) {
        return Verdict::Accept;
    }

    // Length checks. Bare minimum to be worth storing; truly long blobs go
    // through provenance_add, not generic ingest.
    let char_count = content.chars().count();
    if char_count < cfg.min_chars {
        return Verdict::Quarantine {
            reason: "too_short".into(),
        };
    }
    if char_count > cfg.max_chars {
        return Verdict::Quarantine {
            reason: "too_long".into(),
        };
    }
    if word_count(content) < cfg.min_words {
        return Verdict::Quarantine {
            reason: "too_few_words".into(),
        };
    }

    // Blacklist by source path: lock files, build artifacts, secrets dirs.
    // Case-insensitive; patterns are authored lowercase. Path substrings here
    // are genuine path fragments (".env", "target/"), not doc names.
    if let Some(src) = candidate.source {
        let src_lower = src.to_lowercase();
        if cfg
            .blacklist_path_substrings
            .iter()
            .any(|p| src_lower.contains(p.as_str()))
        {
            return Verdict::Quarantine {
                reason: "blacklist_path".into(),
            };
        }
        // Vendored docs: match the FILENAME STEM only, so "README.md" is caught
        // but "readme-generator/notes.md" or a "license-checker" project is not.
        let basename = src_lower.rsplit(['/', '\\']).next().unwrap_or(&src_lower);
        let stem = basename.split('.').next().unwrap_or(basename);
        if cfg.blacklist_doc_stems.iter().any(|s| stem == s.as_str()) {
            return Verdict::Quarantine {
                reason: "blacklist_doc".into(),
            };
        }
    }

    // Blacklist by tool name.
    if let Some(tool) = candidate.tool_name
        && cfg
            .blacklist_tool_names
            .iter()
            .any(|t| t.eq_ignore_ascii_case(tool))
    {
        return Verdict::Quarantine {
            reason: "blacklist_tool".into(),
        };
    }

    Verdict::Accept
}

/// Novelty check against neighboring stored memories. Returns ratio of
/// candidate tokens that are NOT present in `neighbors_tokens`. The caller
/// supplies the union of tokens from top-k neighbors (typically pulled via
/// the sparse / BM25 index — cheap, reuses existing infrastructure).
///
/// Caller compares the result to `cfg.min_novelty` and quarantines under
/// reason `"low_novelty"` if it's below threshold.
pub fn novelty_ratio(candidate_content: &str, neighbors_tokens: &[String]) -> f32 {
    use std::collections::HashSet;
    let neighbors: HashSet<&str> = neighbors_tokens.iter().map(|s| s.as_str()).collect();
    let cand_tokens: Vec<String> = tokenize(candidate_content);
    if cand_tokens.is_empty() {
        return 1.0; // empty candidate is "novel" by convention; cheap-filter handles it
    }
    let total = cand_tokens.len() as f32;
    let new_count = cand_tokens
        .iter()
        .filter(|t| !neighbors.contains(t.as_str()))
        .count() as f32;
    new_count / total
}

/// Cheap tokenizer — lowercased ASCII/Unicode word fragments. Same flavor as
/// what the sparse index does, so the comparison sets line up. Not perfect
/// (no stemming, no language detection) but consistent.
pub fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .filter(|s| s.chars().count() >= 2)
        .map(|s| s.to_string())
        .collect()
}

fn word_count(text: &str) -> usize {
    text.split_whitespace()
        .filter(|s| s.chars().any(|c| c.is_alphanumeric()))
        .count()
}

fn has_decision_marker(content: &str, markers: &[String]) -> bool {
    let lower = content.to_lowercase();
    markers.iter().any(|m| lower.contains(&m.to_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(c: &str) -> Candidate<'_> {
        Candidate {
            content: c,
            source: None,
            tool_name: None,
        }
    }

    #[test]
    fn very_short_is_quarantined() {
        let cfg = GateConfig::default();
        assert_eq!(check_cheap(&cand("yep"), &cfg).reason(), Some("too_short"));
    }

    #[test]
    fn ordinary_sentence_passes() {
        let cfg = GateConfig::default();
        let v = check_cheap(
            &cand("Aurora is the streaming co-host project on FastAPI"),
            &cfg,
        );
        assert!(v.is_accept(), "should accept, got {:?}", v);
    }

    #[test]
    fn decision_marker_overrides_short() {
        let cfg = GateConfig::default();
        // Short, would normally fail length — but "remember" is a decision marker.
        let v = check_cheap(&cand("remember X"), &cfg);
        assert!(
            v.is_accept(),
            "decision marker should override, got {:?}",
            v
        );
    }

    #[test]
    fn russian_decision_marker_overrides_short() {
        let cfg = GateConfig::default();
        let v = check_cheap(&cand("запомни X"), &cfg);
        assert!(v.is_accept(), "RU marker should override, got {:?}", v);
    }

    #[test]
    fn blacklisted_path_quarantined() {
        let cfg = GateConfig::default();
        // Long enough content so length filter doesn't trigger; the path is
        // what should reject this — a dump from a .env file.
        let c = Candidate {
            content: "API_KEY=abcdef1234567890 some other config here too",
            source: Some("project/.env"),
            tool_name: None,
        };
        assert_eq!(check_cheap(&c, &cfg).reason(), Some("blacklist_path"));
    }

    #[test]
    fn vendored_doc_quarantined_case_insensitive() {
        let cfg = GateConfig::default();
        // Real migration dumps had uppercase filenames (README.md, CHANGELOG.md);
        // patterns are lowercase. Match must be case-insensitive or none of the
        // 7800+ vendored docs would be caught. Matched on filename stem.
        for path in [
            "other-repo/README.md",
            "vendor/CHANGELOG.md",
            "dep/LICENSE",
            "x/Contributing.md",
        ] {
            let c = Candidate {
                content: "Installation: run npm install then npm start to launch the app",
                source: Some(path),
                tool_name: None,
            };
            assert_eq!(
                check_cheap(&c, &cfg).reason(),
                Some("blacklist_doc"),
                "expected {path} to be blacklisted as a doc",
            );
        }
    }

    #[test]
    fn doc_stem_does_not_overmatch_paths() {
        let cfg = GateConfig::default();
        // The user's OWN files and projects whose path merely CONTAINS a doc word
        // must pass — the stem match is filename-only, not a path substring.
        for path in [
            "projects/license-checker/design.md",
            "readme-generator/src/main.rs",
            "notes/my-changelog-thoughts.md", // filename stem is "my-changelog-thoughts"
            "vendor/readme-assets/logo-notes.md",
        ] {
            let c = Candidate {
                content: "We decided to ship the narrow v0.1 first and widen it in v0.2 after release",
                source: Some(path),
                tool_name: None,
            };
            assert_eq!(
                check_cheap(&c, &cfg).reason(),
                None,
                "expected {path} to pass the gate (own file, not a vendored doc)",
            );
        }
    }

    #[test]
    fn blacklisted_tool_quarantined() {
        let cfg = GateConfig::default();
        // pwd output is usually a single path; but to make sure we hit the
        // tool filter rather than the word-count filter, pad with extra
        // words so cheap filters above don't fire first.
        let c = Candidate {
            content: "Result was /home/user/projects current working directory",
            source: None,
            tool_name: Some("pwd"),
        };
        assert_eq!(check_cheap(&c, &cfg).reason(), Some("blacklist_tool"));
    }

    #[test]
    fn very_long_quarantined() {
        let cfg = GateConfig {
            max_chars: 100,
            ..GateConfig::default()
        };
        let long = "word ".repeat(50); // 250 chars
        assert_eq!(check_cheap(&cand(&long), &cfg).reason(), Some("too_long"));
    }

    #[test]
    fn novelty_all_new() {
        let n = novelty_ratio("alpha beta gamma", &[]);
        assert!((n - 1.0).abs() < 1e-6);
    }

    #[test]
    fn novelty_full_overlap() {
        let n = novelty_ratio("alpha beta", &["alpha".into(), "beta".into()]);
        assert!((n - 0.0).abs() < 1e-6);
    }

    #[test]
    fn novelty_partial() {
        // 2 of 4 tokens are new -> 0.5
        let n = novelty_ratio("alpha beta gamma delta", &["alpha".into(), "beta".into()]);
        assert!((n - 0.5).abs() < 1e-6);
    }

    #[test]
    fn tokenize_mixed_language() {
        let t = tokenize("Aurora — это ИИ соведущий стрима, FastAPI/Gemma");
        assert!(t.contains(&"aurora".to_string()));
        assert!(t.contains(&"это".to_string()));
        assert!(t.contains(&"fastapi".to_string()));
        assert!(t.contains(&"gemma".to_string()));
        // Punctuation and short fragments dropped.
        assert!(!t.contains(&"—".to_string()));
    }

    #[test]
    fn few_words_quarantined() {
        let cfg = GateConfig::default();
        let v = check_cheap(&cand("ok thanks"), &cfg);
        // 9 chars, 2 words → caught by min_chars OR min_words
        assert!(!v.is_accept());
    }

    #[test]
    fn every_emitted_reason_is_in_known_reasons() {
        // Drift guard: the by-reason quarantine digest tallies KNOWN_REASONS, so
        // any reason the gate can actually emit must be listed there — else it
        // silently falls into the "other" bucket. Exercise each cheap branch and
        // assert its reason is known. (low_novelty comes from the novelty check,
        // migration-vendor-doc from the bulk sweep — both asserted present below.)
        let cfg = GateConfig::default();
        let long = "x ".repeat(cfg.max_chars); // > max_chars
        let cases = [
            cand("hi"),        // too_short
            cand("ok thanks"), // too_short/too_few_words
            cand(&long),       // too_long
            Candidate {
                content: "API_KEY=abcdef1234567890 plus more config text here too",
                source: Some("proj/.env"),
                tool_name: None,
            }, // blacklist_path
            Candidate {
                content: "Install: run npm install then npm start to launch the app",
                source: Some("vendor/README.md"),
                tool_name: None,
            }, // blacklist_doc
            Candidate {
                content: "the working directory is /home/x right now and nothing else",
                source: None,
                tool_name: Some("pwd"),
            }, // blacklist_tool
        ];
        for c in &cases {
            if let Some(reason) = check_cheap(c, &cfg).reason() {
                assert!(
                    KNOWN_REASONS.contains(&reason),
                    "gate emitted reason {reason:?} not in KNOWN_REASONS",
                );
            }
        }
        // The two reasons not produced by check_cheap must still be listed.
        assert!(KNOWN_REASONS.contains(&"low_novelty"));
        assert!(KNOWN_REASONS.contains(&"migration-vendor-doc"));
    }
}
