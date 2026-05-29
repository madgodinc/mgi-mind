use anyhow::{Context, Result};
use qdrant_client::qdrant::{
    PointStruct, PointsIdsList, SetPayloadPointsBuilder, UpsertPointsBuilder,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

use crate::config::MindConfig;
use crate::embedder;
use crate::storage;

/// Namespace for deterministic fact IDs — dedup by (subject, predicate, object) (audit #13).
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
    storage::ensure_facts_collection(&client, config.vector_size).await?;

    let text = format!("{subject} {predicate} {object}");
    let embedding = embedder::embed(config, &text).await?;

    // Deterministic ID dedups identical triples (audit #13): re-adding the same
    // (s,p,o) overwrites instead of piling up duplicates.
    let id = fact_id(subject, predicate, object);
    let now = chrono::Utc::now().to_rfc3339();

    let mut payload: HashMap<String, qdrant_client::qdrant::Value> = HashMap::new();
    payload.insert("subject".into(), subject.into());
    payload.insert("predicate".into(), predicate.into());
    payload.insert("object".into(), object.into());
    payload.insert("created_at".into(), now.into());
    payload.insert("valid".into(), "true".into());
    payload.insert("type".into(), "fact".into());

    let point = PointStruct::new(id.clone(), embedding, payload);

    client
        .upsert_points(UpsertPointsBuilder::new(storage::FACTS_COLLECTION, vec![point]).wait(true))
        .await
        .context("Failed to add fact")?;

    Ok(id)
}

/// Query facts by matching the term against subject, predicate, OR object —
/// via a full scroll + filter, so nothing is silently dropped outside a vector
/// top-K window (audit #12). Only valid (non-invalidated) facts are returned.
pub async fn query_facts(config: &MindConfig, query: &str) -> Result<Vec<Fact>> {
    let client = storage::get_client(config).await?;
    if !client
        .collection_exists(storage::FACTS_COLLECTION)
        .await
        .unwrap_or(false)
    {
        return Ok(Vec::new());
    }

    let points = storage::scroll_all(&client, storage::FACTS_COLLECTION).await?;
    let needle = query.to_lowercase();
    let mut facts = Vec::new();

    for point in points {
        let p = &point.payload;

        if extract_string(p, "valid").as_deref() != Some("true") {
            continue;
        }

        let subject = extract_string(p, "subject").unwrap_or_default();
        let predicate = extract_string(p, "predicate").unwrap_or_default();
        let object = extract_string(p, "object").unwrap_or_default();

        let matches = subject.to_lowercase().contains(&needle)
            || predicate.to_lowercase().contains(&needle)
            || object.to_lowercase().contains(&needle);
        if !matches {
            continue;
        }

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
            subject,
            predicate,
            object,
            created_at: extract_string(p, "created_at"),
            valid: true,
        });
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
