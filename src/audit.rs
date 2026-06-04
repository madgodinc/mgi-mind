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
use std::path::PathBuf;
use std::sync::Mutex;

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
pub fn record(event: AuditEvent) {
    let Some(path) = current_path() else {
        return; // disabled
    };
    let line = match serde_json::to_string(&event) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("audit: failed to serialize event: {e}");
            return;
        }
    };

    // Single-writer through the global mutex. Open-append-write-close per
    // event keeps state simple (no persistent file handle to deal with on
    // shutdown/test-isolation) and OS-level append-mode handles concurrent
    // process safety if it ever matters.
    let _guard = match AUDIT_LOCK.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(), // poisoned mutex: still write, audit is best-effort
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
    }
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
}
