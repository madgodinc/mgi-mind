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
//!   3. Decay report — fold the access journal in, recency-weighted: cold =
//!      high `coldness_score` (old AND long-untouched, not just never-accessed).
//!      Reported always. ARCHIVE (--archive-cold) gets the full recency set
//!      (reversible); PRUNE (--prune-cold, destructive) gets only the
//!      conservative binary subset (count==0), so it never deletes more silently.

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

/// Recency-weighted "coldness" of a memory in days-since-last-relevance.
///
/// The old model was BINARY: cold iff `access_count == 0 AND age >= decay_days`.
/// That misses the common case of a memory accessed a few times long ago and
/// untouched since — `count > 0` made it permanently warm no matter how stale.
///
/// This returns a graduated score: the effective "days since last relevance",
/// discounted by how often it was used. Higher = colder.
///   * No access ever → score = age in days (a never-surfaced memory is as cold
///     as it is old — same signal the binary model used).
///   * Accessed → score = days since `last_access`, divided by a small
///     frequency damp `1 + log10(1 + count)` so a heavily-used memory cools
///     slower. A memory touched once 300 days ago is colder than one touched 50
///     times last week.
///
/// Returns `None` for unknown/unparseable timestamps — never auto-forget what we
/// can't date. Pure, unit-tested without Qdrant. The damp constant is provisional.
pub fn coldness_score(
    created_at: Option<&str>,
    last_access: Option<&str>,
    access_count: u64,
    now: &str,
) -> Option<f64> {
    let now = chrono::DateTime::parse_from_rfc3339(now).ok()?;
    let created = chrono::DateTime::parse_from_rfc3339(created_at?).ok()?;
    let age_days = (now - created).num_seconds() as f64 / 86_400.0;

    if access_count == 0 {
        // Never surfaced: as cold as it is old (matches the old binary signal).
        return Some(age_days.max(0.0));
    }
    // Surfaced at least once: cool from the LAST access, not from creation.
    // last_access should exist when count>0, but fall back to age if missing.
    let since_days = match last_access.and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok()) {
        Some(last) => (now - last).num_seconds() as f64 / 86_400.0,
        None => age_days,
    }
    .max(0.0);
    // Gentle frequency damp: a few uses shouldn't make a long-stale memory
    // immortal. log10 keeps the discount mild (count=5 -> ~1.78x, count=100 ->
    // ~3x), so staleness still dominates — a memory used 5x but untouched ~516
    // days is colder than the 180-day cutoff, which the binary model never was.
    // TODO(circle-3-calibration): this damp + cutoff pair is provisional.
    let damp = 1.0 + (1.0 + access_count as f64).log10();
    Some(since_days / damp)
}

/// Threshold check: a coldness score of at least `decay_days` days makes a
/// memory a decay candidate. Kept separate from `coldness_score` so `run` scores
/// with `last_access` and reuses one cutoff. `None` (undatable) is never cold.
pub fn is_cold_scored(score: Option<f64>, decay_days: i64) -> bool {
    match score {
        Some(s) => s >= decay_days as f64,
        None => false, // undatable → never auto-forget
    }
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

    // 3. Decay report: fold the access journal in, recency-weighted. Cold =
    //    high coldness_score (old + long-untouched), not just never-accessed.
    //    A memory used a few times long ago and stale since now decays too,
    //    instead of being permanently warm on a single ancient access.
    //
    //    TWO sets, deliberately (critic: don't silently delete MORE than before):
    //      * `cold` — recency-weighted, for the REVERSIBLE archive path. Full
    //        power of the new model; a false positive costs a `restore`.
    //      * `cold_prune` — the OLD BINARY subset (count==0 AND age>=decay_days),
    //        for the DESTRUCTIVE delete path. `--prune-cold` thus deletes exactly
    //        what it deleted before the recency change — never more, silently.
    //    `cold_prune ⊆ cold` always.
    let access = crate::access::snapshot();
    let now = chrono::Utc::now().to_rfc3339();
    let mut scored: Vec<(String, f64)> = Vec::new();
    let mut cold_prune: Vec<String> = Vec::new();
    for id in &order {
        if removed.contains(id) {
            continue;
        }
        let stat = access.get(id);
        let count = stat.map(|s| s.count).unwrap_or(0);
        let last_access = stat.and_then(|s| s.last_access.as_deref());
        let created = by_id[id].created_at.as_deref();
        let score = coldness_score(created, last_access, count, &now);
        if is_cold_scored(score, opts.decay_days) {
            scored.push((id.clone(), score.unwrap_or(0.0)));
            // Destructive-eligible only by the conservative binary rule.
            if count == 0 && is_cold_scored(coldness_score(created, None, 0, &now), opts.decay_days)
            {
                cold_prune.push(id.clone());
            }
        }
    }
    // Coldest-first, so a report/archive acts on the most-decayed first.
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let cold: Vec<String> = scored.into_iter().map(|(id, _)| id).collect();
    report.cold_candidates = cold.len();

    // Apply: delete merged duplicates always; for cold memories, ARCHIVE
    // (reversible) takes precedence over PRUNE (destructive) when both are set,
    // so a flag mix never silently deletes what the user meant to archive.
    if opts.apply {
        let to_delete: Vec<String> = removed.iter().cloned().collect();
        storage::delete_memories(config, &to_delete).await?;
        if opts.archive_cold {
            // Archive gets the full recency-weighted set (reversible).
            report.cold_archived = storage::archive_memories(config, &cold).await?;
        } else if opts.prune_cold {
            // Delete only the conservative binary subset — never more than the
            // pre-recency model would have, with no silent expansion.
            report.cold_pruned = cold_prune.len();
            storage::delete_memories(config, &cold_prune).await?;
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
        let cold = |created: Option<&str>, count: u64| {
            is_cold_scored(coldness_score(created, None, count, now), 180)
        };
        // Old + never accessed -> cold (score = age, ~516 days >= 180).
        assert!(cold(Some("2025-01-01T00:00:00+00:00"), 0));
        // Recent + unused -> not cold.
        assert!(!cold(Some("2026-05-20T00:00:00+00:00"), 0));
        // Unknown age -> never cold.
        assert!(!cold(None, 0));
    }

    #[test]
    fn coldness_is_recency_weighted_not_binary() {
        let now = "2026-06-01T00:00:00+00:00";
        let old_create = "2025-01-01T00:00:00+00:00"; // ~516 days before now

        // Never accessed -> score is the age (the old binary signal preserved).
        let never = coldness_score(Some(old_create), None, 0, now).unwrap();
        assert!(
            (never - 516.0).abs() < 2.0,
            "never-accessed score == age, got {never}"
        );

        // THE DISCRIMINATING CASE the binary model got wrong: a memory accessed
        // SEVERAL times but long ago and stale since. Old `is_cold` said "count>0
        // => never cold". Recency-weighted says "last touched ~516 days ago,
        // lightly damped => cold".
        let stale_used = coldness_score(Some(old_create), Some(old_create), 5, now).unwrap();
        assert!(
            is_cold_scored(Some(stale_used), 180),
            "a long-stale-but-used memory must be cold (score {stale_used}), \
             unlike the old binary model"
        );

        // And a memory used recently is NOT cold even if old, because it cools
        // from last_access, not creation.
        let fresh_used =
            coldness_score(Some(old_create), Some("2026-05-28T00:00:00+00:00"), 5, now).unwrap();
        assert!(
            !is_cold_scored(Some(fresh_used), 180),
            "recently-used memory must stay warm (score {fresh_used})"
        );

        // Monotonic sanity: same memory, colder the longer since last access.
        let a =
            coldness_score(Some(old_create), Some("2026-05-01T00:00:00+00:00"), 2, now).unwrap();
        let b =
            coldness_score(Some(old_create), Some("2026-01-01T00:00:00+00:00"), 2, now).unwrap();
        assert!(b > a, "longer since last access => colder ({b} > {a})");

        // More frequent use cools slower (higher damp => lower score) at the same
        // staleness.
        let rare = coldness_score(Some(old_create), Some(old_create), 1, now).unwrap();
        let frequent = coldness_score(Some(old_create), Some(old_create), 100, now).unwrap();
        assert!(
            frequent < rare,
            "frequent use cools slower ({frequent} < {rare})"
        );
    }

    #[test]
    fn prune_subset_is_conservative_binary() {
        // The destructive prune set must be the OLD BINARY rule (count==0), a
        // SUBSET of the recency-weighted archive set — so --prune-cold never
        // deletes more than before the recency change (critic safety contract).
        let now = "2026-06-01T00:00:00+00:00";
        let old = "2025-01-01T00:00:00+00:00"; // >= decay_days
        let decay = 180;
        let recency_cold =
            |count, last| is_cold_scored(coldness_score(Some(old), last, count, now), decay);
        let prune_eligible = |count: u64| {
            count == 0 && is_cold_scored(coldness_score(Some(old), None, 0, now), decay)
        };

        // count==0 old: in BOTH sets.
        assert!(recency_cold(0, None) && prune_eligible(0));
        // count>0 long-stale: in the recency (archive) set, NOT prune-eligible.
        assert!(recency_cold(5, Some(old)), "stale-used is archive-cold");
        assert!(
            !prune_eligible(5),
            "stale-used is NOT prune-eligible (no silent delete)"
        );
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
