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
