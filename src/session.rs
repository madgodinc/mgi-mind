#![allow(dead_code)]
// RecoveredSession.agent + ZombieReport.session_path are surfaced for callers
// (doctor output, zombie cleanup CLI). Lib-side production reads `path` only.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use std::fs;
use std::path::PathBuf;
use uuid::Uuid;

use crate::config;

/// Default idle window before an active session is considered a zombie.
/// 30 minutes balances "user took a break" against "process crashed an hour
/// ago and we still claim it's running". `claude-code` sessions can sit on a
/// long task this long; `cursor` flows are shorter but the overshoot is
/// cheap (just an extra minute of "stale-looking" status).
pub const DEFAULT_IDLE_THRESHOLD_MINUTES: i64 = 30;

/// Per-agent active-session pointer. Each agent gets its own `.current.<agent>`
/// so two agents no longer clobber a single shared `.current` (audit #14).
fn current_pointer(agent: &str) -> PathBuf {
    config::sessions_dir().join(format!(".current.{}", sanitize(agent)))
}

/// Per-agent heartbeat file. Touched on every tool call so an interrupted
/// session (Ctrl-C, kill, crash) can be detected and auto-closed on the next
/// `session_start` of the same agent. Separate file (not a rewrite of the
/// session `.md`) so heartbeat writes are 1 small atomic write, not a re-read
/// + re-write of the whole session body.
fn heartbeat_pointer(agent: &str) -> PathBuf {
    config::sessions_dir().join(format!(".heartbeat.{}", sanitize(agent)))
}

/// Injective filesystem-safe encoding of an agent name. Every byte outside
/// `[A-Za-z0-9-]` - including the escape byte `_` itself - becomes `_HH`, so
/// distinct names can never collapse onto the same `.current.<agent>` pointer.
/// (The old `_`-for-everything mapping reintroduced audit #14: `team a`,
/// `team/a`, `team.a` all shared one pointer and clobbered each other.)
fn sanitize(agent: &str) -> String {
    let mut out = String::with_capacity(agent.len());
    for &b in agent.as_bytes() {
        if b.is_ascii_alphanumeric() || b == b'-' {
            out.push(b as char);
        } else {
            out.push('_');
            out.push_str(&format!("{b:02X}"));
        }
    }
    out
}

pub fn start(agent: &str) -> Result<StartReport> {
    let dir = config::sessions_dir();
    fs::create_dir_all(&dir)?;

    // v0.13: if a previous session for this agent is still active AND its
    // heartbeat is older than the idle threshold, auto-close it before
    // starting a new one. The user is told (warning in the returned struct),
    // we never silently swallow it.
    let recovered = recover_zombie(agent, DEFAULT_IDLE_THRESHOLD_MINUTES).ok().flatten();

    let now = Utc::now();
    // Seconds + a short random suffix → two starts in the same minute can't
    // collide and overwrite each other's session file (audit #14).
    let timestamp = now.format("%Y-%m-%d_%H-%M-%S").to_string();
    let short = &Uuid::new_v4().simple().to_string()[..6];
    let path = dir.join(format!("{timestamp}_{}_{short}.md", sanitize(agent)));

    let header = format!(
        "[session]\nagent = {agent}\nstarted = {}\nstatus = active\n\n---\n\n",
        now.to_rfc3339()
    );

    crate::util::atomic_write_str(&path, &header)?;
    crate::util::atomic_write_str(&current_pointer(agent), &path.to_string_lossy())?;
    // First heartbeat is "now" — session is freshly alive.
    let _ = touch(agent);

    Ok(StartReport { recovered })
}

/// Result of `start`. Carries an optional warning about a previously-active
/// session that we just auto-closed because its heartbeat was stale (v0.13).
/// The caller (CLI / MCP) is responsible for surfacing this to the user so the
/// recovery is **visible**, not silent magic.
pub struct StartReport {
    pub recovered: Option<RecoveredSession>,
}

pub struct RecoveredSession {
    pub agent: String,
    pub path: PathBuf,
    pub last_active_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
}

pub fn end(agent: &str, summary: &str) -> Result<()> {
    let pointer = current_pointer(agent);
    if !pointer.exists() {
        anyhow::bail!(
            "No active session for agent '{agent}'. Start one with `mgimind session start --agent {agent}`"
        );
    }

    let path_str = fs::read_to_string(&pointer)?;
    let path = PathBuf::from(path_str.trim());

    if !path.exists() {
        anyhow::bail!("Session file not found: {}", path.display());
    }

    let now = Utc::now();
    let footer = format!(
        "\n---\n\n[end]\nended = {}\nsummary = {summary}\n",
        now.to_rfc3339()
    );

    let mut content = fs::read_to_string(&path)?;
    content = content.replace("status = active", "status = completed");
    content.push_str(&footer);

    crate::util::atomic_write_str(&path, &content)?;
    fs::remove_file(&pointer).ok();

    Ok(())
}

/// Most recent session. With `agent`, scoped to that agent's sessions;
/// otherwise the globally newest (audit #14: `last` is no longer a blind global pick).
pub fn last(agent: Option<&str>) -> Result<Option<String>> {
    let dir = config::sessions_dir();
    if !dir.exists() {
        return Ok(None);
    }

    let agent_tag = agent.map(|a| format!("_{}_", sanitize(a)));

    let mut sessions: Vec<_> = fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if !name.ends_with(".md") || name.starts_with('.') {
                return false;
            }
            match &agent_tag {
                Some(tag) => name.contains(tag.as_str()),
                None => true,
            }
        })
        .collect();

    // Timestamped names sort chronologically; newest first.
    sessions.sort_by_key(|b| std::cmp::Reverse(b.file_name()));

    if let Some(entry) = sessions.first() {
        let content = fs::read_to_string(entry.path()).context("Failed to read session file")?;
        Ok(Some(content))
    } else {
        Ok(None)
    }
}

/// Stamp `now` into the heartbeat file for `agent`. Cheap: one atomic write
/// of an RFC3339 string. No-op (best-effort) if the sessions dir doesn't
/// exist or the agent has no active session — we don't want a heartbeat
/// failure to abort a real tool call. Returns `Ok(())` either way.
pub fn touch(agent: &str) -> Result<()> {
    if !current_pointer(agent).exists() {
        // No active session — nothing to keep alive. Not an error.
        return Ok(());
    }
    let now = Utc::now().to_rfc3339();
    let dir = config::sessions_dir();
    if !dir.exists() {
        return Ok(());
    }
    let _ = crate::util::atomic_write_str(&heartbeat_pointer(agent), &now);
    Ok(())
}

/// Touch every agent that currently has an active session. The MCP/CLI
/// dispatcher calls this after each tool call — we don't know which agent
/// is the caller, so we keep them all warm. A real-world concurrent-agent
/// setup would still see each agent's own heartbeat update on their own
/// calls; this just ensures a single-agent session can never go cold
/// because we forgot which name it registered under.
pub fn touch_all_active() {
    let dir = config::sessions_dir();
    if !dir.exists() {
        return;
    }
    let Ok(read) = fs::read_dir(&dir) else {
        return;
    };
    let now = Utc::now().to_rfc3339();
    for entry in read.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Some(rest) = name.strip_prefix(".current.") {
            // Build heartbeat path for the same sanitized agent suffix.
            let hb = dir.join(format!(".heartbeat.{}", rest));
            let _ = crate::util::atomic_write_str(&hb, &now);
        }
    }
}

/// Read the heartbeat timestamp for an agent. None if no heartbeat or
/// malformed.
pub fn read_heartbeat(agent: &str) -> Option<DateTime<Utc>> {
    let path = heartbeat_pointer(agent);
    let s = fs::read_to_string(&path).ok()?;
    DateTime::parse_from_rfc3339(s.trim())
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

/// Read the `started` timestamp from a session.md file (`started = <rfc3339>`).
fn read_started_from_file(path: &PathBuf) -> Option<DateTime<Utc>> {
    let content = fs::read_to_string(path).ok()?;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("started = ") {
            return DateTime::parse_from_rfc3339(rest.trim())
                .ok()
                .map(|d| d.with_timezone(&Utc));
        }
    }
    None
}

/// If `agent` has an active session whose heartbeat is older than
/// `idle_minutes`, auto-close it with a reconstructed summary and return a
/// `RecoveredSession`. Idempotent: returns `None` if there's nothing to
/// recover (no active session, fresh heartbeat, or session file gone).
fn recover_zombie(agent: &str, idle_minutes: i64) -> Result<Option<RecoveredSession>> {
    let pointer = current_pointer(agent);
    if !pointer.exists() {
        return Ok(None);
    }

    // Heartbeat policy: if no heartbeat file at all, we *don't* auto-close
    // immediately — there's a window after a `start` before the first tool
    // call writes one. Instead, fall back to the session's `started` time:
    // a session whose start was N minutes ago and never wrote a heartbeat is
    // also a zombie (the parent died between `start` and first activity).
    let now = Utc::now();
    let path_str = fs::read_to_string(&pointer).context("read current pointer")?;
    let path = PathBuf::from(path_str.trim());
    if !path.exists() {
        // Pointer is stale (file got removed). Clean it up.
        let _ = fs::remove_file(&pointer);
        return Ok(None);
    }

    let last_active = read_heartbeat(agent).or_else(|| read_started_from_file(&path));
    let Some(last) = last_active else {
        return Ok(None);
    };
    let age = now.signed_duration_since(last);
    if age.num_minutes() < idle_minutes {
        return Ok(None);
    }

    // Build a synthetic summary out of what we know — no fabrication.
    let summary = format!(
        "Auto-closed by v0.13 liveness check. Last activity at {} (idle for {} min). \
         The session terminated without calling mind_session_end — usually a kill, \
         Ctrl-C, or crash. No explicit summary recorded.",
        last.to_rfc3339(),
        age.num_minutes()
    );

    let started_at = read_started_from_file(&path);

    end(agent, &summary)?;

    // Best-effort cleanup of the heartbeat file (end() already removed the
    // current-pointer).
    let _ = fs::remove_file(heartbeat_pointer(agent));

    Ok(Some(RecoveredSession {
        agent: agent.to_string(),
        path,
        last_active_at: Some(last),
        started_at,
    }))
}

/// All agents that currently look like zombies (active session, idle longer
/// than `idle_minutes`). Used by `mind_doctor` / `mind_stats` to surface the
/// problem to the user before it's silently auto-closed.
pub fn list_zombies(idle_minutes: i64) -> Vec<ZombieReport> {
    let dir = config::sessions_dir();
    let mut out = Vec::new();
    if !dir.exists() {
        return out;
    }
    let Ok(read) = fs::read_dir(&dir) else {
        return out;
    };
    let now = Utc::now();
    for entry in read.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(_) = name.strip_prefix(".current.") else {
            continue;
        };
        // Recover agent from sanitized suffix is lossy (we'd need to reverse
        // sanitize). Instead we expose the sanitized form back — the agent
        // can look up their own pointer name unambiguously.
        let sanitized = name.trim_start_matches(".current.").to_string();
        // Read pointer to find session file → last activity.
        let pointer = dir.join(&*name);
        let Ok(path_str) = fs::read_to_string(&pointer) else {
            continue;
        };
        let session_path = PathBuf::from(path_str.trim());
        // Heartbeat by sanitized suffix.
        let hb_path = dir.join(format!(".heartbeat.{sanitized}"));
        let last_active = fs::read_to_string(&hb_path)
            .ok()
            .and_then(|s| DateTime::parse_from_rfc3339(s.trim()).ok())
            .map(|d| d.with_timezone(&Utc))
            .or_else(|| read_started_from_file(&session_path));
        let Some(last) = last_active else {
            continue;
        };
        let age_min = now.signed_duration_since(last).num_minutes();
        if age_min >= idle_minutes {
            out.push(ZombieReport {
                agent_sanitized: sanitized,
                session_path,
                last_active_at: last,
                age_minutes: age_min,
            });
        }
    }
    out
}

#[derive(Debug, Clone)]
pub struct ZombieReport {
    pub agent_sanitized: String,
    pub session_path: PathBuf,
    pub last_active_at: DateTime<Utc>,
    pub age_minutes: i64,
}

#[cfg(test)]
mod tests {
    use super::sanitize;

    #[test]
    fn sanitize_is_injective_for_colliding_names() {
        // The old mapping collapsed all of these to "team_a" (audit #14 rerun).
        let a = sanitize("team a");
        let b = sanitize("team_a");
        let c = sanitize("team/a");
        let d = sanitize("team.a");
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, d);
        assert_ne!(b, c);
        assert_ne!(b, d);
        assert_ne!(c, d);
    }

    #[test]
    fn sanitize_preserves_safe_chars() {
        assert_eq!(sanitize("claude-code"), "claude-code");
        assert_eq!(sanitize("Cursor2"), "Cursor2");
    }

    #[test]
    fn sanitize_escapes_underscore_itself() {
        // A literal underscore must not be confused with the escape prefix.
        assert_eq!(sanitize("_"), "_5F");
        assert_ne!(sanitize("a_b"), sanitize("a b"));
    }
}
