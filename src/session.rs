use anyhow::{Context, Result};
use chrono::Utc;
use std::fs;
use std::path::PathBuf;

use crate::config;

fn session_file(timestamp: &str, agent: &str) -> PathBuf {
    config::sessions_dir().join(format!("{timestamp}_{agent}.md"))
}

fn current_session_path() -> PathBuf {
    config::sessions_dir().join(".current")
}

pub fn start(agent: &str) -> Result<()> {
    let dir = config::sessions_dir();
    fs::create_dir_all(&dir)?;

    let now = Utc::now();
    let timestamp = now.format("%Y-%m-%d_%H-%M").to_string();
    let path = session_file(&timestamp, agent);

    let header = format!(
        "[session]\nagent = {agent}\nstarted = {}\nstatus = active\n\n---\n\n",
        now.to_rfc3339()
    );

    fs::write(&path, &header)?;

    // Save current session pointer
    fs::write(current_session_path(), path.to_string_lossy().as_bytes())?;

    Ok(())
}

pub fn end(summary: &str) -> Result<()> {
    let current = current_session_path();
    if !current.exists() {
        anyhow::bail!("No active session. Start one with `mgimind session start`");
    }

    let path_str = fs::read_to_string(&current)?;
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
    content.push_str(&footer);

    // Replace status
    content = content.replace("status = active", "status = completed");

    fs::write(&path, content)?;

    // Remove current pointer
    fs::remove_file(&current)?;

    Ok(())
}

pub fn last() -> Result<Option<String>> {
    let dir = config::sessions_dir();
    if !dir.exists() {
        return Ok(None);
    }

    let mut sessions: Vec<_> = fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name.ends_with(".md") && !name.starts_with('.')
        })
        .collect();

    // Sort by name descending (timestamps ensure chronological order)
    sessions.sort_by(|a, b| b.file_name().cmp(&a.file_name()));

    if let Some(entry) = sessions.first() {
        let content = fs::read_to_string(entry.path())
            .context("Failed to read session file")?;
        Ok(Some(content))
    } else {
        Ok(None)
    }
}
