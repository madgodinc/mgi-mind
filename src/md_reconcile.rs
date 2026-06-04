#![allow(dead_code)]
// print_plan is the debug helper for `mgimind md-reconcile --plan-only`.
// Public for downstream tooling; production CLI calls a different exporter.

//! Markdown reconcile import — the escape hatch for hand-editing memories.
//!
//! mgi-mind is automated memory. Qdrant is the source of truth. The store
//! decides what gets written, when, and how — agent-driven through
//! `mind_ingest`, system-driven through hooks, all of it tracked in the audit
//! log. md import exists for one narrow case: the user opens a memory file in
//! their editor and edits it because the automatic write got something wrong.
//!
//! That makes md import structurally different from a sync. The two sides are
//! NOT equal peers — Qdrant is the live store, the file on disk is a manual
//! correction the user wrote knowing what they wanted to change. So the rule is
//! simple: when md import runs, **md wins for the rows it touches**. No
//! last-write-by-timestamp guessing. The user touched the file in crisis-mode
//! exactly to override whatever the system wrote; a timestamp-based merge would
//! pick the system's recent write and silently revert the user's correction.
//!
//! Identity is by `source` tag, not by content hash: changing one word in a
//! file would otherwise look like a brand-new memory (different UUIDv5) with
//! the old version still in place — a duplicate, not a fix. We find existing
//! points by `source = <filename>` in the target library, remove them, and
//! write the new content under the same source. The audit log carries the
//! before/after so the trail of what was overwritten survives.
//!
//! Default mode is dry-run: print the plan, do nothing. `--apply` mutates.
//! This matches every other write surface that takes a real action only on
//! explicit opt-in (consolidate, vault delete, drop --apply).

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::audit;
use crate::config::MindConfig;
use crate::storage;

/// One file's reconcile plan: what's on disk, what's in the store under the
/// same source, and how they relate.
#[derive(Debug)]
pub struct FilePlan {
    pub source: String,
    pub new_content: String,
    pub existing: Vec<storage::MemoryRecord>,
    pub action: PlanAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanAction {
    /// First-time write — no existing points for this source. md goes in clean.
    New,
    /// Identical content already stored. Skip (md and store agree on this row).
    Unchanged,
    /// Existing content differs from md. md will replace the existing points.
    /// THIS is where "md wins" applies.
    Replace,
    /// File is empty or too short — skip the whole entry.
    Skip,
}

impl PlanAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            PlanAction::New => "new",
            PlanAction::Unchanged => "unchanged",
            PlanAction::Replace => "replace",
            PlanAction::Skip => "skip",
        }
    }
}

/// Top-level reconcile plan across all files under a directory.
#[derive(Debug, Default)]
pub struct ReconcilePlan {
    pub library: String,
    pub root: PathBuf,
    pub files: Vec<FilePlan>,
}

impl ReconcilePlan {
    pub fn counts(&self) -> PlanCounts {
        let mut c = PlanCounts::default();
        for f in &self.files {
            match f.action {
                PlanAction::New => c.new += 1,
                PlanAction::Unchanged => c.unchanged += 1,
                PlanAction::Replace => c.replace += 1,
                PlanAction::Skip => c.skip += 1,
            }
        }
        c
    }
}

#[derive(Debug, Default)]
pub struct PlanCounts {
    pub new: usize,
    pub unchanged: usize,
    pub replace: usize,
    pub skip: usize,
}

/// Build the plan: scan the directory, fetch existing points per source,
/// decide per-file action. Does NOT mutate anything — that's `apply`.
pub async fn plan(config: &MindConfig, library: &str, root: &Path) -> Result<ReconcilePlan> {
    if !root.exists() || !root.is_dir() {
        anyhow::bail!("Directory not found: {}", root.display());
    }
    let mut paths: Vec<PathBuf> = Vec::new();
    scan_md(root, &mut paths)?;

    let mut plan = ReconcilePlan {
        library: library.into(),
        root: root.to_path_buf(),
        ..Default::default()
    };

    for path in paths {
        let source = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let new_content = match std::fs::read_to_string(&path) {
            Ok(s) => s.trim().to_string(),
            Err(_) => {
                plan.files.push(FilePlan {
                    source,
                    new_content: String::new(),
                    existing: Vec::new(),
                    action: PlanAction::Skip,
                });
                continue;
            }
        };
        if new_content.chars().count() < 10 {
            plan.files.push(FilePlan {
                source,
                new_content,
                existing: Vec::new(),
                action: PlanAction::Skip,
            });
            continue;
        }

        let existing = storage::find_by_source(config, library, &source).await?;
        let action = decide_action(&new_content, &existing);

        plan.files.push(FilePlan {
            source,
            new_content,
            existing,
            action,
        });
    }
    Ok(plan)
}

/// Apply the plan. For each `Replace`, delete old points and write the new
/// content under the same source. For `New`, just write. `Unchanged`/`Skip`
/// are no-ops. Every mutation goes through `storage::add_memory` and
/// `storage::delete_memory`, so the regular audit log captures everything; we
/// also write one summary audit entry tagged actor=md-import for the whole
/// reconcile so the trail is easy to find.
pub async fn apply(config: &MindConfig, plan: &ReconcilePlan) -> Result<ApplyReport> {
    let mut report = ApplyReport::default();

    for f in &plan.files {
        match f.action {
            PlanAction::Unchanged | PlanAction::Skip => {
                continue;
            }
            PlanAction::Replace => {
                // Delete existing points first so the new write doesn't pile up
                // duplicates if any chunking changed.
                for old in &f.existing {
                    storage::delete_memory(config, &plan.library, &old.id)
                        .await
                        .with_context(|| format!("Failed to delete old point for {}", f.source))?;
                }
                let n = storage::add_memory(config, &plan.library, &f.new_content, Some(&f.source))
                    .await
                    .with_context(|| format!("Failed to write new content for {}", f.source))?;
                report.replaced += 1;
                report.chunks_written += n;
                audit::record(
                    audit::AuditEvent::new(audit::AuditOp::Update, &plan.library, &f.source)
                        .actor("md-import")
                        .note(format!(
                            "md reconcile: replaced {} existing point(s) with {n} chunk(s)",
                            f.existing.len()
                        )),
                );
            }
            PlanAction::New => {
                let n = storage::add_memory(config, &plan.library, &f.new_content, Some(&f.source))
                    .await
                    .with_context(|| format!("Failed to write new content for {}", f.source))?;
                report.added += 1;
                report.chunks_written += n;
                audit::record(
                    audit::AuditEvent::new(audit::AuditOp::Add, &plan.library, &f.source)
                        .actor("md-import")
                        .note(format!("md reconcile: new file ({n} chunks)")),
                );
            }
        }
    }
    Ok(report)
}

#[derive(Debug, Default)]
pub struct ApplyReport {
    pub added: usize,
    pub replaced: usize,
    pub chunks_written: usize,
}

/// Decide what to do with one file given what's already stored under its source.
///
/// - empty existing → `New`
/// - any existing point has identical content → `Unchanged` (md and store agree)
/// - otherwise → `Replace` (md wins)
fn decide_action(new_content: &str, existing: &[storage::MemoryRecord]) -> PlanAction {
    if existing.is_empty() {
        return PlanAction::New;
    }
    // If a single point already holds the exact same content, we're idempotent
    // — same identity, same body. Don't churn the store for a no-op import.
    if existing.len() == 1 && existing[0].content.trim() == new_content.trim() {
        return PlanAction::Unchanged;
    }
    // Anything else (different content, or multiple points where chunking
    // produced more than one row) is a Replace under "md wins".
    PlanAction::Replace
}

fn scan_md(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if !name.starts_with('.') {
                scan_md(&path, out)?;
            }
        } else if path.extension().is_some_and(|ext| ext == "md") {
            out.push(path);
        }
    }
    Ok(())
}

/// Render a plan as a string. The intent is for the user to actually read
/// this before passing `--apply` — so the format leads with the direction of
/// change ("Qdrant now → md says ..."), not an abstract old-vs-new diff.
/// "md wins" is the rule, so the diff is asymmetric on purpose.
pub fn render_plan(plan: &ReconcilePlan) -> String {
    use std::fmt::Write;
    let c = plan.counts();
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Reconcile plan for library '{}' from {}:",
        plan.library,
        plan.root.display()
    );
    let _ = writeln!(
        out,
        "  files scanned: {}  new: {}  replace: {}  unchanged: {}  skip: {}",
        plan.files.len(),
        c.new,
        c.replace,
        c.unchanged,
        c.skip,
    );
    let _ = writeln!(out);
    for f in &plan.files {
        if f.action == PlanAction::Skip || f.action == PlanAction::Unchanged {
            continue;
        }
        let _ = writeln!(out, "[{}] {}", f.action.as_str(), f.source);
        if f.action == PlanAction::Replace {
            for (i, old) in f.existing.iter().enumerate() {
                let preview = first_line(&old.content);
                let _ = writeln!(out, "   Qdrant now (#{}): {preview}", i + 1);
            }
            let preview = first_line(&f.new_content);
            let _ = writeln!(out, "   will become (md): {preview}");
            let _ = writeln!(out);
        } else if f.action == PlanAction::New {
            let preview = first_line(&f.new_content);
            let _ = writeln!(out, "   will become (md): {preview}");
            let _ = writeln!(out);
        }
    }
    if c.new + c.replace == 0 {
        let _ = writeln!(out, "Nothing to apply — md and Qdrant agree on every file.");
    } else {
        let _ = writeln!(
            out,
            "Run with --apply to write {} new and replace {} existing entr{}.",
            c.new,
            c.replace,
            if c.replace == 1 { "y" } else { "ies" }
        );
    }
    out
}

/// Backwards-compatible printer; just dumps `render_plan` to stdout.
pub fn print_plan(plan: &ReconcilePlan) {
    print!("{}", render_plan(plan));
}

fn first_line(s: &str) -> String {
    const MAX: usize = 100;
    let line = s.lines().next().unwrap_or("").trim().to_string();
    if line.chars().count() <= MAX {
        line
    } else {
        let mut out: String = line.chars().take(MAX).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(content: &str) -> storage::MemoryRecord {
        storage::MemoryRecord {
            id: "id-1".into(),
            content: content.into(),
            source: Some("file.md".into()),
            r#type: "memory".into(),
            created_at: String::new(),
            updated_at: String::new(),
            // Reconcile tests predate v1.4 and don't care about scores.
            confidence_score: None,
        }
    }

    #[test]
    fn no_existing_is_new() {
        assert_eq!(decide_action("hello", &[]), PlanAction::New);
    }

    #[test]
    fn single_identical_is_unchanged() {
        let existing = vec![rec("hello world")];
        assert_eq!(
            decide_action("hello world", &existing),
            PlanAction::Unchanged
        );
    }

    #[test]
    fn single_different_is_replace() {
        let existing = vec![rec("hello world")];
        assert_eq!(decide_action("hello rust", &existing), PlanAction::Replace);
    }

    #[test]
    fn whitespace_only_changes_dont_trigger_replace() {
        let existing = vec![rec("  hello  ")];
        assert_eq!(decide_action("hello", &existing), PlanAction::Unchanged);
    }

    #[test]
    fn multi_chunk_existing_is_always_replace() {
        // even if one chunk happens to match, we treat the set as "may have
        // drifted" and let md win cleanly.
        let existing = vec![rec("chunk one"), rec("chunk two")];
        assert_eq!(decide_action("chunk one", &existing), PlanAction::Replace);
    }

    #[test]
    fn render_plan_shows_asymmetric_direction_on_replace() {
        // Regression guard for the v1.0 contract: dry-run output MUST lead
        // with "Qdrant now → will become (md)" so the user reading the diff
        // sees the direction of change before deciding to --apply.
        let plan = ReconcilePlan {
            library: "notes".into(),
            root: PathBuf::from("/tmp/notes"),
            files: vec![FilePlan {
                source: "alpha.md".into(),
                new_content: "new body that wins".into(),
                existing: vec![rec("old body in store")],
                action: PlanAction::Replace,
            }],
        };
        let rendered = render_plan(&plan);
        assert!(
            rendered.contains("Qdrant now"),
            "plan should mention Qdrant now: {rendered}"
        );
        assert!(
            rendered.contains("will become (md)"),
            "plan should mention will become (md): {rendered}"
        );
        let qdrant_pos = rendered.find("Qdrant now").unwrap();
        let md_pos = rendered.find("will become (md)").unwrap();
        assert!(
            qdrant_pos < md_pos,
            "Qdrant now must appear before will become (md) for the md-wins direction"
        );
    }

    #[test]
    fn render_plan_empty_says_agree() {
        let plan = ReconcilePlan {
            library: "notes".into(),
            root: PathBuf::from("/tmp/notes"),
            files: vec![FilePlan {
                source: "u.md".into(),
                new_content: "same".into(),
                existing: vec![rec("same")],
                action: PlanAction::Unchanged,
            }],
        };
        let rendered = render_plan(&plan);
        assert!(rendered.contains("md and Qdrant agree"));
    }
}
