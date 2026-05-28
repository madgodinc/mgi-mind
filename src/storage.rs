use anyhow::{Context, Result};
use qdrant_client::qdrant::{
    CreateCollectionBuilder, Distance, PointStruct, SearchPointsBuilder,
    VectorParamsBuilder, DeleteCollectionBuilder, UpsertPointsBuilder,
    DeletePointsBuilder, PointsIdsList, ScrollPointsBuilder,
};
use qdrant_client::Qdrant;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

use crate::config::MindConfig;
use crate::embedder;

const VECTOR_SIZE: u64 = 384;
const MEMORIES_PREFIX: &str = "mem_";
pub const FACTS_COLLECTION: &str = "_kg_facts";

#[derive(Debug, Serialize, Deserialize)]
pub struct SearchResult {
    pub id: String,
    pub library: String,
    pub content: String,
    pub source: Option<String>,
    pub score: f32,
}

pub async fn get_client(config: &MindConfig) -> Result<Qdrant> {
    let url = format!("http://localhost:{}", config.qdrant_port);
    let client = Qdrant::from_url(&url)
        .build()
        .context("Failed to connect to Qdrant")?;
    Ok(client)
}

fn collection_name(library: &str) -> String {
    format!("{MEMORIES_PREFIX}{library}")
}

fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{truncated}...")
    }
}

fn format_point_id(pid: &qdrant_client::qdrant::PointId) -> String {
    use qdrant_client::qdrant::point_id::PointIdOptions;
    match &pid.point_id_options {
        Some(PointIdOptions::Uuid(u)) => u.clone(),
        Some(PointIdOptions::Num(n)) => n.to_string(),
        None => "unknown".to_string(),
    }
}

pub fn extract_string_pub(payload: &HashMap<String, qdrant_client::qdrant::Value>, key: &str) -> Option<String> {
    extract_string(payload, key)
}

fn extract_string(payload: &HashMap<String, qdrant_client::qdrant::Value>, key: &str) -> Option<String> {
    payload.get(key).and_then(|v| {
        if let Some(qdrant_client::qdrant::value::Kind::StringValue(s)) = &v.kind {
            Some(s.clone())
        } else {
            None
        }
    })
}

pub async fn init(config: &MindConfig) -> Result<()> {
    let client = get_client(config).await?;

    let exists = client.collection_exists(FACTS_COLLECTION).await.unwrap_or(false);
    if !exists {
        client
            .create_collection(
                CreateCollectionBuilder::new(FACTS_COLLECTION)
                    .vectors_config(VectorParamsBuilder::new(VECTOR_SIZE, Distance::Cosine)),
            )
            .await
            .context("Failed to create facts collection")?;
    }

    Ok(())
}

pub async fn create_library(config: &MindConfig, name: &str) -> Result<()> {
    let client = get_client(config).await?;
    let col = collection_name(name);

    let exists = client.collection_exists(&col).await.unwrap_or(false);
    if exists {
        anyhow::bail!("{}", crate::error::MindError::LibraryExists(name.to_string()));
    }

    client
        .create_collection(
            CreateCollectionBuilder::new(&col)
                .vectors_config(VectorParamsBuilder::new(VECTOR_SIZE, Distance::Cosine)),
        )
        .await
        .context("Failed to create library collection")?;

    Ok(())
}

pub async fn drop_library(config: &MindConfig, name: &str) -> Result<()> {
    let client = get_client(config).await?;
    let col = collection_name(name);

    client
        .delete_collection(DeleteCollectionBuilder::new(&col))
        .await
        .context("Failed to delete library")?;

    Ok(())
}

pub async fn list_libraries(config: &MindConfig) -> Result<Vec<String>> {
    let client = get_client(config).await?;
    let collections = client.list_collections().await?;

    let libraries: Vec<String> = collections
        .collections
        .iter()
        .filter_map(|c| {
            c.name
                .strip_prefix(MEMORIES_PREFIX)
                .map(|s| s.to_string())
        })
        .collect();

    Ok(libraries)
}

pub async fn ensure_facts_collection(client: &Qdrant) -> Result<()> {
    let exists = client.collection_exists(FACTS_COLLECTION).await.unwrap_or(false);
    if !exists {
        client
            .create_collection(
                CreateCollectionBuilder::new(FACTS_COLLECTION)
                    .vectors_config(VectorParamsBuilder::new(VECTOR_SIZE, Distance::Cosine)),
            )
            .await
            .context("Failed to create facts collection")?;
    }
    Ok(())
}

pub async fn add_memory(
    config: &MindConfig,
    library: &str,
    content: &str,
    source: Option<&str>,
) -> Result<String> {
    let client = get_client(config).await?;
    let col = collection_name(library);

    if !client.collection_exists(&col).await.unwrap_or(false) {
        anyhow::bail!("{}", crate::error::MindError::LibraryNotFound(library.to_string()));
    }

    // Deduplication: check if content hash already exists
    let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
    let embedding = embedder::embed(config, content).await?;

    // Search for near-exact match by embedding (score > 0.99 = duplicate)
    let dup_check = client
        .search_points(
            SearchPointsBuilder::new(&col, embedding.clone(), 1).with_payload(true),
        )
        .await;

    if let Ok(response) = dup_check {
        if let Some(point) = response.result.first() {
            if point.score > 0.99 {
                let existing_hash = extract_string(&point.payload, "hash").unwrap_or_default();
                if existing_hash == hash {
                    let existing_id = point.id.as_ref().map(|id| format!("{id:?}")).unwrap_or_default();
                    anyhow::bail!("Duplicate content already exists [id: {existing_id}]");
                }
            }
        }
    }

    let id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    let mut payload: HashMap<String, qdrant_client::qdrant::Value> = HashMap::new();
    payload.insert("content".into(), content.into());
    payload.insert("hash".into(), hash.into());
    payload.insert("created_at".into(), now.into());
    payload.insert("library".into(), library.into());
    if let Some(src) = source {
        payload.insert("source".into(), src.into());
    }

    let point = PointStruct::new(id.clone(), embedding, payload);

    client
        .upsert_points(UpsertPointsBuilder::new(&col, vec![point]).wait(true))
        .await
        .context("Failed to add memory")?;

    Ok(id)
}

pub async fn search(
    config: &MindConfig,
    query: &str,
    library: Option<&str>,
    limit: usize,
    tier: u8,
) -> Result<Vec<SearchResult>> {
    let client = get_client(config).await?;
    let embedding = embedder::embed(config, query).await?;

    let collections: Vec<String> = if let Some(lib) = library {
        vec![collection_name(lib)]
    } else {
        let all = client.list_collections().await?;
        all.collections
            .iter()
            .filter(|c| c.name.starts_with(MEMORIES_PREFIX))
            .map(|c| c.name.clone())
            .collect()
    };

    let mut results = Vec::new();

    for col in &collections {
        let lib_name = col.strip_prefix(MEMORIES_PREFIX).unwrap_or(col);

        let search_result = client
            .search_points(
                SearchPointsBuilder::new(col, embedding.clone(), limit as u64).with_payload(true),
            )
            .await;

        if let Ok(response) = search_result {
            for point in response.result {
                let payload = &point.payload;

                let content = extract_string(payload, "content").unwrap_or_default();
                let source = extract_string(payload, "source");

                let display_content = match tier {
                    1 => truncate_str(&content, 100),
                    2 => truncate_str(&content, 500),
                    _ => content.clone(),
                };

                let id = match &point.id {
                    Some(pid) => format_point_id(pid),
                    None => "unknown".to_string(),
                };

                results.push(SearchResult {
                    id,
                    library: lib_name.to_string(),
                    content: display_content,
                    source,
                    score: point.score,
                });
            }
        }
    }

    results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    results.truncate(limit);

    Ok(results)
}

pub async fn delete_memory(config: &MindConfig, library: &str, id: &str) -> Result<()> {
    let client = get_client(config).await?;
    let col = collection_name(library);

    if !client.collection_exists(&col).await.unwrap_or(false) {
        anyhow::bail!("{}", crate::error::MindError::LibraryNotFound(library.to_string()));
    }

    let point_id: qdrant_client::qdrant::PointId = id.to_string().into();

    client
        .delete_points(
            DeletePointsBuilder::new(&col)
                .points(PointsIdsList {
                    ids: vec![point_id],
                })
                .wait(true),
        )
        .await
        .context("Failed to delete memory")?;

    Ok(())
}

pub async fn history(config: &MindConfig, limit: usize) -> Result<Vec<SearchResult>> {
    let client = get_client(config).await?;
    let collections = client.list_collections().await?;

    let mut all_results = Vec::new();

    for col in &collections.collections {
        if let Some(lib_name) = col.name.strip_prefix(MEMORIES_PREFIX) {
            let scroll = client
                .scroll(ScrollPointsBuilder::new(&col.name).limit(limit as u32).with_payload(true))
                .await;

            if let Ok(response) = scroll {
                for point in response.result {
                    let payload = &point.payload;
                    let content = extract_string(payload, "content").unwrap_or_default();
                    let source = extract_string(payload, "source");
                    let _created = extract_string(payload, "created_at").unwrap_or_default();

                    let id = match &point.id {
                        Some(pid) => format_point_id(pid),
                        None => "unknown".to_string(),
                    };

                    all_results.push(SearchResult {
                        id,
                        library: lib_name.to_string(),
                        content: truncate_str(&content, 200),
                        source,
                        score: 0.0, // no score for scroll
                    });
                }
            }
        }
    }

    // Sort by newest first (entries have created_at in payload but we truncated it)
    // For now just take last N
    all_results.truncate(limit);
    Ok(all_results)
}

pub async fn export_all(config: &MindConfig, format: &str, output_dir: &str) -> Result<usize> {
    let client = get_client(config).await?;
    let collections = client.list_collections().await?;

    std::fs::create_dir_all(output_dir)?;

    let mut total = 0;

    for col in &collections.collections {
        let lib_name = col.name.strip_prefix(MEMORIES_PREFIX).unwrap_or(&col.name);

        let scroll = client
            .scroll(ScrollPointsBuilder::new(&col.name).limit(10000).with_payload(true))
            .await;

        if let Ok(response) = scroll {
            let entries: Vec<serde_json::Value> = response
                .result
                .iter()
                .filter_map(|point| {
                    let payload = &point.payload;
                    let content = extract_string(payload, "content")?;
                    let source = extract_string(payload, "source");
                    let created = extract_string(payload, "created_at");
                    let id = point.id.as_ref().map(format_point_id);

                    Some(serde_json::json!({
                        "id": id,
                        "content": content,
                        "source": source,
                        "created_at": created,
                    }))
                })
                .collect();

            total += entries.len();

            let file_path = format!("{output_dir}/{lib_name}.{format}");

            match format {
                "json" => {
                    let json = serde_json::to_string_pretty(&entries)?;
                    std::fs::write(&file_path, json)?;
                }
                "md" => {
                    let mut md = format!("# {lib_name}\n\n");
                    for entry in &entries {
                        if let Some(content) = entry.get("content").and_then(|v| v.as_str()) {
                            md.push_str(&format!("---\n\n{content}\n\n"));
                            if let Some(src) = entry.get("source").and_then(|v| v.as_str()) {
                                md.push_str(&format!("*source: {src}*\n\n"));
                            }
                        }
                    }
                    std::fs::write(&file_path, md)?;
                }
                _ => anyhow::bail!("Unsupported format: {format}. Use json or md"),
            }
        }
    }

    Ok(total)
}

pub async fn stats(config: &MindConfig) -> Result<(Vec<(String, u64)>, u64)> {
    let client = get_client(config).await?;
    let collections = client.list_collections().await?;

    let mut libraries = Vec::new();
    let mut facts_count = 0u64;

    for col in &collections.collections {
        let info = client.collection_info(&col.name).await;
        let count = info
            .map(|i| i.result.map(|r| r.points_count.unwrap_or(0)).unwrap_or(0))
            .unwrap_or(0);

        if col.name == FACTS_COLLECTION {
            facts_count = count;
        } else if let Some(lib_name) = col.name.strip_prefix(MEMORIES_PREFIX) {
            libraries.push((lib_name.to_string(), count));
        }
    }

    Ok((libraries, facts_count))
}

pub fn backup(output: &str) -> Result<()> {
    let home = crate::config::mind_home();

    let status = std::process::Command::new("tar")
        .args(["-czf", output, "-C", &home.to_string_lossy(), "."])
        .status()
        .context("Failed to run tar for backup")?;

    if !status.success() {
        anyhow::bail!("Backup failed with exit code: {:?}", status.code());
    }

    Ok(())
}

pub fn restore(input: &str) -> Result<()> {
    let home = crate::config::mind_home();
    std::fs::create_dir_all(&home)?;

    let status = std::process::Command::new("tar")
        .args(["-xzf", input, "-C", &home.to_string_lossy()])
        .status()
        .context("Failed to run tar for restore")?;

    if !status.success() {
        anyhow::bail!("Restore failed with exit code: {:?}", status.code());
    }

    Ok(())
}
