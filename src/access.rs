//! In-process access counters — the decay foundation (phase Д2/Д4).
//!
//! Decay needs to know which memories are actually used. The obvious way —
//! bump an `access_count` payload field on every search hit — is a write on the
//! read path, which conflicts with audit #5 (reads must stay read-only to
//! Qdrant: a search should never mutate the store). So instead we count accesses
//! in PROCESS memory and periodically flush them to a small JSON journal,
//! completely decoupled from the vector store. Consolidation (PR2) reads this
//! journal to decay memories that are old AND rarely accessed.
//!
//! Single-process MCP makes this natural: one long-lived `mgimind mcp` process
//! accumulates counts across the whole session, so the in-memory tally is
//! meaningful (it is not reset per call as the old spawn-per-call model would
//! have forced). The flush is threshold-based ("periodic"): cheap, and durable
//! enough that a session that never exits cleanly still persists its counts.

use once_cell::sync::OnceCell;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// Flush to disk after this many recorded accesses since the last flush. Keeps
/// the read path light (no disk write per search) while bounding how many counts
/// a hard crash can lose. The journal is tiny, so the write itself is cheap.
const FLUSH_EVERY: u64 = 64;

/// One memory's usage stats. `count` is lifetime accesses observed across all
/// journals; `last_access` is the most recent RFC3339 timestamp.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AccessStat {
    pub count: u64,
    pub last_access: Option<String>,
}

/// id -> stat. Loaded lazily from the on-disk journal on first use, then kept
/// authoritative in process and merged back on flush.
static LOG: OnceCell<Mutex<HashMap<String, AccessStat>>> = OnceCell::new();

/// Recorded accesses since the last flush (cheap, lock-free trigger check).
static PENDING: AtomicU64 = AtomicU64::new(0);

fn journal_path() -> std::path::PathBuf {
    crate::config::mind_home().join("access_journal.json")
}

fn load_from_disk() -> HashMap<String, AccessStat> {
    std::fs::read_to_string(journal_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn log() -> &'static Mutex<HashMap<String, AccessStat>> {
    LOG.get_or_init(|| Mutex::new(load_from_disk()))
}

/// Record that these memory ids were just surfaced by a search. In-process only;
/// no Qdrant write. Flushes to the journal once `FLUSH_EVERY` accesses accumulate.
/// `now` is the caller-supplied RFC3339 timestamp (kept as a param so this stays
/// pure/testable and so callers reuse the timestamp they already computed).
pub fn record(ids: &[String], now: &str) {
    if ids.is_empty() {
        return;
    }
    if let Ok(mut map) = log().lock() {
        for id in ids {
            let stat = map.entry(id.clone()).or_default();
            stat.count += 1;
            stat.last_access = Some(now.to_string());
        }
    }
    let pending = PENDING.fetch_add(ids.len() as u64, Ordering::Relaxed) + ids.len() as u64;
    if pending >= FLUSH_EVERY {
        // `record` is sync and sits inside `storage::search_filtered`, which is
        // async. Flushing inline would run two fsyncs (temp file, then the
        // directory) on a Tokio worker thread and stall every other task on it,
        // so hand the write to the blocking pool when there is a runtime to hand
        // it to. Outside a runtime (CLI, tests) the inline flush is correct.
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn_blocking(flush);
            }
            Err(_) => flush(),
        }
    }
}

/// Persist the in-memory counts to the journal (atomic write). Merges with
/// whatever is on disk by taking the max count and latest timestamp, so two
/// processes (e.g. the MCP server and a `mgimind consolidate` CLI run) can't
/// clobber each other's tallies. Best-effort: a flush failure is non-fatal (the
/// counts stay in memory and retry on the next threshold).
pub fn flush() {
    PENDING.store(0, Ordering::Relaxed);
    // Copy the counts out under the lock and release it before touching the
    // disk. The write below ends in two fsyncs, and `record` on the search path
    // takes this same lock; holding it across the write would put every
    // concurrent search behind the slowest part of the flush.
    let map = {
        let Ok(guard) = log().lock() else {
            return;
        };
        guard.clone()
    };
    let mut merged = load_from_disk();
    for (id, stat) in map.iter() {
        let e = merged.entry(id.clone()).or_default();
        e.count = e.count.max(stat.count);
        if stat.last_access > e.last_access {
            e.last_access = stat.last_access.clone();
        }
    }
    if let Ok(json) = serde_json::to_string(&merged) {
        let _ = crate::util::atomic_write_str(&journal_path(), &json);
    }
}

/// Rename journal entries after a `relibrary` move (old id -> new id). Merges
/// with any stat already at the new id using the same policy as `flush` (max
/// count, latest `last_access`) so a point that already had hits before the
/// move does not lose them. Updates both the on-disk journal and the
/// in-process cache, so a long-lived MCP process does not flush a stale entry
/// back under the old id later. Returns how many ids were actually renamed.
pub fn rekey_ids(mapping: &HashMap<String, String>) -> usize {
    if mapping.is_empty() {
        return 0;
    }

    fn merge_in(map: &mut HashMap<String, AccessStat>, mapping: &HashMap<String, String>) -> usize {
        let mut renamed = 0;
        for (old_id, new_id) in mapping {
            let Some(stat) = map.remove(old_id) else {
                continue;
            };
            let entry = map.entry(new_id.clone()).or_default();
            entry.count = entry.count.max(stat.count);
            if stat.last_access > entry.last_access {
                entry.last_access = stat.last_access;
            }
            renamed += 1;
        }
        renamed
    }

    let mut on_disk = load_from_disk();
    let renamed = merge_in(&mut on_disk, mapping);
    if renamed > 0 {
        let _ = serde_json::to_string(&on_disk)
            .map(|json| crate::util::atomic_write_str(&journal_path(), &json));
    }
    if let Ok(mut in_proc) = log().lock() {
        merge_in(&mut in_proc, mapping);
    }
    renamed
}

/// Read the merged access journal (on-disk + in-process). Used by consolidation
/// to decide what to decay. Does not mutate anything.
pub fn snapshot() -> HashMap<String, AccessStat> {
    let mut merged = load_from_disk();
    if let Ok(map) = log().lock() {
        for (id, stat) in map.iter() {
            let e = merged.entry(id.clone()).or_default();
            e.count = e.count.max(stat.count);
            if stat.last_access > e.last_access {
                e.last_access = stat.last_access.clone();
            }
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    // Each test isolates the data dir via MGIMIND_HOME so the journal path is a
    // temp dir. The in-process LOG is a process global, so we don't assert on
    // cross-test in-memory state — we assert on the journal file via flush/load.
    // MGIMIND_HOME_TEST_LOCK serializes every test in the crate that overrides
    // this env var — see its doc comment in config.rs.

    #[test]
    fn flush_then_load_roundtrips_counts() {
        let _guard = crate::config::MGIMIND_HOME_TEST_LOCK.blocking_lock();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("MGIMIND_HOME", dir.path()) };

        let path = dir.path().join("access_journal.json");
        let mut m: HashMap<String, AccessStat> = HashMap::new();
        m.insert(
            "id-a".into(),
            AccessStat {
                count: 3,
                last_access: Some("2026-01-01T00:00:00Z".into()),
            },
        );
        crate::util::atomic_write_str(&path, &serde_json::to_string(&m).unwrap()).unwrap();

        let loaded = load_from_disk();
        assert_eq!(loaded.get("id-a").unwrap().count, 3);

        unsafe { std::env::remove_var("MGIMIND_HOME") };
    }

    #[test]
    fn record_empty_is_noop() {
        // Must not panic or write anything for an empty id list.
        record(&[], "2026-01-01T00:00:00Z");
    }

    #[test]
    fn rekey_merges_into_an_existing_entry_by_max_count_latest_access() {
        let _guard = crate::config::MGIMIND_HOME_TEST_LOCK.blocking_lock();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("MGIMIND_HOME", dir.path()) };

        let mut seed: HashMap<String, AccessStat> = HashMap::new();
        seed.insert(
            "old-id".into(),
            AccessStat {
                count: 5,
                last_access: Some("2026-01-01T00:00:00Z".into()),
            },
        );
        seed.insert(
            "new-id".into(),
            AccessStat {
                count: 2,
                last_access: Some("2026-02-01T00:00:00Z".into()),
            },
        );
        crate::util::atomic_write_str(&journal_path(), &serde_json::to_string(&seed).unwrap())
            .unwrap();

        let mut mapping = HashMap::new();
        mapping.insert("old-id".to_string(), "new-id".to_string());
        let renamed = rekey_ids(&mapping);
        assert_eq!(renamed, 1);

        let after = load_from_disk();
        assert!(!after.contains_key("old-id"), "old key must be gone");
        let merged = after.get("new-id").expect("new key must exist");
        assert_eq!(merged.count, 5, "keeps the larger count");
        assert_eq!(
            merged.last_access.as_deref(),
            Some("2026-02-01T00:00:00Z"),
            "keeps the later timestamp"
        );

        unsafe { std::env::remove_var("MGIMIND_HOME") };
    }

    #[test]
    fn rekey_empty_mapping_is_noop() {
        assert_eq!(rekey_ids(&HashMap::new()), 0);
    }
}
