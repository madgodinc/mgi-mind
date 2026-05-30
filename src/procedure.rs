//! Procedural memory — "learning from screw-ups" (phase Д6).
//!
//! Playbooks of "how we fix this", primarily error -> fix. A special case of
//! extraction + retrieval at task time. Stored as `type = procedure` points (see
//! `storage::add_procedure`); the error signature drives a lexical/sparse match
//! (exact codes/identifiers) and the task context drives a dense/semantic match.
//!
//! Truth signal (fundamental, from the spec): without a "the fix actually worked"
//! signal you learn superstitions. A reliable `verified = true` needs a
//! deterministic signal (test green / exit 0) from the harness, not from mgimind.
//! So:
//!   - MVP shipping now: manual `mind_learn(error, fix)` with `verified = false`.
//!   - Reliable mode (deferred): a hook on the verification signal sets verified.
//!
//! Proactivity rule: only `verified` is surfaced proactively; unverified is
//! low-weight. On reuse, a fix that fails again raises fail_count (via
//! `mind_procedure_outcome`) and is demoted, so the store self-corrects instead
//! of ossifying on a bad playbook.

use anyhow::Result;

use crate::config::MindConfig;
use crate::storage::{self, ProcedureHit};

/// Normalize an error signature so the same error matches regardless of volatile
/// detail: drop file paths, line:col numbers, hex addresses, long hashes, and
/// bare numbers. Keeps error codes (e.g. `E0599`) and identifiers. Pure + tested.
pub fn normalize_error(raw: &str) -> String {
    raw.split_whitespace()
        .filter_map(normalize_token)
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_hex(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_hexdigit())
}

fn normalize_token(tok: &str) -> Option<String> {
    // File paths -> placeholder.
    if tok.contains('/') || tok.contains('\\') {
        return Some("<path>".to_string());
    }
    // Hex memory address.
    if let Some(rest) = tok.strip_prefix("0x")
        && is_hex(rest)
    {
        return Some("<addr>".to_string());
    }
    // Drop empty and pure-number colon segments (line:col like `:12:5`), keeping
    // codes/identifiers (`E0599`, `main.rs`).
    let kept: Vec<&str> = tok
        .split(':')
        .filter(|s| !s.is_empty() && !s.chars().all(|c| c.is_ascii_digit()))
        .collect();
    let joined = kept.join(":");
    if joined.is_empty() {
        return None; // token was all numbers / colons
    }
    if joined.chars().all(|c| c.is_ascii_digit()) {
        return Some("<n>".to_string());
    }
    if joined.len() >= 12 && is_hex(&joined) {
        return Some("<hash>".to_string());
    }
    Some(joined)
}

/// Rank recalled procedures by the proactivity rule: verified first, then by net
/// success (success - fail), then by retrieval score. Pure, so it is unit-tested.
pub fn rank(mut hits: Vec<ProcedureHit>) -> Vec<ProcedureHit> {
    hits.sort_by(|a, b| {
        b.verified
            .cmp(&a.verified)
            .then((b.success_count - b.fail_count).cmp(&(a.success_count - a.fail_count)))
            .then(
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
    });
    hits
}

/// Record a lesson: error -> fix. `verified` is false for a manual `mind_learn`
/// (no truth signal yet). Returns a confirmation with the procedure id.
pub async fn learn(
    config: &MindConfig,
    error: &str,
    fix: &str,
    context: &str,
    provenance: Option<&str>,
    verified: bool,
) -> Result<String> {
    let norm = normalize_error(error);
    if norm.is_empty() || fix.trim().is_empty() {
        anyhow::bail!("mind_learn needs a non-empty error signature and fix");
    }
    let id = storage::add_procedure(config, &norm, context, fix, provenance, verified).await?;
    Ok(format!(
        "Learned procedure [id: {id}]\n  error: {norm}\n  fix:   {fix}\n  verified: {verified} \
         (unverified lessons are surfaced with low weight until a real signal confirms them)"
    ))
}

/// Recall and rank playbooks for an error and/or task context.
pub async fn recall(
    config: &MindConfig,
    error: Option<&str>,
    context: Option<&str>,
    limit: usize,
) -> Result<String> {
    let norm = error.map(normalize_error);
    let hits = storage::recall_procedures(config, norm.as_deref(), context, limit).await?;
    let ranked = rank(hits);
    Ok(render(&ranked, limit))
}

fn render(hits: &[ProcedureHit], limit: usize) -> String {
    if hits.is_empty() {
        return "No matching procedures found.".to_string();
    }
    let mut s = String::from("Procedures (verified first):\n");
    for h in hits.iter().take(limit) {
        let mark = if h.verified {
            "✓ verified"
        } else {
            "· unverified"
        };
        s.push_str(&format!(
            "\n[{mark}] (✓{}/✗{}) id: {}\n  error: {}\n  fix:   {}\n",
            h.success_count, h.fail_count, h.id, h.trigger_error, h.fix
        ));
        if !h.trigger_context.is_empty() {
            s.push_str(&format!("  when:  {}\n", h.trigger_context));
        }
        if let Some(p) = &h.provenance {
            s.push_str(&format!("  from:  {p}\n"));
        }
    }
    s
}

/// Record the outcome of reusing a procedure (self-correction loop).
pub async fn outcome(config: &MindConfig, id: &str, worked: bool) -> Result<String> {
    storage::procedure_outcome(config, id, worked).await?;
    Ok(format!(
        "Recorded outcome for {id}: {}.",
        if worked {
            "worked (success++)"
        } else {
            "failed (fail++, demoted)"
        }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(id: &str, verified: bool, succ: i64, fail: i64, score: f32) -> ProcedureHit {
        ProcedureHit {
            id: id.into(),
            trigger_error: "e".into(),
            trigger_context: String::new(),
            fix: "f".into(),
            provenance: None,
            verified,
            success_count: succ,
            fail_count: fail,
            score,
        }
    }

    #[test]
    fn normalize_strips_paths_lines_addrs_numbers() {
        let n = normalize_error("error[E0599]: no method foo at src/main.rs:12:5");
        assert!(n.contains("error[E0599]"));
        assert!(n.contains("<path>"));
        assert!(!n.contains("12"));
        assert!(!n.contains("src/main.rs"));
    }

    #[test]
    fn normalize_collapses_addresses_and_hashes() {
        let n = normalize_error("segfault at 0xdeadbeef hash 0123456789abcdef");
        assert!(n.contains("<addr>"));
        assert!(n.contains("<hash>"));
    }

    #[test]
    fn normalize_is_stable_across_volatile_detail() {
        let a = normalize_error("panic at src/a.rs:10:2 code 42");
        let b = normalize_error("panic at src/b.rs:99:7 code 17");
        assert_eq!(a, b, "only volatile parts differ -> same signature");
    }

    #[test]
    fn rank_puts_verified_first() {
        let hits = vec![hit("unv", false, 10, 0, 0.9), hit("ver", true, 0, 0, 0.1)];
        let r = rank(hits);
        assert_eq!(
            r[0].id, "ver",
            "verified outranks a high-scoring unverified"
        );
    }

    #[test]
    fn rank_demotes_failing_fix() {
        let hits = vec![hit("bad", false, 0, 5, 0.9), hit("good", false, 5, 0, 0.1)];
        let r = rank(hits);
        assert_eq!(r[0].id, "good", "net-positive fix outranks a failing one");
    }

    #[test]
    fn render_empty_is_friendly() {
        assert_eq!(render(&[], 5), "No matching procedures found.");
    }
}
