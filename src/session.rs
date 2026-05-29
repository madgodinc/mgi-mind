use anyhow::{Context, Result};
use chrono::Utc;
use std::fs;
use std::path::PathBuf;
use uuid::Uuid;

use crate::config;

/// Per-agent active-session pointer. Each agent gets its own `.current.<agent>`
/// so two agents no longer clobber a single shared `.current` (audit #14).
fn current_pointer(agent: &str) -> PathBuf {
    config::sessions_dir().join(format!(".current.{}", sanitize(agent)))
}

/// Injective filesystem-safe encoding of an agent name. Every byte outside
/// `[A-Za-z0-9-]` — including the escape byte `_` itself — becomes `_HH`, so
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

pub fn start(agent: &str) -> Result<()> {
    let dir = config::sessions_dir();
    fs::create_dir_all(&dir)?;

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

    Ok(())
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
