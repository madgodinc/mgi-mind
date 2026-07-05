//! Audit log for mutating operations.
//!
//! Separate from `access.rs` — that one tracks read hits for decay (process
//! memory + periodic flush, fundamentally a counter). This one tracks every
//! write/update/delete done against the memory store, so a future viewer or
//! `mgimind audit` command can answer "what changed, when, with what content".
//!
//! Why mandatory before the viewer ships invalidate/forget buttons: a destructive
//! UI without an audit trail is a sharp tool. The user clicks "forget", and
//! whatever was there is gone with no way to see what was lost or who pressed
//! the button. Append-only audit closes that — the actual ciphertext-equivalent
//! of the deleted memory lives in the log, retrievable for at least the
//! retention window.
//!
//! Wire format: NDJSON, one event per line, append-only file at
//! `$MGIMIND_HOME/audit.log`. Newline-delimited JSON survives a partial write at
//! end-of-file (the last broken line is discarded on parse), is grep-friendly,
//! and is trivial to rotate. Not Qdrant — audit data must survive a corrupted
//! vector store, and writing to a separate file means a panic mid-mutation can
//! still leave a trace.

use anyhow::{Context, Result};
use chrono::Utc;
use once_cell::sync::OnceCell;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// Operations we record. Everything that mutates the store goes through one
/// of these variants. Read operations are NOT audited (they go through
/// `access.rs` counters instead, by design — see audit #5).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuditOp {
    /// New memory written. `after` = stored content (post-scrub).
    Add,
    /// Existing memory replaced. `before` = old content if available, `after` = new.
    Update,
    /// Memory removed. `before` = content at time of deletion if available.
    Delete,
    /// Whole library created.
    LibraryCreate,
    /// Whole library dropped (every memory in it deleted in one shot).
    LibraryDrop,
    /// Fact (knowledge-graph triple) added.
    FactAdd,
    /// Fact invalidated (soft-deleted).
    FactInvalidate,
    /// Procedural memory recorded (error→fix lesson).
    ProcedureAdd,
    /// Outcome of a procedure replay recorded.
    ProcedureOutcome,
    /// Auto-extraction wrote candidates from an ingest call.
    Ingest,
    /// An ingest candidate was dropped as a near-duplicate of an existing
    /// memory (cosine ≥ the dedup threshold). `before` = the existing neighbor's
    /// content is not fetched, but `after` = the dropped candidate's content so
    /// "where did my write go?" is answerable. This drop is currently
    /// unrecoverable (unlike quarantine), which is exactly why it must be logged.
    SkipDup,
    /// An ingest candidate was routed to quarantine by the relevance gate
    /// (recoverable). `after` = the candidate; `note` = the gate reason.
    Quarantine,
    /// An ingest candidate was refused because it looked like a secret. `after`
    /// is intentionally omitted (never log the secret); `note` = the detector
    /// label only.
    SkipSecret,
    /// Consolidation merged or pruned memories.
    Consolidate,
    /// v1.5 Phase 8: background re-test pass promoted a fact to the
    /// doubt window. `before` = old confidence_score, `after` = new.
    /// `note` = "promote_to_doubt".
    RetestPromote,
    /// v1.5 Phase 8: background re-test pass recovered a fact from the
    /// doubt window. `before` = old confidence_score, `after` = new.
    /// `note` = "recover_from_doubt".
    RetestRecover,
    /// A cold memory was soft-forgotten (archived): hidden from search but
    /// retained and restorable. `note` = why (e.g. "cold: consolidate"). The
    /// reversible counterpart to Delete — kept for traceability so a restore is
    /// answerable from the log.
    Archive,
    /// An archived memory was restored to search. `target` = the memory id.
    Restore,
}

/// One audit record. Designed to be small enough that an unbounded log is fine
/// for typical use (kilobytes per day), and self-contained enough that a single
/// line is meaningful without context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    /// RFC3339 UTC timestamp.
    pub ts: String,
    /// What happened.
    pub op: AuditOp,
    /// Library name. Empty for library-level ops where the library itself is
    /// the object (Create/Drop name lives in `target`).
    pub library: String,
    /// The thing being touched: memory id, fact id, procedure id, or library
    /// name for library-level ops.
    pub target: String,
    /// Who/what initiated. Free-form string set by the caller — typically the
    /// CLI command name, the MCP tool name, or "auto" for ingest/consolidate.
    /// Defaults to "cli" so a missing tag still tells you the surface.
    #[serde(default = "default_actor")]
    pub actor: String,
    /// Content before the operation, if applicable and known. None for Add and
    /// library-level ops.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    /// Content after the operation, if applicable. None for Delete and
    /// library-level ops.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    /// Optional free-form note. Used by consolidate to say things like
    /// "merged N near-dups" without ballooning a single line per affected id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Tamper-evidence: BLAKE3 hex of the PREVIOUS log line's exact bytes (v2.4).
    /// Set at write time by `record`, chaining each entry to the last. `None` for
    /// the first entry and for legacy lines written before the chain existed —
    /// `audit verify` treats a run of `None`s as an unverified legacy prefix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev_hash: Option<String>,
}

fn default_actor() -> String {
    "cli".into()
}

impl AuditEvent {
    pub fn new(op: AuditOp, library: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            ts: Utc::now().to_rfc3339(),
            op,
            library: library.into(),
            target: target.into(),
            actor: default_actor(),
            before: None,
            after: None,
            note: None,
            prev_hash: None,
        }
    }

    pub fn actor(mut self, actor: impl Into<String>) -> Self {
        self.actor = actor.into();
        self
    }

    pub fn before(mut self, before: impl Into<String>) -> Self {
        self.before = Some(before.into());
        self
    }

    pub fn after(mut self, after: impl Into<String>) -> Self {
        self.after = Some(after.into());
        self
    }

    pub fn note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }
}

/// Global single-writer guard. The audit log is append-only and tiny per write,
/// so one mutex serializing across all callers is cheap and removes any chance
/// of interleaved partial lines. The OnceCell wraps an `Option<PathBuf>` so a
/// `disabled` config (or a missing MGIMIND_HOME) just makes audit a no-op
/// instead of crashing the write path.
static AUDIT_PATH: OnceCell<Option<PathBuf>> = OnceCell::new();
static AUDIT_LOCK: Mutex<()> = Mutex::new(());

/// Configure where the audit log lives. Called once at startup by the config
/// loader. Passing `None` disables auditing entirely (the recording functions
/// become no-ops). Tests use this to isolate per-test logs.
pub fn init(path: Option<PathBuf>) {
    // Set-once. If already set (e.g. tests calling twice), ignore — the first
    // init wins for the process lifetime.
    let _ = AUDIT_PATH.set(path);
}

fn current_path() -> Option<&'static PathBuf> {
    AUDIT_PATH.get().and_then(|opt| opt.as_ref())
}

/// Record an event. Never panics, never fails the caller. A logging failure
/// is itself logged via `tracing::warn` but does not propagate up — the mutate
/// operation has already succeeded by the time we're here, and refusing to
/// return success because we couldn't write a log line would be the wrong
/// tradeoff.
/// Chain tip: BLAKE3 hex of the last line written this process, so the next
/// `record` chains to it without re-reading the file. Seeded once from the file
/// tail on the first write. Only ever touched while holding `AUDIT_LOCK`.
static LAST_HASH: Mutex<Option<String>> = Mutex::new(None);
static LAST_HASH_SEEDED: AtomicBool = AtomicBool::new(false);

/// BLAKE3 hex of a log line's exact bytes (the string as written, no newline).
fn hash_line(line: &str) -> String {
    blake3::hash(line.as_bytes()).to_hex().to_string()
}

/// Hash of the last non-empty line already in `path`, or None if empty/absent.
/// Read once to continue the chain across process restarts.
fn seed_last_hash(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    content
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(hash_line)
}

pub fn record(mut event: AuditEvent) {
    // Emit a live pulse for the viewer's graph, independent of whether the
    // audit FILE is enabled — the visual feed should pulse even on a system
    // that has audit logging turned off.
    emit_pulse(&event);

    let Some(path) = current_path() else {
        return; // disabled
    };

    // Single-writer through the global mutex; `LAST_HASH` is only touched here,
    // so the same lock serializes the chain-tip update. Open-append-write-close
    // per event keeps state simple and OS-level append-mode handles concurrent
    // process safety if it ever matters.
    let _guard = match AUDIT_LOCK.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(), // poisoned mutex: still write, audit is best-effort
    };
    let mut last = LAST_HASH.lock().unwrap_or_else(|p| p.into_inner());
    if !LAST_HASH_SEEDED.swap(true, Ordering::SeqCst) {
        *last = seed_last_hash(path);
    }
    // Chain this entry to the previous line (tamper-evidence, v2.4).
    event.prev_hash = last.clone();

    let line = match serde_json::to_string(&event) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("audit: failed to serialize event: {e}");
            return; // chain tip unchanged — nothing was written
        }
    };
    let mut file = match OpenOptions::new().create(true).append(true).open(path) {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!("audit: failed to open {}: {e}", path.display());
            return;
        }
    };
    if let Err(e) = writeln!(file, "{line}") {
        tracing::warn!("audit: failed to write line: {e}");
        return; // do NOT advance the chain tip on a failed write
    }
    *last = Some(hash_line(&line));
}

/// Result of `audit verify`: hash-chain integrity over the log file.
#[derive(Debug)]
pub struct AuditVerifyReport {
    /// Total lines in the log.
    pub total: usize,
    /// Lines that carry a `prev_hash` (the chained suffix — legacy lines don't).
    pub chained: usize,
    /// 1-based line number of the first chain break, or None if intact.
    pub broken_at: Option<usize>,
}

/// Verify the hash-chain of the configured audit log.
pub fn verify() -> Result<AuditVerifyReport> {
    let path = current_path().ok_or_else(|| anyhow::anyhow!("audit logging is disabled"))?;
    verify_path(path)
}

/// Verify a specific audit file's chain (pure over the file; used by tests).
/// Every entry that carries `prev_hash` must equal the BLAKE3 of the previous
/// line's exact bytes; entries without it (the legacy prefix) are counted but
/// not checked. Reports the 1-based line number of the first break.
pub fn verify_path(path: &Path) -> Result<AuditVerifyReport> {
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = content.lines().collect();
    let mut chained = 0;
    for i in 1..lines.len() {
        let ev: AuditEvent = match serde_json::from_str(lines[i]) {
            Ok(e) => e,
            Err(_) => continue, // unparseable line — can't check its own link
        };
        if let Some(ph) = ev.prev_hash.as_deref() {
            chained += 1;
            if ph != hash_line(lines[i - 1]) {
                return Ok(AuditVerifyReport {
                    total: lines.len(),
                    chained,
                    broken_at: Some(i + 1),
                });
            }
        }
    }
    Ok(AuditVerifyReport {
        total: lines.len(),
        chained,
        broken_at: None,
    })
}

/// Map an audit event to a live graph pulse. Writes (Add/FactAdd/...) are
/// "write" impulses toward the affected core; quarantine/consolidate-style ops
/// are "process". Reads are emitted separately at the read sites, not here.
fn emit_pulse(event: &AuditEvent) {
    use crate::pulse::{PulseEvent, PulseKind};
    let (kind, target) = match event.op {
        // New cores written.
        AuditOp::Add | AuditOp::Ingest => {
            let t = if !event.target.is_empty() {
                format!("mem:{}", event.target)
            } else {
                format!("lib:{}", event.library)
            };
            (PulseKind::Write, t)
        }
        AuditOp::FactAdd => (PulseKind::Write, "fact".to_string()),
        AuditOp::ProcedureAdd => (PulseKind::Write, format!("mem:{}", event.target)),
        AuditOp::LibraryCreate | AuditOp::LibraryDrop => {
            (PulseKind::Write, format!("lib:{}", event.library))
        }
        // Internal processing — duel/quarantine/consolidate/retest/outcome,
        // and the write-path drops (near-dup skip, quarantine, secret-skip).
        AuditOp::Update
        | AuditOp::Delete
        | AuditOp::FactInvalidate
        | AuditOp::ProcedureOutcome
        | AuditOp::Consolidate
        | AuditOp::RetestPromote
        | AuditOp::RetestRecover
        | AuditOp::SkipDup
        | AuditOp::Quarantine
        | AuditOp::Archive
        | AuditOp::Restore
        | AuditOp::SkipSecret => {
            let t = if !event.target.is_empty() {
                format!("mem:{}", event.target)
            } else {
                format!("lib:{}", event.library)
            };
            (PulseKind::Process, t)
        }
    };
    let label = format!("{:?}", event.op).to_lowercase();
    crate::pulse::emit(PulseEvent::new(kind, target, label).actor(Some(event.actor.clone())));
}

/// Read all events from the log, oldest first. Skips any trailing line that
/// doesn't parse — typical NDJSON tail-on-crash robustness. Used by the
/// `mgimind audit show` command and the upcoming viewer.
pub fn load_all() -> Result<Vec<AuditEvent>> {
    let Some(path) = current_path() else {
        return Ok(Vec::new());
    };
    if !path.exists() {
        return Ok(Vec::new());
    }
    let f = File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
    let reader = BufReader::new(f);
    let mut out = Vec::new();
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<AuditEvent>(&line) {
            Ok(ev) => out.push(ev),
            Err(_) => {
                // Tail line might be torn; skip rather than fail the whole read.
                continue;
            }
        }
    }
    Ok(out)
}

/// Read events whose target matches `id`. Used by `mgimind audit show <id>`.
pub fn for_target(id: &str) -> Result<Vec<AuditEvent>> {
    Ok(load_all()?.into_iter().filter(|e| e.target == id).collect())
}

/// Read most recent N events across the whole log. Used by `mgimind audit list`.
pub fn recent(n: usize) -> Result<Vec<AuditEvent>> {
    let mut all = load_all()?;
    let len = all.len();
    if len > n {
        all.drain(0..(len - n));
    }
    Ok(all)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Each test isolates its own audit path via `init` with a temp file. The
    /// `OnceCell` is process-wide, so we can't actually re-init within a single
    /// test process — instead we drive the underlying functions through a
    /// per-test path constructed by hand. The public API still goes through
    /// `init` so production code stays simple.
    fn write_event(path: &PathBuf, event: &AuditEvent) {
        let line = serde_json::to_string(event).unwrap();
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(file, "{line}").unwrap();
    }

    #[test]
    fn hash_chain_verifies_and_detects_tampering() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("audit.log");

        // Build a 3-line chain by hand the way `record` does: each line's
        // prev_hash = BLAKE3 of the previous line's exact bytes.
        let e0 = AuditEvent::new(AuditOp::Add, "lib", "a");
        let l0 = serde_json::to_string(&e0).unwrap();
        let mut e1 = AuditEvent::new(AuditOp::Add, "lib", "b");
        e1.prev_hash = Some(hash_line(&l0));
        let l1 = serde_json::to_string(&e1).unwrap();
        let mut e2 = AuditEvent::new(AuditOp::Delete, "lib", "b");
        e2.prev_hash = Some(hash_line(&l1));
        let l2 = serde_json::to_string(&e2).unwrap();
        std::fs::write(&path, format!("{l0}\n{l1}\n{l2}\n")).unwrap();

        let report = verify_path(&path).unwrap();
        assert!(
            report.broken_at.is_none(),
            "intact chain must verify, got {report:?}"
        );
        assert_eq!(report.chained, 2, "two chained entries (l1, l2)");

        // Tamper l1's bytes → l2.prev_hash (hash of the ORIGINAL l1) no longer
        // matches → the break surfaces at l2 (line 3), not at the edited line.
        let mut e1_evil = AuditEvent::new(AuditOp::Add, "lib", "EVIL");
        e1_evil.prev_hash = Some(hash_line(&l0));
        let l1_bad = serde_json::to_string(&e1_evil).unwrap();
        std::fs::write(&path, format!("{l0}\n{l1_bad}\n{l2}\n")).unwrap();

        let report = verify_path(&path).unwrap();
        assert!(
            report.broken_at.is_some(),
            "tampered chain must fail, got {report:?}"
        );
        assert_eq!(report.broken_at, Some(3), "break detected at line 3 (l2)");
    }

    fn read_events(path: &PathBuf) -> Vec<AuditEvent> {
        let f = File::open(path).unwrap();
        let reader = BufReader::new(f);
        let mut out = Vec::new();
        for line in reader.lines().map_while(|r| r.ok()) {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(ev) = serde_json::from_str::<AuditEvent>(&line) {
                out.push(ev);
            }
        }
        out
    }

    #[test]
    fn event_builder_chains() {
        let ev = AuditEvent::new(AuditOp::Update, "lib", "id-x")
            .actor("mcp")
            .before("old")
            .after("new")
            .note("changed by user");
        assert_eq!(ev.library, "lib");
        assert_eq!(ev.target, "id-x");
        assert_eq!(ev.actor, "mcp");
        assert_eq!(ev.before.as_deref(), Some("old"));
        assert_eq!(ev.after.as_deref(), Some("new"));
        assert_eq!(ev.note.as_deref(), Some("changed by user"));
    }

    #[test]
    fn ndjson_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("audit.log");
        let ev1 = AuditEvent::new(AuditOp::Add, "projects", "id-1").after("hello");
        let ev2 = AuditEvent::new(AuditOp::Delete, "projects", "id-1").before("hello");
        write_event(&path, &ev1);
        write_event(&path, &ev2);
        let read = read_events(&path);
        assert_eq!(read.len(), 2);
        assert_eq!(read[0].op, AuditOp::Add);
        assert_eq!(read[1].op, AuditOp::Delete);
    }

    #[test]
    fn torn_tail_line_is_skipped() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("audit.log");
        let ev = AuditEvent::new(AuditOp::Add, "p", "id-1").after("a");
        write_event(&path, &ev);
        // Simulate a crash mid-write: append a half-line without newline closing
        // and without valid JSON.
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"{\"ts\":\"2026-01-01T00:00:00Z\",\"op\"")
            .unwrap();
        // The reader should return only the first valid event.
        let read = read_events(&path);
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].target, "id-1");
    }

    #[test]
    fn skips_blank_lines() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("audit.log");
        let ev = AuditEvent::new(AuditOp::Add, "p", "id-1");
        write_event(&path, &ev);
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"\n\n").unwrap();
        let ev2 = AuditEvent::new(AuditOp::Delete, "p", "id-1");
        write_event(&path, &ev2);
        let read = read_events(&path);
        assert_eq!(read.len(), 2);
    }

    #[test]
    fn serialization_omits_empty_optional_fields() {
        let ev = AuditEvent::new(AuditOp::LibraryCreate, "", "newlib");
        let json = serde_json::to_string(&ev).unwrap();
        // before / after / note should be omitted, library is empty string
        // (kept — empty library is meaningful for library-level ops).
        assert!(!json.contains("before"));
        assert!(!json.contains("after"));
        assert!(!json.contains("note"));
    }

    #[test]
    fn write_path_ops_have_stable_snake_case_tags() {
        // The `audit writes` tally and the `--op` filter key on these exact
        // strings; a rename would silently break the "where did writes go" tool.
        let tag = |op: AuditOp| {
            serde_json::to_string(&op)
                .unwrap()
                .trim_matches('"')
                .to_string()
        };
        assert_eq!(tag(AuditOp::Ingest), "ingest");
        assert_eq!(tag(AuditOp::SkipDup), "skip_dup");
        assert_eq!(tag(AuditOp::Quarantine), "quarantine");
        assert_eq!(tag(AuditOp::SkipSecret), "skip_secret");
    }

    #[test]
    fn skip_secret_event_never_carries_content() {
        // Defense-in-depth: a secret-skip audit event must not put content in
        // `after`/`before` — only the static detector label belongs in `note`.
        let ev = AuditEvent::new(AuditOp::SkipSecret, "lib", "")
            .actor("ingest")
            .note("secret-skipped (GitHub token)");
        let json = serde_json::to_string(&ev).unwrap();
        assert!(!json.contains("\"after\""));
        assert!(!json.contains("\"before\""));
        assert!(json.contains("secret-skipped"));
    }
}
