use anyhow::{Context, Result};
use qdrant_client::qdrant::{
    Condition, Filter, NamedVectors, PointStruct, PointsIdsList, ScrollPointsBuilder,
    SetPayloadPointsBuilder, UpsertPointsBuilder,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

use crate::config::MindConfig;
use crate::storage;

/// Namespace for deterministic fact IDs - dedup by (subject, predicate, object) (audit #13).
const FACT_NAMESPACE: Uuid = Uuid::from_u128(0x6d676900_6661_6374_0000_000000000001);

/// Predicate cardinality — load-bearing for the v1.4 duel rule.
///
/// Without cardinality, every `(subject, predicate)` pair with two distinct
/// objects would look like a conflict — even when both objects are honestly
/// true at the same time ("uses Rust" + "uses Go"). Cardinality is the axis
/// that decides whether a second value contradicts or coexists.
///
/// - `Single`: at most one current object per subject. `primary_language`,
///   `birth_year`, `current_project`. Two values → conflict.
/// - `TemporalSingle`: single at any moment, but historically a sequence.
///   Same conflict semantics as `Single` for the live value; the old value
///   is dampened with a `valid_until`, not deleted. This is the natural
///   pair for bi-temporal axes (v1.4 §4).
/// - `Multi`: many objects allowed simultaneously. `uses_language`,
///   `worked_at`, `speaks`. Two values → both kept, no conflict.
///
/// Unknown predicates default to `Multi` — better to keep both honest facts
/// than to start a false duel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Cardinality {
    Single,
    TemporalSingle,
    Multi,
}

impl Default for Cardinality {
    fn default() -> Self {
        Cardinality::Multi
    }
}

impl Cardinality {
    /// Parse from a lowercase MCP / config string. Returns `None` for unknown
    /// values so the caller can decide (warning + Multi fallback, hard error,
    /// etc.) instead of silently picking a wrong default.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "single" => Some(Cardinality::Single),
            "temporal-single" | "temporal_single" | "temporalsingle" => {
                Some(Cardinality::TemporalSingle)
            }
            "multi" => Some(Cardinality::Multi),
            _ => None,
        }
    }

    /// Does a second value along this predicate constitute a conflict with the
    /// existing one? True for `Single` and `TemporalSingle`, false for `Multi`.
    pub fn admits_conflict(self) -> bool {
        !matches!(self, Cardinality::Multi)
    }
}

/// Pure (no Qdrant) conflict detector for the duel rule.
///
/// Inputs: the facts already on record for this `(subject, predicate)` pair
/// and the new object being proposed.
/// Output: whether the new object contradicts any *valid* existing object.
///
/// Multi predicates never conflict. Single / TemporalSingle conflict when a
/// new object differs from any existing valid object; re-asserting the same
/// object is idempotent, not a conflict.
///
/// Phase 0 primitive — wired into `add_fact` in Phase 0 step 1.3 and into the
/// duel rule in Phase 2. Allowed-dead until then so the v1.4 schema lands
/// without warnings, in one bisectable commit.
#[allow(dead_code)]
pub fn detect_conflict(
    existing: &[Fact],
    new_object: &str,
    cardinality: Cardinality,
) -> bool {
    if !cardinality.admits_conflict() {
        return false;
    }
    existing
        .iter()
        .any(|f| f.valid && f.object != new_object)
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Fact {
    pub id: String,
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub created_at: Option<String>,
    pub valid: bool,
}

fn extract_string(
    payload: &HashMap<String, qdrant_client::qdrant::Value>,
    key: &str,
) -> Option<String> {
    payload.get(key).and_then(|v| {
        if let Some(qdrant_client::qdrant::value::Kind::StringValue(s)) = &v.kind {
            Some(s.clone())
        } else {
            None
        }
    })
}

fn fact_id(subject: &str, predicate: &str, object: &str) -> String {
    let key = format!("{subject}\u{0}{predicate}\u{0}{object}");
    Uuid::new_v5(&FACT_NAMESPACE, key.as_bytes()).to_string()
}

pub async fn add_fact(
    config: &MindConfig,
    subject: &str,
    predicate: &str,
    object: &str,
) -> Result<String> {
    let client = storage::get_client(config).await?;
    storage::ensure_facts_collection(&client).await?;

    // Facts are vectorless (audit #6): they're looked up by exact/lexical payload
    // match, never by vector, so we no longer pay an embedding inference or store
    // a dead 768-dim vector per fact.

    // Deterministic ID dedups identical triples (audit #13): re-adding the same
    // (s,p,o) overwrites instead of piling up duplicates.
    let id = fact_id(subject, predicate, object);
    let now = chrono::Utc::now().to_rfc3339();
    // Keep the original created_at on re-add; only updated_at moves.
    let created_at =
        storage::existing_payload_string(&client, storage::FACTS_COLLECTION, &id, "created_at")
            .await
            .unwrap_or_else(|| now.clone());

    let mut payload: HashMap<String, qdrant_client::qdrant::Value> = HashMap::new();
    payload.insert("subject".into(), subject.into());
    payload.insert("predicate".into(), predicate.into());
    payload.insert("object".into(), object.into());
    payload.insert("created_at".into(), created_at.into());
    payload.insert("updated_at".into(), now.into());
    payload.insert("valid".into(), "true".into());
    payload.insert("type".into(), "fact".into());

    // Payload-only point (NamedVectors::default() is empty - no vector stored).
    let point = PointStruct::new(id.clone(), NamedVectors::default(), payload);

    client
        .upsert_points(UpsertPointsBuilder::new(storage::FACTS_COLLECTION, vec![point]).wait(true))
        .await
        .context("Failed to add fact")?;

    Ok(id)
}

/// Query facts whose subject, predicate, OR object matches the term, filtered
/// SERVER-SIDE (audit #6): `valid = true` AND a full-text match on any of the
/// three fields. Qdrant returns only the matching facts (using the payload
/// indexes), instead of scrolling the whole collection into RAM and filtering
/// in process. Matching is full-text (tokenized) rather than raw substring.
pub async fn query_facts(config: &MindConfig, query: &str) -> Result<Vec<Fact>> {
    let client = storage::get_client(config).await?;
    if !client
        .collection_exists(storage::FACTS_COLLECTION)
        .await
        .unwrap_or(false)
    {
        return Ok(Vec::new());
    }

    // valid = true AND (subject ~ q OR predicate ~ q OR object ~ q).
    let filter = Filter {
        must: vec![Condition::matches("valid", "true".to_string())],
        should: vec![
            Condition::matches_text("subject", query),
            Condition::matches_text("predicate", query),
            Condition::matches_text("object", query),
        ],
        ..Default::default()
    };

    let mut facts = Vec::new();
    let mut offset: Option<qdrant_client::qdrant::PointId> = None;
    loop {
        let mut builder = ScrollPointsBuilder::new(storage::FACTS_COLLECTION)
            .filter(filter.clone())
            .limit(256)
            .with_payload(true);
        if let Some(o) = offset.clone() {
            builder = builder.offset(o);
        }

        let response = client.scroll(builder).await?;
        for point in &response.result {
            let p = &point.payload;
            let id = point
                .id
                .as_ref()
                .map(|pid| {
                    use qdrant_client::qdrant::point_id::PointIdOptions;
                    match &pid.point_id_options {
                        Some(PointIdOptions::Uuid(u)) => u.clone(),
                        Some(PointIdOptions::Num(n)) => n.to_string(),
                        None => "unknown".to_string(),
                    }
                })
                .unwrap_or_default();

            facts.push(Fact {
                id,
                subject: extract_string(p, "subject").unwrap_or_default(),
                predicate: extract_string(p, "predicate").unwrap_or_default(),
                object: extract_string(p, "object").unwrap_or_default(),
                created_at: extract_string(p, "created_at"),
                valid: true,
            });
        }

        match response.next_page_offset {
            Some(next) => offset = Some(next),
            None => break,
        }
    }

    Ok(facts)
}

/// Soft-delete: mark a fact `valid = false` instead of physically removing it,
/// so the temporal-validity flag is actually honored (audit #13).
pub async fn invalidate_fact(config: &MindConfig, id: &str) -> Result<()> {
    let client = storage::get_client(config).await?;

    let point_id: qdrant_client::qdrant::PointId = id.to_string().into();

    let mut payload: HashMap<String, qdrant_client::qdrant::Value> = HashMap::new();
    payload.insert("valid".into(), "false".into());

    client
        .set_payload(
            SetPayloadPointsBuilder::new(storage::FACTS_COLLECTION, payload)
                .points_selector(PointsIdsList {
                    ids: vec![point_id],
                })
                .wait(true),
        )
        .await
        .context("Failed to invalidate fact")?;

    Ok(())
}

// ===== v1.4 Phase 0: cardinality + conflict detector =====
//
// Pure tests for the load-bearing primitives. No Qdrant, no async, no
// embedder — every assertion runs in microseconds and the whole module
// completes in well under 10ms. These tests are the spec the duel rule
// in Phase 2 will be calibrated against; if any of them fails or has
// to be edited later, the duel rule was built against the wrong axis.
#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build a `valid = true` fact with a fixed subject/predicate so
    /// the cases below differ only on `object`. The conflict detector is
    /// concerned with object disagreement, not identity rewriting.
    fn rec(object: &str) -> Fact {
        Fact {
            id: format!("test-{object}"),
            subject: "Mad".into(),
            predicate: "primary_language".into(),
            object: object.into(),
            created_at: None,
            valid: true,
        }
    }

    #[test]
    fn parse_round_trips_known_variants() {
        assert_eq!(Cardinality::parse("single"), Some(Cardinality::Single));
        assert_eq!(
            Cardinality::parse("temporal-single"),
            Some(Cardinality::TemporalSingle)
        );
        assert_eq!(
            Cardinality::parse("temporal_single"),
            Some(Cardinality::TemporalSingle)
        );
        assert_eq!(Cardinality::parse("multi"), Some(Cardinality::Multi));
    }

    #[test]
    fn parse_is_case_and_whitespace_tolerant() {
        assert_eq!(Cardinality::parse("  SINGLE  "), Some(Cardinality::Single));
        assert_eq!(Cardinality::parse("Multi"), Some(Cardinality::Multi));
    }

    #[test]
    fn parse_returns_none_for_unknown() {
        // None — not silent fallback — so the caller logs the unknown value
        // and chooses (multi fallback, hard error, etc.) explicitly.
        assert_eq!(Cardinality::parse("kinda-single"), None);
        assert_eq!(Cardinality::parse(""), None);
    }

    #[test]
    fn default_is_multi() {
        // The roadmap-loadbearing default. Unknown predicates must not start
        // duels — better to keep both honest facts than to fire a false
        // conflict. If this ever changes the v1.4 §4 contract breaks.
        assert_eq!(Cardinality::default(), Cardinality::Multi);
    }

    #[test]
    fn multi_predicate_never_admits_conflict() {
        assert!(!Cardinality::Multi.admits_conflict());
        let existing = vec![rec("Rust")];
        assert!(!detect_conflict(&existing, "Go", Cardinality::Multi));
    }

    #[test]
    fn single_predicate_admits_conflict_on_distinct_object() {
        assert!(Cardinality::Single.admits_conflict());
        let existing = vec![rec("Rust")];
        assert!(detect_conflict(&existing, "Go", Cardinality::Single));
    }

    #[test]
    fn temporal_single_admits_conflict_too() {
        // TemporalSingle and Single share the live-value semantics; the
        // difference is only in how the loser is dampened (Phase 2 step 2.3),
        // not in whether a conflict event fires here.
        assert!(Cardinality::TemporalSingle.admits_conflict());
        let existing = vec![rec("Rust")];
        assert!(detect_conflict(
            &existing,
            "Go",
            Cardinality::TemporalSingle
        ));
    }

    #[test]
    fn re_asserting_same_object_is_idempotent() {
        // The duel rule only fires on disagreement. Re-stating "Rust" should
        // not look like a conflict, even under Single cardinality.
        let existing = vec![rec("Rust")];
        assert!(!detect_conflict(&existing, "Rust", Cardinality::Single));
    }

    #[test]
    fn invalidated_existing_facts_dont_trigger_conflict() {
        // A `valid = false` fact is dampened or invalidated; the new value
        // does not duel against it. This is what lets the loser of a past
        // duel keep its audit trace without poisoning future writes.
        let mut existing = rec("Rust");
        existing.valid = false;
        assert!(!detect_conflict(
            &[existing],
            "Go",
            Cardinality::Single
        ));
    }

    #[test]
    fn empty_existing_set_is_never_a_conflict() {
        // First write of any kind is `New`, not a duel.
        assert!(!detect_conflict(&[], "Go", Cardinality::Single));
        assert!(!detect_conflict(&[], "Go", Cardinality::Multi));
    }
}
