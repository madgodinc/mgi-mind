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
    /// v2.7: a memory was moved to a different library by `mgimind relibrary`.
    /// `target` = the NEW point id (library-addressed ids change on a move);
    /// `note` carries `"moved from <old_library>:<old_id>"`. The old id's own
    /// prior history stays under the old id — the hash chain cannot be
    /// rewritten, so `audit show <new_id>` starts fresh from this event.
    Relibrary,
    /// v2.7.1: an operator acknowledged a historical, pre-existing chain break
    /// found by `verify()` — `mgimind audit reanchor --reason ...`. Does NOT
    /// touch the broken line or anything before it; it only appends a new,
    /// normally-chained event that carries `ack_breaks` (the acknowledged break
    /// line numbers) in `note`'s companion field. After this event, `verify()`
    /// stops reporting those line numbers as unacknowledged — the segment
    /// starting here is held to full tamper-evidence as normal, while the
    /// documented gap before it is no longer mistaken for new tampering.
    Reanchor,
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
    /// v2.7.1, `Reanchor` events only: the 1-based line numbers of chain breaks
    /// this event acknowledges. `verify()` unions this across every `Reanchor`
    /// line in the log and treats any break whose line number appears here as
    /// explained, not as a tamper signal. `None` for every other op.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ack_breaks: Option<Vec<usize>>,
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
            ack_breaks: None,
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

/// BLAKE3 hex of a log line's exact bytes (the string as written, no newline).
fn hash_line(line: &str) -> String {
    blake3::hash(line.as_bytes()).to_hex().to_string()
}

/// Hash of the last non-empty line already in `path`, or None if empty/absent.
/// Always re-read from disk at write time (see `record`) rather than cached —
/// a cache is exactly what broke the chain in production: a long-running MCP
/// server, a `serve-http` process and one-shot CLI invocations each append to
/// the same file, and a process-lifetime "last hash" goes stale the moment a
/// DIFFERENT process appends. Re-reading the tail is the only way one process
/// can know what another just wrote.
fn seed_last_hash(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    content
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(hash_line)
}

/// Sibling lock file for the cross-process critical section (`<audit log>.lock`,
/// e.g. `audit.log.lock`). A dedicated zero-byte file rather than locking
/// `audit.log` itself, so a plain `tail -f audit.log` from another tool never
/// contends with it.
fn lock_file_path(path: &Path) -> PathBuf {
    let mut os = path.as_os_str().to_os_string();
    os.push(".lock");
    PathBuf::from(os)
}

/// Append one event to `path`: read the real chain tip fresh and write the
/// new line, under a cross-process lock so no two writers can race the tip.
/// Pure over a path (no global state) so tests can drive it directly; `record`
/// and `reanchor` are thin wrappers that resolve the configured path and
/// decide how to handle an error.
///
/// `AUDIT_LOCK` (a process-local mutex, taken by the caller) only serializes
/// writers WITHIN this process. That is not enough on its own: mgi-mind runs
/// as several separate processes against the same MGIMIND_HOME at once (a
/// long-running MCP server per connected agent, `serve-http`, one-shot CLI
/// calls). Before v2.7.1 the chain tip was cached in a process-lifetime
/// static, seeded once from the file tail; two processes racing an append
/// each wrote a `prev_hash` pointing at a line the OTHER process had since
/// superseded, and the chain broke at whichever line lost the race — this is
/// the root cause of the production break (pre-existing, same in a backup
/// predating this fix). The fix is the cross-process advisory lock below
/// around the read-tail + append, with NO in-memory cache: every writer
/// re-reads the real tip fresh while holding the lock, so there is nothing
/// left to go stale.
fn append_event(path: &Path, mut event: AuditEvent) -> Result<()> {
    let lock_path = lock_file_path(path);
    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("opening lock file {}", lock_path.display()))?;
    fs2::FileExt::lock_exclusive(&lock_file).context("acquiring cross-process audit lock")?;
    // `lock_file` is dropped (and the OS releases the flock) at the end of
    // this function either way — a crash mid-write can't wedge it.

    event.prev_hash = seed_last_hash(path);
    let line = serde_json::to_string(&event).context("serializing audit event")?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    writeln!(file, "{line}").context("writing audit line")?;
    Ok(())
}

/// Record an event. Never panics, never fails the caller. A logging failure
/// is itself logged via `tracing::warn` but does not propagate up — the mutate
/// operation has already succeeded by the time we're here, and refusing to
/// return success because we couldn't write a log line would be the wrong
/// tradeoff.
pub fn record(event: AuditEvent) {
    // Emit a live pulse for the viewer's graph, independent of whether the
    // audit FILE is enabled — the visual feed should pulse even on a system
    // that has audit logging turned off.
    emit_pulse(&event);

    let Some(path) = current_path() else {
        return; // disabled
    };
    let _guard = match AUDIT_LOCK.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(), // poisoned mutex: still write, audit is best-effort
    };
    if let Err(e) = append_event(path, event) {
        tracing::warn!("audit: failed to record event: {e:#}");
    }
}

/// Result of `audit verify`: hash-chain integrity over the log file.
///
/// A log can carry more than one kind of gap, and this struct keeps them
/// distinct rather than collapsing to a single pass/fail bit:
/// - lines before the chain existed at all (no `prev_hash` field, not counted
///   in `breaks` — there is nothing to check);
/// - a genuine break, where `prev_hash` is present but does not match the hash
///   of the line before it;
/// - a break an operator has since acknowledged with `mgimind audit reanchor`,
///   after confirming the cause (e.g. the pre-v2.7.1 concurrent-writer race).
///
/// `verify` only fails on the third category being NON-empty of the first —
/// i.e. on `unacknowledged`.
#[derive(Debug)]
pub struct AuditVerifyReport {
    /// Total lines in the log.
    pub total: usize,
    /// Lines that carry a `prev_hash` (the chained suffix — legacy lines don't).
    /// This is NOT "lines verified so far before giving up" — every chained
    /// line in the whole file is checked, even past a break, so a log can
    /// report several distinct breaks in one `verify` call.
    pub chained: usize,
    /// 1-based line numbers of every chain break found, in file order.
    pub breaks: Vec<usize>,
    /// Subset of `breaks` covered by at least one `Reanchor` event's
    /// `ack_breaks` anywhere in the log.
    pub acknowledged: Vec<usize>,
    /// `breaks` minus `acknowledged` — what still needs an operator's
    /// attention. Empty means the chain is either fully intact or every break
    /// in it has been explained.
    pub unacknowledged: Vec<usize>,
    /// First entry of `unacknowledged`, or None. Kept for callers that only
    /// want a single line number (e.g. the CLI's headline message).
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
/// not checked. Scans the WHOLE file rather than stopping at the first break,
/// so a log with several distinct breaks reports all of them, and collects
/// every `Reanchor` event's `ack_breaks` to tell an explained break apart from
/// an unexplained one.
pub fn verify_path(path: &Path) -> Result<AuditVerifyReport> {
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = content.lines().collect();
    let mut chained = 0;
    let mut breaks = Vec::new();
    let mut acknowledged_set = std::collections::BTreeSet::new();

    // Line 0 can't be checked against a predecessor, but it can still be a
    // Reanchor event (a fresh log re-anchored before anything else was ever
    // written to it is a degenerate but valid case), so parse it for ack_breaks.
    if let Some(acked) = lines
        .first()
        .and_then(|l| serde_json::from_str::<AuditEvent>(l).ok())
        .and_then(|ev0| ev0.ack_breaks)
    {
        acknowledged_set.extend(acked);
    }

    for i in 1..lines.len() {
        let ev: AuditEvent = match serde_json::from_str(lines[i]) {
            Ok(e) => e,
            Err(_) => continue, // unparseable line — can't check its own link
        };
        if let Some(acked) = ev.ack_breaks {
            acknowledged_set.extend(acked);
        }
        if let Some(ph) = ev.prev_hash.as_deref() {
            chained += 1;
            if ph != hash_line(lines[i - 1]) {
                breaks.push(i + 1); // 1-based line number of the offending line
            }
        }
    }

    let unacknowledged: Vec<usize> = breaks
        .iter()
        .copied()
        .filter(|b| !acknowledged_set.contains(b))
        .collect();
    let broken_at = unacknowledged.first().copied();
    let acknowledged: Vec<usize> = breaks
        .iter()
        .copied()
        .filter(|b| acknowledged_set.contains(b))
        .collect();

    Ok(AuditVerifyReport {
        total: lines.len(),
        chained,
        breaks,
        acknowledged,
        unacknowledged,
        broken_at,
    })
}

/// Acknowledge every currently-unacknowledged chain break in `path` by
/// appending a `Reanchor` event. Does not touch the broken line or anything
/// before it: history is never rewritten. The new event chains normally from
/// the current tip and carries the acknowledged break line numbers, so
/// `verify()` stops flagging them from here on while everything from this
/// event forward is held to full tamper-evidence same as always.
///
/// Refuses when there is nothing unacknowledged, so it can't be run as a
/// reflex — reanchoring is a deliberate record that a specific, investigated
/// gap is explained, not a routine step. Pure over a path so tests can drive
/// it directly; `reanchor` is the thin wrapper over the configured log.
pub fn reanchor_path(
    path: &Path,
    reason: impl Into<String>,
    actor: impl Into<String>,
) -> Result<AuditEvent> {
    let report = verify_path(path)?;
    if report.unacknowledged.is_empty() {
        anyhow::bail!("audit chain has no unacknowledged break; nothing to reanchor");
    }
    let mut event = AuditEvent::new(AuditOp::Reanchor, "", "audit-chain")
        .actor(actor)
        .note(reason.into());
    event.ack_breaks = Some(report.unacknowledged.clone());
    append_event(path, event.clone())?;
    Ok(event)
}

/// Acknowledge every currently-unacknowledged break in the configured audit
/// log — `mgimind audit reanchor --reason ...`. See `reanchor_path`.
pub fn reanchor(reason: impl Into<String>, actor: impl Into<String>) -> Result<AuditEvent> {
    let path = current_path().ok_or_else(|| anyhow::anyhow!("audit logging is disabled"))?;
    let _guard = match AUDIT_LOCK.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    reanchor_path(path, reason, actor)
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
        | AuditOp::Relibrary
        | AuditOp::Reanchor
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

    /// Two concurrent "processes" (here: threads, each with its own open file
    /// handle and NO shared in-memory state, mirroring separate OS processes)
    /// append through `append_event` at once. Before the cross-process lock
    /// this raced exactly like the production break: both read the same stale
    /// tail, both wrote a `prev_hash` pointing at it, and the loser's line
    /// broke the chain. With the lock, every append reads the tail fresh under
    /// mutual exclusion, so the result is a strictly valid chain regardless of
    /// interleaving.
    #[test]
    fn append_event_is_cross_process_safe_under_concurrency() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("audit.log");
        std::fs::write(&path, "").unwrap();

        let mut handles = Vec::new();
        for n in 0..8 {
            let path = path.clone();
            handles.push(std::thread::spawn(move || {
                for i in 0..10 {
                    let ev = AuditEvent::new(AuditOp::Add, "lib", format!("t{n}-{i}"));
                    append_event(&path, ev).unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let report = verify_path(&path).unwrap();
        assert_eq!(report.total, 80, "all 80 concurrent appends landed");
        assert!(
            report.breaks.is_empty(),
            "no break under concurrent writers once the tip is always read fresh under \
             the lock, got {report:?}"
        );
    }

    #[test]
    fn verify_reports_every_break_not_just_the_first() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("audit.log");

        // Three independent chains concatenated, as a migration/log-merge
        // would produce: each internally consistent, but line 2 and line 4
        // (1-based) don't chain to what precedes them.
        let a0 = AuditEvent::new(AuditOp::Add, "lib", "a0");
        let l_a0 = serde_json::to_string(&a0).unwrap();
        let b0 = AuditEvent::new(AuditOp::Add, "lib", "b0"); // prev_hash: None — a fresh segment
        let l_b0 = serde_json::to_string(&b0).unwrap();
        let mut b1 = AuditEvent::new(AuditOp::Add, "lib", "b1");
        b1.prev_hash = Some(hash_line("something that was never actually written"));
        let l_b1 = serde_json::to_string(&b1).unwrap();
        std::fs::write(&path, format!("{l_a0}\n{l_b0}\n{l_b1}\n")).unwrap();

        let report = verify_path(&path).unwrap();
        // l_b0 carries no prev_hash (not "chained"), so only l_b1 is a checked,
        // broken link in this file.
        assert_eq!(report.breaks, vec![3]);
        assert_eq!(report.unacknowledged, vec![3]);
        assert_eq!(report.broken_at, Some(3));
    }

    #[test]
    fn reanchor_acknowledges_the_break_and_verify_then_passes() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("audit.log");

        // A two-line log with a broken second line (simulates the production
        // break: `prev_hash` present but pointing at the wrong predecessor).
        let e0 = AuditEvent::new(AuditOp::Add, "lib", "a");
        let l0 = serde_json::to_string(&e0).unwrap();
        let mut e1 = AuditEvent::new(AuditOp::Add, "lib", "b");
        e1.prev_hash = Some(hash_line("not the real previous line"));
        let l1 = serde_json::to_string(&e1).unwrap();
        std::fs::write(&path, format!("{l0}\n{l1}\n")).unwrap();

        let before = verify_path(&path).unwrap();
        assert_eq!(before.unacknowledged, vec![2]);

        let reanchor_event =
            reanchor_path(&path, "root cause: migration concatenated two logs", "test")
                .expect("reanchor must succeed when there is an unacknowledged break");
        assert_eq!(reanchor_event.op, AuditOp::Reanchor);
        assert_eq!(reanchor_event.ack_breaks, Some(vec![2]));

        let after = verify_path(&path).unwrap();
        assert!(
            after.unacknowledged.is_empty(),
            "the acknowledged break must no longer be reported, got {after:?}"
        );
        assert_eq!(after.breaks, vec![2], "the raw break is still on record");
        assert_eq!(after.acknowledged, vec![2]);

        // Re-running with nothing left unacknowledged is refused, not a silent
        // no-op — reanchor is a deliberate action, never a reflex.
        let again = reanchor_path(&path, "second attempt", "test");
        assert!(again.is_err());
    }

    #[test]
    fn reanchor_event_itself_chains_and_appends_without_rewriting_history() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("audit.log");
        let e0 = AuditEvent::new(AuditOp::Add, "lib", "a");
        let l0 = serde_json::to_string(&e0).unwrap();
        let mut e1 = AuditEvent::new(AuditOp::Add, "lib", "b");
        e1.prev_hash = Some(hash_line("wrong"));
        let l1 = serde_json::to_string(&e1).unwrap();
        std::fs::write(&path, format!("{l0}\n{l1}\n")).unwrap();

        reanchor_path(&path, "explained break", "test").unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(
            lines.len(),
            3,
            "reanchor only APPENDS, never removes a line"
        );
        assert_eq!(lines[0], l0, "original first line untouched");
        assert_eq!(
            lines[1], l1,
            "the broken line itself is left exactly as it was"
        );

        // The new third line chains correctly to the (still broken) second
        // line — the new segment starting here is fully verifiable going
        // forward even though it follows a historical gap.
        let ev2: AuditEvent = serde_json::from_str(lines[2]).unwrap();
        assert_eq!(ev2.op, AuditOp::Reanchor);
        assert_eq!(ev2.prev_hash.as_deref(), Some(hash_line(lines[1]).as_str()));
    }
}
