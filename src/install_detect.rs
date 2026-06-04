//! v1.5 Phase 6 step 6.2 — install-mode detection collector.
//!
//! Pulls the two counts that the `install_mode::recommend()` pure
//! heuristic consumes:
//!
//! 1. Procedure-outcome events recorded in the last 7 days, used as
//!    a proxy for external-signal frequency (`mind_procedure_outcome`
//!    is the only external-signal source until v1.5 Phase 7 lands the
//!    generalised `mind_outcome` API).
//! 2. Distinct session-agent names seen in the last 30 days, via
//!    `mtime` on the per-agent session pointer files.
//!
//! The collector is best-effort: any fetch failure falls back to 0
//! for that count, which conservatively keeps `recommend()` on the
//! safe `ChatOnly` default. Auto-detect must never crash the
//! `serve` startup path or `doctor` smoke test.

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use std::collections::HashSet;
use std::path::Path;

use crate::config::{self, MindConfig};
use crate::install_mode::DetectInputs;

/// Count distinct session-agent names with activity in the last
/// `days` days. Reads `.current.<agent>` and `.heartbeat.<agent>`
/// pointer mtimes; both encode the same agent identity.
///
/// Pure file-system traversal; no async. Returns 0 on any I/O error
/// (best-effort — see module docs).
pub fn distinct_session_agents(days: i64) -> u32 {
    distinct_session_agents_in(&config::sessions_dir(), days)
}

/// Same as `distinct_session_agents` but scans `dir` instead of the
/// configured sessions dir. Lets unit tests target a tempdir without
/// mutating the `MGIMIND_HOME` env var (which is shared across tests
/// and would race when cargo runs them in parallel).
pub fn distinct_session_agents_in(dir: &Path, days: i64) -> u32 {
    if !dir.exists() {
        return 0;
    }
    let cutoff = Utc::now() - Duration::days(days);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };

    let mut agents: HashSet<String> = HashSet::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };

        // `.current.<agent>` and `.heartbeat.<agent>` are the only
        // per-agent pointers — both touched on session activity.
        let agent_part = if let Some(rest) = name.strip_prefix(".current.") {
            rest
        } else if let Some(rest) = name.strip_prefix(".heartbeat.") {
            rest
        } else {
            continue;
        };

        if !is_active_within(&entry.path(), cutoff) {
            continue;
        }
        agents.insert(agent_part.to_string());
    }
    agents.len() as u32
}

/// Returns true if the path's mtime is after `cutoff`. Treats any
/// stat failure as "not active" — conservative default.
fn is_active_within(path: &Path, cutoff: DateTime<Utc>) -> bool {
    let Ok(meta) = path.metadata() else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    let modified: DateTime<Utc> = modified.into();
    modified >= cutoff
}

/// Count external-signal events in the last `days` days by scanning
/// the knowledge store for procedures with non-empty `last_used`.
///
/// This is the proxy until v1.5 Phase 7 lands `mind_outcome` and
/// gives us a typed signal log to count directly.
///
/// Best-effort: returns 0 on any Qdrant fetch error.
pub async fn external_signal_count(config: &MindConfig, days: i64) -> u32 {
    match try_external_signal_count(config, days).await {
        Ok(n) => n,
        Err(e) => {
            tracing::debug!("install-mode detect: external_signal_count fallback to 0: {e}");
            0
        }
    }
}

async fn try_external_signal_count(config: &MindConfig, days: i64) -> Result<u32> {
    let timestamps = crate::storage::list_procedure_last_used(config).await?;
    let cutoff = Utc::now() - Duration::days(days);

    let count = timestamps
        .iter()
        .filter_map(|ts| DateTime::parse_from_rfc3339(ts).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .filter(|dt| *dt >= cutoff)
        .count();

    Ok(count as u32)
}

/// One-shot collector that returns the full `DetectInputs` payload
/// for `install_mode::recommend()`. Used by `mgimind doctor` and by
/// the first-run path of `mgimind serve`.
pub async fn collect(config: &MindConfig) -> DetectInputs {
    DetectInputs {
        external_signal_count_last_7d: external_signal_count(config, 7).await,
        distinct_session_agents_last_30d: distinct_session_agents(30),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `distinct_session_agents_in` returns 0 on a non-existent sessions
    /// dir (fresh install before first `mgimind serve`). Conservative
    /// default that keeps `recommend()` on ChatOnly.
    #[test]
    fn distinct_agents_returns_zero_when_dir_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let nonexistent = tmp.path().join("sessions-never-created");
        let n = distinct_session_agents_in(&nonexistent, 30);
        assert_eq!(n, 0);
    }

    /// Per-agent pointer file mtimes seed the distinct-agents count.
    /// Three pointer files = three distinct agents.
    ///
    /// Uses the `_in` variant that accepts an explicit dir so the test
    /// does not race the shared `MGIMIND_HOME` env var with parallel
    /// tests (which previously broke `access::tests::flush_then_load_*`).
    #[test]
    fn distinct_agents_counts_recent_pointers() {
        let tmp = tempfile::tempdir().unwrap();
        let sessions = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();

        for agent in ["claude-code", "cursor", "warp"] {
            let path = sessions.join(format!(".current.{agent}"));
            std::fs::write(&path, "").unwrap();
        }

        let n = distinct_session_agents_in(&sessions, 30);
        assert_eq!(n, 3, "expected 3 distinct agents, got {n}");
    }

    /// Pointer files older than the window must not count. Sets mtime
    /// explicitly so the test isn't tied to wall-clock.
    #[test]
    fn distinct_agents_ignores_stale_pointers() {
        use filetime::{FileTime, set_file_mtime};
        let tmp = tempfile::tempdir().unwrap();
        let sessions = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();

        let fresh = sessions.join(".current.fresh-agent");
        let stale = sessions.join(".current.stale-agent");
        std::fs::write(&fresh, "").unwrap();
        std::fs::write(&stale, "").unwrap();

        // Backdate the stale pointer to 60 days ago — well outside the
        // 30-day default window the detector queries with.
        let sixty_days_ago =
            std::time::SystemTime::now() - std::time::Duration::from_secs(60 * 86_400);
        set_file_mtime(&stale, FileTime::from_system_time(sixty_days_ago)).unwrap();

        let n = distinct_session_agents_in(&sessions, 30);
        assert_eq!(n, 1, "stale pointer should not count");
    }
}
