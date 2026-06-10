//! Consolidation (phase Д2, PR2) — the mandatory companion to auto-write.
//!
//! Auto-ingest without consolidation bloats the store and degrades recall (one of
//! the two hard invariants in `docs/PHASE_D2_D6.md`). Consolidation is the bloat
//! control: it merges duplicates and near-duplicates, and reports "cold" memories
//! (old AND never surfaced) for optional pruning.
//!
//! Run as a CLI/cron command (`mgimind consolidate`), NOT inside the hot
//! single-process MCP read loop where a panic is session-fatal. It is DRY-RUN by
//! default: it only reports unless `--apply` is given, and cold pruning needs the
//! extra explicit `--prune-cold` (deletion of real data is opt-in, never implicit).
//!
//! Three operations on `memory`-typed points (procedures are untouched):
//!   1. Exact dedup — identical content within a library (by stored blake3 `hash`).
//!   2. Near-dup merge — cosine >= threshold via each point's stored vector
//!      (no re-embedding); keep the richer/older record, drop the other.
//!   3. Decay report — fold the access journal in: cold = older than `decay_days`
//!      AND access_count == 0. Reported always; deleted only with `--prune-cold`.

use anyhow::Result;
use std::collections::{HashMap, HashSet};

use crate::config::MindConfig;
use crate::storage::{self, MemoryMeta};

/// How many neighbors to pull per point in the near-dup scan. A handful is plenty:
/// duplicates cluster tightly, so anything past the first few is below threshold.
const NEAR_NEIGHBORS: u64 = 5;

#[derive(Debug, Default, Clone)]
pub struct Options {
    /// Actually mutate the store. Without it, consolidation only reports.
    pub apply: bool,
    /// Scope to one library (None = all).
    pub library: Option<String>,
    /// Cosine threshold for "near-duplicate" (0..1). High by default so only true
    /// near-identicals merge - distinct-but-related memories are NOT collapsed.
    pub near_dup_threshold: f32,
    /// A memory older than this many days with zero recorded accesses is "cold".
    pub decay_days: i64,
    /// Also DELETE cold memories (requires `apply`). Off by default - pruning real
    /// data is opt-in.
    pub prune_cold: bool,
    /// ARCHIVE cold memories instead of deleting (requires `apply`): hide them
    /// from default search but keep them restorable. Off by default. The safe,
    /// reversible forgetting path — prefer this over `prune_cold` when you want
    /// to forget without destroying. If both are set, archive wins (the
    /// non-destructive choice takes precedence).
    pub archive_cold: bool,
}

impl Options {
    pub fn with_defaults(mut self) -> Self {
        if self.near_dup_threshold <= 0.0 {
            self.near_dup_threshold = 0.97;
        }
        // Only a NEGATIVE decay_days is "unset"; an explicit 0 means "everything
        // with zero accesses is cold" (a real intent, e.g. archive-all-unused).
        // Callers that want the default pass it explicitly; the CLI default is
        // 180 via clap, the preview callers (MCP/viewer) pass 180 directly.
        if self.decay_days < 0 {
            self.decay_days = 180;
        }
        self
    }
}

#[derive(Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Report {
    pub scanned: usize,
    pub exact_dups_removed: usize,
    pub near_dups_removed: usize,
    pub cold_candidates: usize,
    pub cold_pruned: usize,
    pub cold_archived: usize,
    pub applied: bool,
}

/// Choose which of two near-duplicate memories to DROP: keep the one with more
/// content (richer); on a tie keep the older one (earlier `created_at`, stable
/// provenance). Returns the id to drop. Pure + deterministic for testability.
fn choose_drop<'a>(a: &'a MemoryMeta, b: &'a MemoryMeta) -> &'a str {
    let (la, lb) = (a.content.chars().count(), b.content.chars().count());
    if la != lb {
        // Drop the shorter one.
        return if la < lb { &a.id } else { &b.id };
    }
    // Equal length: keep the earlier created_at (drop the later). RFC3339 in UTC
    // sorts lexicographically, so a string compare is a time compare here.
    match (&a.created_at, &b.created_at) {
        (Some(ca), Some(cb)) if ca <= cb => &b.id,
        (Some(_), Some(_)) => &a.id,
        // Missing timestamps: arbitrary but stable - drop b.
        _ => &b.id,
    }
}

/// Is a memory "cold" (a decay candidate): older than `decay_days` AND never
/// surfaced (access_count == 0)? Pure, so it is unit-tested without Qdrant.
pub fn is_cold(created_at: Option<&str>, access_count: u64, now: &str, decay_days: i64) -> bool {
    if access_count > 0 {
        return false;
    }
    let Some(created) = created_at else {
        return false; // unknown age -> never auto-prune
    };
    let (Ok(created), Ok(now)) = (
        chrono::DateTime::parse_from_rfc3339(created),
        chrono::DateTime::parse_from_rfc3339(now),
    ) else {
        return false;
    };
    (now - created).num_days() >= decay_days
}

/// Run a consolidation pass. Dry-run unless `opts.apply`.
pub async fn run(config: &MindConfig, opts: Options) -> Result<Report> {
    let opts = opts.with_defaults();
    let metas = storage::scroll_memory_meta(config).await?;

    // Index by id, and keep a stable iteration order for determinism.
    let order: Vec<String> = metas
        .iter()
        .filter(|m| opts.library.as_deref().is_none_or(|l| m.library == l))
        .map(|m| m.id.clone())
        .collect();
    let by_id: HashMap<String, MemoryMeta> = metas.into_iter().map(|m| (m.id.clone(), m)).collect();

    let mut removed: HashSet<String> = HashSet::new();
    let mut report = Report {
        scanned: order.len(),
        applied: opts.apply,
        ..Default::default()
    };

    // 1. Exact dedup: group surviving points by (library, hash); keep one.
    let mut groups: HashMap<(String, String), Vec<String>> = HashMap::new();
    for id in &order {
        let m = &by_id[id];
        if let Some(hash) = &m.hash {
            groups
                .entry((m.library.clone(), hash.clone()))
                .or_default()
                .push(id.clone());
        }
    }
    for ids in groups.values() {
        if ids.len() < 2 {
            continue;
        }
        // Keep the first (canonical), drop the rest.
        let mut canonical = &ids[0];
        for id in ids {
            // Prefer earliest created_at as canonical.
            if by_id[id].created_at < by_id[canonical].created_at {
                canonical = id;
            }
        }
        for id in ids {
            if id != canonical {
                removed.insert(id.clone());
                report.exact_dups_removed += 1;
            }
        }
    }

    // 2. Near-dup merge: for each surviving point, look at its nearest neighbors
    //    (by stored vector) within the same library; merge any above threshold.
    for id in &order {
        if removed.contains(id) {
            continue;
        }
        let m = &by_id[id];
        let neighbors =
            storage::near_neighbors_by_id(config, id, Some(&m.library), NEAR_NEIGHBORS).await?;
        for (nid, score) in neighbors {
            if score < opts.near_dup_threshold {
                break; // neighbors come back sorted desc; nothing else qualifies
            }
            if removed.contains(&nid) || nid == *id {
                continue;
            }
            let Some(other) = by_id.get(&nid) else {
                continue;
            };
            let drop_id = choose_drop(m, other).to_string();
            removed.insert(drop_id.clone());
            report.near_dups_removed += 1;
            if drop_id == *id {
                break; // this point itself was dropped; stop scanning its neighbors
            }
        }
    }

    // 3. Decay report: fold the access journal in. Cold = old AND never surfaced.
    let access = crate::access::snapshot();
    let now = chrono::Utc::now().to_rfc3339();
    let mut cold: Vec<String> = Vec::new();
    for id in &order {
        if removed.contains(id) {
            continue;
        }
        let count = access.get(id).map(|s| s.count).unwrap_or(0);
        if is_cold(
            by_id[id].created_at.as_deref(),
            count,
            &now,
            opts.decay_days,
        ) {
            cold.push(id.clone());
        }
    }
    report.cold_candidates = cold.len();

    // Apply: delete merged duplicates always; for cold memories, ARCHIVE
    // (reversible) takes precedence over PRUNE (destructive) when both are set,
    // so a flag mix never silently deletes what the user meant to archive.
    if opts.apply {
        let to_delete: Vec<String> = removed.iter().cloned().collect();
        storage::delete_memories(config, &to_delete).await?;
        if opts.archive_cold {
            report.cold_archived = storage::archive_memories(config, &cold).await?;
        } else if opts.prune_cold {
            report.cold_pruned = cold.len();
            storage::delete_memories(config, &cold).await?;
        }
        // Persist any in-memory access counts before we finish.
        crate::access::flush();
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(id: &str, lib: &str, content: &str, created: &str) -> MemoryMeta {
        MemoryMeta {
            id: id.into(),
            library: lib.into(),
            content: content.into(),
            created_at: Some(created.into()),
            hash: None,
        }
    }

    #[test]
    fn choose_drop_keeps_longer_content() {
        let a = meta("a", "l", "short", "2026-01-01T00:00:00+00:00");
        let b = meta(
            "b",
            "l",
            "a much longer memory body",
            "2026-02-01T00:00:00+00:00",
        );
        assert_eq!(choose_drop(&a, &b), "a", "drop the shorter one");
    }

    #[test]
    fn choose_drop_tie_keeps_older() {
        let a = meta("a", "l", "same len", "2026-01-01T00:00:00+00:00");
        let b = meta("b", "l", "same len", "2026-03-01T00:00:00+00:00");
        // equal length -> keep older (a) -> drop newer (b)
        assert_eq!(choose_drop(&a, &b), "b");
    }

    #[test]
    fn cold_requires_age_and_zero_access() {
        let now = "2026-06-01T00:00:00+00:00";
        // Old + unused -> cold.
        assert!(is_cold(Some("2025-01-01T00:00:00+00:00"), 0, now, 180));
        // Old but accessed -> not cold.
        assert!(!is_cold(Some("2025-01-01T00:00:00+00:00"), 3, now, 180));
        // Recent + unused -> not cold.
        assert!(!is_cold(Some("2026-05-20T00:00:00+00:00"), 0, now, 180));
        // Unknown age -> never cold.
        assert!(!is_cold(None, 0, now, 180));
    }

    #[test]
    fn defaults_fill_in() {
        let o = Options::default().with_defaults();
        assert_eq!(o.near_dup_threshold, 0.97);
        // decay_days: only a NEGATIVE value is "unset" and filled to 180. An
        // explicit 0 (and Options::default()'s 0) is honored as "0 days = all
        // zero-access memories are cold" — so callers wanting the 180 default
        // pass it explicitly (CLI clap default, MCP/viewer literals).
        assert_eq!(o.decay_days, 0, "an explicit/zero decay_days is kept as 0");
        let filled = Options {
            decay_days: -1,
            ..Default::default()
        }
        .with_defaults();
        assert_eq!(filled.decay_days, 180, "a negative decay_days fills to 180");
    }
}
