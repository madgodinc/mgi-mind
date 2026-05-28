use anyhow::{Context, Result};
use qdrant_client::qdrant::{PointStruct, SearchPointsBuilder, UpsertPointsBuilder};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

use crate::config::MindConfig;
use crate::embedder;
use crate::storage;

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

pub async fn add_fact(
    config: &MindConfig,
    subject: &str,
    predicate: &str,
    object: &str,
) -> Result<String> {
    let client = storage::get_client(config).await?;

    // Ensure facts collection exists
    storage::ensure_facts_collection(&client).await?;

    let text = format!("{subject} {predicate} {object}");
    let embedding = embedder::embed(config, &text).await?;

    let id = Uuid::new_v4().to_string();
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
        .upsert_points(
            UpsertPointsBuilder::new(storage::FACTS_COLLECTION, vec![point]).wait(true),
        )
        .await
        .context("Failed to add fact")?;

    Ok(id)
}

pub async fn query_facts(config: &MindConfig, subject: &str) -> Result<Vec<Fact>> {
    let client = storage::get_client(config).await?;

    let embedding = embedder::embed(config, subject).await?;

    let results = client
        .search_points(
            SearchPointsBuilder::new(storage::FACTS_COLLECTION, embedding, 20)
                .with_payload(true),
        )
        .await
        .context("Failed to query facts")?;

    let mut facts = Vec::new();

    for point in results.result {
        let p = &point.payload;

        let fact_subject = extract_string(p, "subject").unwrap_or_default();
        let valid = extract_string(p, "valid").unwrap_or_default();

        if valid != "true" {
            continue;
        }

        if !fact_subject.to_lowercase().contains(&subject.to_lowercase()) {
            continue;
        }

        facts.push(Fact {
            id: point.id.as_ref().map(|pid| {
                use qdrant_client::qdrant::point_id::PointIdOptions;
                match &pid.point_id_options {
                    Some(PointIdOptions::Uuid(u)) => u.clone(),
                    Some(PointIdOptions::Num(n)) => n.to_string(),
                    None => "unknown".to_string(),
                }
            }).unwrap_or_default(),
            subject: fact_subject,
            predicate: extract_string(p, "predicate").unwrap_or_default(),
            object: extract_string(p, "object").unwrap_or_default(),
            created_at: extract_string(p, "created_at"),
            valid: true,
        });
    }

    Ok(facts)
}

pub async fn invalidate_fact(config: &MindConfig, id: &str) -> Result<()> {
    let client = storage::get_client(config).await?;

    let point_id: qdrant_client::qdrant::PointId = id.to_string().into();

    client
        .delete_points(
            qdrant_client::qdrant::DeletePointsBuilder::new(storage::FACTS_COLLECTION)
                .points(qdrant_client::qdrant::PointsIdsList {
                    ids: vec![point_id],
                })
                .wait(true),
        )
        .await
        .context("Failed to invalidate fact")?;

    Ok(())
}
