//! Skills: playbooks the store surfaces BEFORE the work.
//!
//! Procedural memory (`procedure.rs`) is reactive: something broke, and the
//! store hands back the fix that worked last time. A skill is the other half.
//! It is the house way of doing a kind of work, matched against the task the
//! agent is about to start, so the rule arrives before the mistake instead of
//! after the correction.
//!
//! Shape borrowed from OpenViking's "memory, knowledge and skills behind one
//! retrieval surface", built the way the rest of this store is built: no LLM in
//! the loop, one Qdrant point per skill, and the same typed outcome signals
//! that keep procedures honest. A skill nobody's work ever confirmed stays
//! low-trust; one that keeps failing sinks.
//!
//! Identity is the NAME. Writing the same name twice edits the skill and keeps
//! its outcome history, because the second write is the house rule maturing,
//! not a competing copy of it.

use anyhow::Result;

use crate::config::MindConfig;
use crate::storage::{self, SkillHit};

/// Normalize and validate a skill name: trimmed, lowercased, 1-64 chars of
/// `[a-z0-9_-]`. Same rules as a pinned block, so the two namespaces read alike
/// and a name is safe to type, to log, and to use as a stable identity.
pub fn normalize_name(name: &str) -> Result<String> {
    let n = name.trim().to_lowercase();
    if n.is_empty()
        || n.len() > 64
        || !n
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        anyhow::bail!("skill name must be 1-64 chars of [a-z0-9_-], got '{name}'");
    }
    Ok(n)
}

/// Relevance is the base; a verified skill and a positive worked-ratio boost it,
/// repeated failures sink it. Same shape as `procedure::rank_score`, so trust
/// tilts the order without ever becoming a hard gate that buries a better match.
fn rank_score(h: &SkillHit) -> f32 {
    let mut s = h.score;
    if h.verified {
        s += 0.25;
    }
    let total = h.success_count + h.fail_count;
    if total > 0 {
        let ratio = (h.success_count - h.fail_count) as f32 / total as f32;
        let confidence = (total as f32 / 5.0).min(1.0);
        s += 0.20 * ratio * confidence;
    }
    s
}

/// Rank matched skills by relevance + trust. Pure, so it is unit-tested.
pub fn rank(mut hits: Vec<SkillHit>) -> Vec<SkillHit> {
    hits.sort_by(|a, b| {
        rank_score(b)
            .partial_cmp(&rank_score(a))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    hits
}

/// First non-empty line of a skill's trigger, for one-line catalogue rendering.
pub fn summary(h: &SkillHit) -> &str {
    h.when
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
}

/// Create or update a skill.
pub async fn set(config: &MindConfig, name: &str, when: &str, body: &str) -> Result<String> {
    let name = normalize_name(name)?;
    if body.trim().is_empty() {
        anyhow::bail!("skill '{name}' needs a body: what to actually do");
    }
    let existed = storage::get_skill(config, &name).await?.is_some();
    let id = storage::add_skill(config, &name, when, body).await?;
    Ok(format!(
        "{} skill '{name}' [id: {id}]\n  when: {}",
        if existed { "Updated" } else { "Added" },
        when.trim()
    ))
}

/// Match skills against the task about to be done, ranked by relevance + trust.
pub async fn match_task(config: &MindConfig, task: &str, limit: usize) -> Result<String> {
    if task.trim().is_empty() {
        anyhow::bail!("skill match needs a task description");
    }
    let hits = storage::match_skills(config, task, limit).await?;
    Ok(render_matches(&rank(hits), limit))
}

fn render_matches(hits: &[SkillHit], limit: usize) -> String {
    if hits.is_empty() {
        return "No matching skills.".to_string();
    }
    let mut s = String::from("Skills for this task (ranked by relevance + trust):\n");
    for h in hits.iter().take(limit) {
        let mark = if h.verified {
            "✓ verified"
        } else {
            "· unverified"
        };
        s.push_str(&format!(
            "\n[{mark}] (✓{}/✗{}) {}\n  when: {}\n{}\n",
            h.success_count,
            h.fail_count,
            h.name,
            summary(h),
            indent(&h.body)
        ));
    }
    s
}

fn indent(body: &str) -> String {
    body.trim_end()
        .lines()
        .map(|l| format!("  {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The whole catalogue, one line per skill. This is what a context render
/// carries: an agent that can see the names asks for the body when it needs it.
pub async fn list(config: &MindConfig) -> Result<String> {
    let hits = storage::list_skills(config).await?;
    if hits.is_empty() {
        return Ok(
            "No skills yet. Add one with `mgimind skill set <name> --when ... --body ...`."
                .to_string(),
        );
    }
    let mut s = format!("Skills ({}):\n", hits.len());
    for h in &hits {
        let mark = if h.verified { "✓" } else { "·" };
        s.push_str(&format!("{mark} {:<24} {}\n", h.name, summary(h)));
    }
    Ok(s.trim_end().to_string())
}

/// One skill in full.
pub async fn show(config: &MindConfig, name: &str) -> Result<String> {
    let name = normalize_name(name)?;
    let Some(h) = storage::get_skill(config, &name).await? else {
        return Ok(format!("No skill named '{name}'."));
    };
    Ok(format!(
        "{} ({})\n  when: {}\n  outcomes: ✓{} ✗{}\n\n{}",
        h.name,
        if h.verified { "verified" } else { "unverified" },
        h.when.trim(),
        h.success_count,
        h.fail_count,
        h.body.trim_end()
    ))
}

/// Delete a skill and its outcome history.
pub async fn remove(config: &MindConfig, name: &str) -> Result<String> {
    let name = normalize_name(name)?;
    Ok(if storage::remove_skill(config, &name).await? {
        format!("Removed skill '{name}'.")
    } else {
        format!("No skill named '{name}'.")
    })
}

/// Record how applying a skill went. `verify` promotes it to verified on
/// success; pass it only for a deterministic signal, the same bar procedures
/// hold, because a skill nobody checked is a preference, not a proven rule.
pub async fn outcome(
    config: &MindConfig,
    name: &str,
    worked: bool,
    verify: bool,
) -> Result<String> {
    let name = normalize_name(name)?;
    let Some(h) = storage::get_skill(config, &name).await? else {
        return Ok(format!("No skill named '{name}'."));
    };
    storage::procedure_outcome(config, &h.id, worked, verify).await?;
    Ok(format!(
        "Recorded outcome for skill '{name}': {}.",
        match (worked, verify) {
            (true, true) => "worked (success++, verified)",
            (true, false) => "worked (success++)",
            (false, _) => "failed (fail++, demoted in ranking)",
        }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(name: &str, score: f32, verified: bool, succ: i64, fail: i64) -> SkillHit {
        SkillHit {
            id: format!("id-{name}"),
            name: name.to_string(),
            when: format!("when to use {name}\nsecond line"),
            body: "do the thing".to_string(),
            verified,
            success_count: succ,
            fail_count: fail,
            score,
        }
    }

    #[test]
    fn a_name_is_lowercased_and_trimmed() {
        assert_eq!(normalize_name("  Rust-CLI  ").unwrap(), "rust-cli");
    }

    #[test]
    fn a_name_with_spaces_or_slashes_is_refused() {
        for bad in ["two words", "path/like", "", "   ", &"x".repeat(65)] {
            assert!(normalize_name(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn trust_lifts_a_proven_skill_over_a_marginally_closer_one() {
        let ranked = rank(vec![
            hit("unproven", 0.90, false, 0, 0),
            hit("proven", 0.80, true, 5, 0),
        ]);
        assert_eq!(ranked[0].name, "proven");
    }

    #[test]
    fn relevance_still_wins_when_the_gap_is_wide() {
        // The boost is bounded, so a much better match is not buried by a badge.
        let ranked = rank(vec![
            hit("proven", 0.30, true, 5, 0),
            hit("close", 0.95, false, 0, 0),
        ]);
        assert_eq!(ranked[0].name, "close");
    }

    #[test]
    fn repeated_failures_sink_a_skill() {
        let ranked = rank(vec![
            hit("failing", 0.85, false, 0, 6),
            hit("quiet", 0.80, false, 0, 0),
        ]);
        assert_eq!(ranked[0].name, "quiet");
    }

    #[test]
    fn the_summary_is_the_first_non_empty_trigger_line() {
        let h = hit("css", 0.5, false, 0, 0);
        assert_eq!(summary(&h), "when to use css");
    }

    #[test]
    fn an_empty_match_says_so_instead_of_rendering_a_header() {
        assert_eq!(render_matches(&[], 5), "No matching skills.");
    }

    #[test]
    fn a_rendered_match_carries_the_body_and_the_counts() {
        let out = render_matches(&[hit("css", 0.9, true, 3, 1)], 5);
        assert!(out.contains("✓ verified"), "{out}");
        assert!(out.contains("(✓3/✗1)"), "{out}");
        assert!(out.contains("  do the thing"), "{out}");
    }
}
