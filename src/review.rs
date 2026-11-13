//! Human review: facts that wait on a person.
//!
//! A fact enters review in two ways: a write that lands `contested` (it
//! contradicts a value already on record and the duel could not settle it), or
//! an agent asking a person directly (`mind_review action=ask`). Either way the
//! fact carries `review = pending` and a question, and it stays that way until a
//! person answers.
//!
//! The rule that makes this a review and not a flag: the agent that wrote the
//! fact holds no verb that answers it. The agent surfaces (MCP, the agent HTTP
//! API) can list the queue and ask; they cannot accept or reject. Answers come
//! from exactly two places, both gated on something an agent process lacks:
//!
//! * `mgimind review accept|reject` refuses unless stdin and stdout are a real
//!   terminal, and then asks the person to type the fact's short id back. An
//!   agent's shell runs without a terminal, so the command stops there.
//! * The viewer's review tab, which sends the person's review PIN with each
//!   answer. The PIN is set only at a terminal (`mgimind review pin`, hidden
//!   input) and stored as an Argon2id hash, so a process that can read the data
//!   directory still cannot produce it.
//!
//! Once answered the decision is final for agents: an accepted fact cannot be
//! unseated by a duel or invalidated by an agent, and a rejected triple is
//! refused if an agent writes it again. Every answer is an audit event with
//! the surface that produced it.

use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use qdrant_client::qdrant::{
    Condition, Filter, PointsIdsList, ScrollPointsBuilder, SetPayloadPointsBuilder,
};

use crate::config::MindConfig;
use crate::knowledge::{EntryStatus, Fact};
use crate::storage;

pub const PENDING: &str = "pending";
pub const ACCEPTED: &str = "accepted";
pub const REJECTED: &str = "rejected";

/// Does a person hold this fact: a question still open, or an answer given in
/// its favour? Such a fact is outside every agent-driven retirement path.
pub fn holds(review: Option<&str>) -> bool {
    matches!(review, Some(PENDING) | Some(ACCEPTED))
}

/// One open question: the fact that waits, plus every other live value on its
/// (subject, predicate) axis, so the person sees what it contradicts.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Question {
    pub fact: Fact,
    pub rivals: Vec<Fact>,
}

/// The decision a person gives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Accept,
    Reject,
}

impl Answer {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "accept" => Some(Answer::Accept),
            "reject" => Some(Answer::Reject),
            _ => None,
        }
    }
}

/// Every fact waiting on a person, with its rivals, oldest question first.
pub async fn list_pending(config: &MindConfig, limit: usize) -> Result<Vec<Question>> {
    let client = storage::get_client(config).await?;
    if !client
        .collection_exists(storage::FACTS_COLLECTION)
        .await
        .unwrap_or(false)
    {
        return Ok(Vec::new());
    }
    let filter = Filter::must([
        Condition::matches("valid", "true".to_string()),
        Condition::matches("review", PENDING.to_string()),
    ]);
    let mut pending = Vec::new();
    let mut offset: Option<qdrant_client::qdrant::PointId> = None;
    loop {
        let mut b = ScrollPointsBuilder::new(storage::FACTS_COLLECTION)
            .filter(filter.clone())
            .limit(256)
            .with_payload(true);
        if let Some(o) = offset.clone() {
            b = b.offset(o);
        }
        let r = client.scroll(b).await?;
        for p in &r.result {
            pending.push(crate::knowledge::fact_from_point_pub(p));
        }
        match r.next_page_offset {
            Some(n) => offset = Some(n),
            None => break,
        }
    }
    pending.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    pending.truncate(limit);
    let mut out = Vec::with_capacity(pending.len());
    for f in pending {
        let rivals =
            crate::knowledge::find_facts_by_subject_predicate(config, &f.subject, &f.predicate)
                .await?
                .into_iter()
                .filter(|r| r.id != f.id)
                .collect();
        out.push(Question { fact: f, rivals });
    }
    Ok(out)
}

async fn read_fact(config: &MindConfig, id: &str) -> Result<Fact> {
    let client = storage::get_client(config).await?;
    let point_id: qdrant_client::qdrant::PointId = id.to_string().into();
    let got = client
        .get_points(
            qdrant_client::qdrant::GetPointsBuilder::new(storage::FACTS_COLLECTION, vec![point_id])
                .with_payload(true),
        )
        .await
        .context("reading fact for review")?;
    let p = got
        .result
        .first()
        .ok_or_else(|| anyhow::anyhow!("fact id '{id}' not found"))?;
    let mut f = crate::knowledge::fact_from_point_pub(p);
    // `fact_from_point` assumes a valid row (its readers filter on it); this
    // reader does not, so take the flag from the payload.
    f.valid = !matches!(
        p.payload.get("valid").and_then(|v| v.kind.as_ref()),
        Some(qdrant_client::qdrant::value::Kind::StringValue(s)) if s == "false"
    );
    Ok(f)
}

async fn set_fields(config: &MindConfig, id: &str, fields: &[(&str, String)]) -> Result<()> {
    let client = storage::get_client(config).await?;
    let mut payload: HashMap<String, qdrant_client::qdrant::Value> = HashMap::new();
    for (k, v) in fields {
        payload.insert((*k).to_string(), v.clone().into());
    }
    let point_id: qdrant_client::qdrant::PointId = id.to_string().into();
    client
        .set_payload(
            SetPayloadPointsBuilder::new(storage::FACTS_COLLECTION, payload)
                .points_selector(PointsIdsList {
                    ids: vec![point_id],
                })
                .wait(true),
        )
        .await
        .context("writing review fields")?;
    crate::doubt::record_edit();
    Ok(())
}

fn triple(f: &Fact) -> String {
    format!("{} {} {}", f.subject, f.predicate, f.object)
}

/// An agent puts a question about a fact to a person. The agent may open a
/// question; it can never close one. Refused for a triple a person already
/// rejected (that answer is final) and for a fact that is not live.
pub async fn ask(config: &MindConfig, id: &str, question: &str, actor: &str) -> Result<()> {
    let question = question.trim();
    if question.is_empty() {
        bail!("a review question must not be empty");
    }
    let _xproc = crate::knowledge::lock_facts_cross_process(config).await?;
    let f = read_fact(config, id).await?;
    if !f.valid {
        bail!("fact {id} is invalidated; restore it before asking about it");
    }
    if f.review.as_deref() == Some(REJECTED) {
        bail!("a person already rejected fact {id}; that answer is final for agents");
    }
    if f.status
        .as_deref()
        .and_then(EntryStatus::parse)
        .is_some_and(|s| !s.is_default_visible())
    {
        bail!(
            "fact {id} is retired ({}); ask about the live value instead",
            f.status.as_deref().unwrap_or("?")
        );
    }
    let now = chrono::Utc::now().to_rfc3339();
    set_fields(
        config,
        id,
        &[
            ("review", PENDING.to_string()),
            ("review_question", question.chars().take(500).collect()),
            ("review_asked_by", actor.to_string()),
            ("review_asked_at", now),
        ],
    )
    .await?;
    crate::audit::record(
        crate::audit::AuditEvent::new(crate::audit::AuditOp::ReviewAsk, "_facts", id)
            .actor(actor)
            .after(storage::truncate_for_audit(&triple(&f)))
            .note(storage::truncate_for_audit(question)),
    );
    Ok(())
}

/// A person's answer. Callable only from the two human surfaces (the terminal
/// command and the PIN-checked viewer route); `by` names which one, and lands
/// in the payload and the audit log.
///
/// Accept: the fact becomes the active value with `review = accepted`. On a
/// single-valued axis every other live value is retired and marked rejected,
/// since the person picked this one over them.
/// Reject: the fact is retired and marked rejected; an agent re-writing the
/// same triple is refused from then on.
pub(crate) async fn answer(config: &MindConfig, id: &str, a: Answer, by: &str) -> Result<Fact> {
    let _xproc = crate::knowledge::lock_facts_cross_process(config).await?;
    let f = read_fact(config, id).await?;
    if !f.valid {
        bail!("fact {id} is invalidated; nothing to review");
    }
    let card = crate::knowledge::get_cardinality(config, &f.predicate).await?;
    let now = chrono::Utc::now().to_rfc3339();
    let decided = |verdict: &str| {
        vec![
            ("review", verdict.to_string()),
            ("reviewed_by", by.to_string()),
            ("reviewed_at", now.clone()),
        ]
    };
    match a {
        Answer::Accept => {
            let mut fields = decided(ACCEPTED);
            fields.push(("status", EntryStatus::Active.as_str().to_string()));
            set_fields(config, id, &fields).await?;
            // A value the person accepted holds; clear any retirement stamp.
            let client = storage::get_client(config).await?;
            let point_id: qdrant_client::qdrant::PointId = id.to_string().into();
            client
                .delete_payload(
                    qdrant_client::qdrant::DeletePayloadPointsBuilder::new(
                        storage::FACTS_COLLECTION,
                        vec!["valid_until".to_string()],
                    )
                    .points_selector(PointsIdsList {
                        ids: vec![point_id],
                    })
                    .wait(true),
                )
                .await
                .context("clearing valid_until on accept")?;
            if card.admits_conflict() {
                let rivals = crate::knowledge::find_facts_by_subject_predicate(
                    config,
                    &f.subject,
                    &f.predicate,
                )
                .await?
                .into_iter()
                .filter(|r| r.id != f.id);
                for r in rivals {
                    crate::duel::retire_loser(config, card, &r.id).await?;
                    set_fields(config, &r.id, &decided(REJECTED)).await?;
                    crate::audit::record(
                        crate::audit::AuditEvent::new(
                            crate::audit::AuditOp::ReviewAnswer,
                            "_facts",
                            r.id.clone(),
                        )
                        .actor(by)
                        .before(storage::truncate_for_audit(&triple(&r)))
                        .note(format!("rejected: a person accepted {} instead", f.id)),
                    );
                }
            }
        }
        Answer::Reject => {
            crate::duel::retire_loser(config, card, id).await?;
            set_fields(config, id, &decided(REJECTED)).await?;
        }
    }
    crate::audit::record(
        crate::audit::AuditEvent::new(crate::audit::AuditOp::ReviewAnswer, "_facts", id)
            .actor(by)
            .before(storage::truncate_for_audit(&triple(&f)))
            .note(match a {
                Answer::Accept => "accepted by a person",
                Answer::Reject => "rejected by a person",
            }),
    );
    read_fact(config, id).await
}

/// The tag a reader shows next to a fact, so a contested or still-open value
/// never reads as plain truth. Empty for an ordinary active fact.
pub fn label(status: Option<&str>, review: Option<&str>) -> &'static str {
    match (review, status) {
        (Some(PENDING), _) => "  [disputed: waiting on the user's review]",
        (Some(ACCEPTED), _) => "  [confirmed by the user]",
        (_, Some("contested")) => "  [disputed]",
        _ => "",
    }
}

// ---- the terminal gate ----------------------------------------------------

/// True when both ends of the process are a real terminal. An agent's shell
/// tool runs commands with pipes, so this is false there.
pub fn at_terminal() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

/// Short form of an id the person types back to confirm an answer.
pub fn short_id(id: &str) -> String {
    id.chars().filter(|c| *c != '-').take(6).collect()
}

// ---- the viewer PIN -------------------------------------------------------

fn pin_path(config: &MindConfig) -> std::path::PathBuf {
    config.data_dir.join("review_pin")
}

pub fn pin_is_set(config: &MindConfig) -> bool {
    pin_path(config).exists()
}

/// Store the review PIN as an Argon2id PHC string. Callers must have checked
/// `at_terminal()` and read the PIN with hidden input.
pub fn set_pin(config: &MindConfig, pin: &str) -> Result<()> {
    use argon2::password_hash::{PasswordHasher, SaltString, rand_core::OsRng};
    if pin.chars().count() < 6 {
        bail!("the review PIN must be at least 6 characters");
    }
    let salt = SaltString::generate(&mut OsRng);
    let hash = argon2::Argon2::default()
        .hash_password(pin.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("hashing the review PIN: {e}"))?
        .to_string();
    std::fs::write(pin_path(config), hash).context("writing the review PIN hash")?;
    Ok(())
}

/// Check a PIN against the stored hash. False when no PIN is set.
pub fn verify_pin(config: &MindConfig, pin: &str) -> bool {
    use argon2::password_hash::{PasswordHash, PasswordVerifier};
    let Ok(stored) = std::fs::read_to_string(pin_path(config)) else {
        return false;
    };
    let Ok(parsed) = PasswordHash::new(stored.trim()) else {
        return false;
    };
    argon2::Argon2::default()
        .verify_password(pin.as_bytes(), &parsed)
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holds_only_open_or_accepted() {
        assert!(holds(Some(PENDING)));
        assert!(holds(Some(ACCEPTED)));
        assert!(!holds(Some(REJECTED)));
        assert!(!holds(None));
    }

    #[test]
    fn short_id_drops_dashes() {
        assert_eq!(short_id("3e01b364-e383-5a9c"), "3e01b3");
    }

    #[test]
    fn pin_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let config = MindConfig {
            data_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        assert!(!verify_pin(&config, "123456"));
        assert!(set_pin(&config, "12345").is_err(), "too short");
        set_pin(&config, "123456").unwrap();
        assert!(verify_pin(&config, "123456"));
        assert!(!verify_pin(&config, "123457"));
        let stored = std::fs::read_to_string(pin_path(&config)).unwrap();
        assert!(!stored.contains("123456"), "only the hash is stored");
    }
}
