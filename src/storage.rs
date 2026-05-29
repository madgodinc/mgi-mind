use anyhow::{Context, Result};
use qdrant_client::Qdrant;
use qdrant_client::qdrant::{
    CreateCollectionBuilder, DeleteCollectionBuilder, DeletePointsBuilder, Distance,
    GetPointsBuilder, PointStruct, PointsIdsList, ScrollPointsBuilder, SearchPointsBuilder,
    UpsertPointsBuilder, VectorParamsBuilder,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

use crate::config::MindConfig;
use crate::embedder;

const MEMORIES_PREFIX: &str = "mem_";
pub const FACTS_COLLECTION: &str = "_kg_facts";

/// Fixed namespace for deterministic content IDs (audit #15).
/// UUIDv5(namespace, library + content) → identical content yields the same
/// point ID, so re-adding is an idempotent upsert (no duplicates, no TOCTOU race).
const MGI_NAMESPACE: Uuid = Uuid::from_u128(0x6d676900_6d69_6e64_0000_000000000001);

/// How many points to pull per scroll page (export/history pagination, audit #10).
const SCROLL_PAGE: u32 = 256;

#[derive(Debug, Serialize, Deserialize)]
pub struct SearchResult {
    pub id: String,
    pub library: String,
    pub content: String,
    pub source: Option<String>,
    pub created_at: Option<String>,
    pub score: f32,
}

pub async fn get_client(config: &MindConfig) -> Result<Qdrant> {
    let url = format!("http://localhost:{}", config.qdrant_port);
    let mut builder = Qdrant::from_url(&url);
    // Authenticate when an API key is configured (audit #7).
    if let Some(key) = &config.qdrant_api_key {
        builder = builder.api_key(key.clone());
    }
    let client = builder.build().context("Failed to connect to Qdrant")?;
    Ok(client)
}

fn collection_name(library: &str) -> String {
    format!("{MEMORIES_PREFIX}{library}")
}

fn deterministic_id(library: &str, content: &str) -> String {
    let key = format!("{library}\u{0}{content}");
    Uuid::new_v5(&MGI_NAMESPACE, key.as_bytes()).to_string()
}

fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let truncated: String = s.chars().take(max_chars).collect();
    // Break at the last word boundary instead of slicing mid-token (audit #24).
    let cut = match truncated.rfind(char::is_whitespace) {
        Some(i) if i >= max_chars / 2 => i,
        _ => truncated.len(),
    };
    format!("{}...", truncated[..cut].trim_end())
}

fn format_point_id(pid: &qdrant_client::qdrant::PointId) -> String {
    use qdrant_client::qdrant::point_id::PointIdOptions;
    match &pid.point_id_options {
        Some(PointIdOptions::Uuid(u)) => u.clone(),
        Some(PointIdOptions::Num(n)) => n.to_string(),
        None => "unknown".to_string(),
    }
}

pub fn extract_string_pub(
    payload: &HashMap<String, qdrant_client::qdrant::Value>,
    key: &str,
) -> Option<String> {
    extract_string(payload, key)
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

async fn create_vector_collection(client: &Qdrant, name: &str, dim: u64) -> Result<()> {
    client
        .create_collection(
            CreateCollectionBuilder::new(name)
                .vectors_config(VectorParamsBuilder::new(dim, Distance::Cosine)),
        )
        .await
        .with_context(|| format!("Failed to create collection {name}"))?;
    Ok(())
}

pub async fn init(config: &MindConfig) -> Result<()> {
    let client = get_client(config).await?;
    ensure_facts_collection(&client, config.vector_size).await?;
    Ok(())
}

pub async fn create_library(config: &MindConfig, name: &str) -> Result<()> {
    let client = get_client(config).await?;
    let col = collection_name(name);

    if client.collection_exists(&col).await.unwrap_or(false) {
        anyhow::bail!(
            "{}",
            crate::error::MindError::LibraryExists(name.to_string())
        );
    }

    create_vector_collection(&client, &col, config.vector_size).await?;
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
        .filter_map(|c| c.name.strip_prefix(MEMORIES_PREFIX).map(|s| s.to_string()))
        .collect();

    Ok(libraries)
}

pub async fn ensure_facts_collection(client: &Qdrant, dim: u64) -> Result<()> {
    if !client
        .collection_exists(FACTS_COLLECTION)
        .await
        .unwrap_or(false)
    {
        create_vector_collection(client, FACTS_COLLECTION, dim).await?;
    }
    Ok(())
}

/// Validate that an embedding matches the configured dimension (audit #11).
pub(crate) fn check_dim(embedding: &[f32], config: &MindConfig) -> Result<()> {
    if embedding.len() as u64 != config.vector_size {
        anyhow::bail!(
            "Embedding dimension {} does not match configured vector_size {} \
             (model '{}' may have changed — run a reindex)",
            embedding.len(),
            config.vector_size,
            config.model_name
        );
    }
    Ok(())
}

/// Read the configured vector dimension of an existing collection, if it can be
/// determined. Returns `None` for named-vector layouts or any shape we can't
/// confidently parse — callers treat `None` as "unknown", never as a mismatch.
fn collection_dim(info: &qdrant_client::qdrant::CollectionInfo) -> Option<u64> {
    use qdrant_client::qdrant::vectors_config::Config;
    let vc = info
        .config
        .as_ref()?
        .params
        .as_ref()?
        .vectors_config
        .as_ref()?;
    match vc.config.as_ref()? {
        Config::Params(p) => Some(p.size),
        Config::ParamsMap(_) => None,
    }
}

/// Best-effort check that every memory/fact collection's on-disk vector
/// dimension matches `config.vector_size` (audit #11). A mismatch means the
/// embedding model changed without a reindex — upserts would fail with a raw
/// Qdrant error. Returns `(collection, actual_dim)` for each disagreement;
/// collections whose dimension can't be parsed are skipped, never falsely flagged.
pub async fn dimension_mismatches(config: &MindConfig) -> Result<Vec<(String, u64)>> {
    let client = get_client(config).await?;
    let collections = client.list_collections().await?;
    let mut out = Vec::new();

    for col in &collections.collections {
        if !col.name.starts_with(MEMORIES_PREFIX) && col.name != FACTS_COLLECTION {
            continue;
        }
        if let Ok(info) = client.collection_info(&col.name).await
            && let Some(ci) = info.result.as_ref()
            && let Some(dim) = collection_dim(ci)
            && dim != config.vector_size
        {
            out.push((col.name.clone(), dim));
        }
    }

    Ok(out)
}

/// Fetch a payload string for an existing point by id (used to preserve
/// `created_at` across idempotent re-upserts of content-addressed points).
pub(crate) async fn existing_payload_string(
    client: &Qdrant,
    collection: &str,
    id: &str,
    key: &str,
) -> Option<String> {
    let pid: qdrant_client::qdrant::PointId = id.to_string().into();
    let resp = client
        .get_points(GetPointsBuilder::new(collection, vec![pid]).with_payload(true))
        .await
        .ok()?;
    let point = resp.result.into_iter().next()?;
    extract_string(&point.payload, key)
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
        anyhow::bail!(
            "{}",
            crate::error::MindError::LibraryNotFound(library.to_string())
        );
    }

    let embedding = embedder::embed(config, content).await?;
    check_dim(&embedding, config)?;

    // Deterministic ID → idempotent upsert. Identical content overwrites the same
    // point instead of creating a duplicate, with no read-before-write race (audit #8, #15).
    let id = deterministic_id(library, content);
    let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    // Preserve the original first-seen timestamp on re-add; only `updated_at`
    // moves. Otherwise re-adding identical content would reset created_at=now
    // and the entry would jump to the top of chronological history.
    let created_at = existing_payload_string(&client, &col, &id, "created_at")
        .await
        .unwrap_or_else(|| now.clone());

    let mut payload: HashMap<String, qdrant_client::qdrant::Value> = HashMap::new();
    payload.insert("content".into(), content.into());
    payload.insert("hash".into(), hash.into());
    payload.insert("created_at".into(), created_at.into());
    payload.insert("updated_at".into(), now.into());
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
    check_dim(&embedding, config)?;

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
                let created_at = extract_string(payload, "created_at");

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
                    created_at,
                    score: point.score,
                });
            }
        }
    }

    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(limit);

    Ok(results)
}

pub async fn delete_memory(config: &MindConfig, library: &str, id: &str) -> Result<()> {
    let client = get_client(config).await?;
    let col = collection_name(library);

    if !client.collection_exists(&col).await.unwrap_or(false) {
        anyhow::bail!(
            "{}",
            crate::error::MindError::LibraryNotFound(library.to_string())
        );
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

/// Scroll an entire collection, following pagination to the end (audit #10).
pub async fn scroll_all(
    client: &Qdrant,
    collection: &str,
) -> Result<Vec<qdrant_client::qdrant::RetrievedPoint>> {
    let mut out = Vec::new();
    let mut offset: Option<qdrant_client::qdrant::PointId> = None;

    loop {
        let mut builder = ScrollPointsBuilder::new(collection)
            .limit(SCROLL_PAGE)
            .with_payload(true);
        if let Some(o) = offset.clone() {
            builder = builder.offset(o);
        }

        let response = client.scroll(builder).await?;
        out.extend(response.result);

        match response.next_page_offset {
            Some(next) => offset = Some(next),
            None => break,
        }
    }

    Ok(out)
}

/// Recent memories across all libraries, newest first.
///
/// NOTE (v0.3): this scrolls every collection fully into memory and then keeps
/// the top `limit`. Correct, but O(total memories) regardless of `limit` — fine
/// at the current scale, a known scalability item. The proper fix is a Qdrant
/// `order_by` over a datetime payload index on `created_at` (newest-N without
/// reading everything), which needs a payload-index migration on existing
/// collections — deferred to the v0.3 storage rework alongside #16/#18.
pub async fn history(config: &MindConfig, limit: usize) -> Result<Vec<SearchResult>> {
    let client = get_client(config).await?;
    let collections = client.list_collections().await?;

    let mut all_results = Vec::new();

    for col in &collections.collections {
        if let Some(lib_name) = col.name.strip_prefix(MEMORIES_PREFIX) {
            let points = scroll_all(&client, &col.name).await.unwrap_or_default();
            for point in points {
                let payload = &point.payload;
                let content = extract_string(payload, "content").unwrap_or_default();
                let source = extract_string(payload, "source");
                let created_at = extract_string(payload, "created_at");

                let id = match &point.id {
                    Some(pid) => format_point_id(pid),
                    None => "unknown".to_string(),
                };

                all_results.push(SearchResult {
                    id,
                    library: lib_name.to_string(),
                    content: truncate_str(&content, 200),
                    source,
                    created_at,
                    score: 0.0,
                });
            }
        }
    }

    // Newest first. RFC3339 timestamps sort chronologically as strings (audit #9).
    all_results.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    all_results.truncate(limit);
    Ok(all_results)
}

pub async fn export_all(config: &MindConfig, format: &str, output_dir: &str) -> Result<usize> {
    if format != "json" && format != "md" {
        anyhow::bail!("Unsupported format: {format}. Use json or md");
    }

    let client = get_client(config).await?;
    let collections = client.list_collections().await?;

    std::fs::create_dir_all(output_dir)?;

    let mut total = 0;

    for col in &collections.collections {
        let lib_name = col.name.strip_prefix(MEMORIES_PREFIX).unwrap_or(&col.name);

        // Full pagination — no silent 10k cap (audit #10).
        let points = scroll_all(&client, &col.name).await.unwrap_or_default();

        let entries: Vec<serde_json::Value> = points
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
        let path = std::path::Path::new(&file_path);

        match format {
            "json" => {
                let json = serde_json::to_string_pretty(&entries)?;
                crate::util::atomic_write_str(path, &json)?;
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
                crate::util::atomic_write_str(path, &md)?;
            }
            _ => unreachable!(),
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

/// Native gzip+tar backup of the data dir — no `tar` shellout (audit #19).
pub fn backup(output: &str) -> Result<()> {
    let home = crate::config::mind_home();
    let file = std::fs::File::create(output)
        .with_context(|| format!("Failed to create backup file {output}"))?;
    let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut tar = tar::Builder::new(enc);
    tar.append_dir_all(".", &home)
        .context("Failed to archive data directory")?;
    tar.into_inner()?.finish()?;
    Ok(())
}

/// Native gzip+tar restore — no `tar` shellout (audit #19).
pub fn restore(input: &str) -> Result<()> {
    let home = crate::config::mind_home();
    std::fs::create_dir_all(&home)?;
    let file = std::fs::File::open(input)
        .with_context(|| format!("Failed to open backup file {input}"))?;
    let dec = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(dec);
    archive.unpack(&home).context("Failed to extract backup")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_id_is_stable_and_content_addressed() {
        let a = deterministic_id("lib", "hello world");
        let b = deterministic_id("lib", "hello world");
        let c = deterministic_id("lib", "different");
        let d = deterministic_id("other", "hello world");
        assert_eq!(
            a, b,
            "same library+content must yield the same id (idempotent upsert)"
        );
        assert_ne!(a, c, "different content must differ");
        assert_ne!(a, d, "different library must differ");
        // valid UUID string
        assert!(uuid::Uuid::parse_str(&a).is_ok());
    }

    #[test]
    fn truncate_breaks_on_word_boundary() {
        let s = "alpha beta gamma delta epsilon";
        let t = super::truncate_str(s, 12);
        assert!(t.ends_with("..."));
        // must not cut mid-word: the part before "..." ends at a full word
        let body = t.trim_end_matches('.');
        assert!(s.starts_with(body.trim_end()));
        assert!(!body.ends_with("gam"));
    }

    #[test]
    fn truncate_noop_when_short() {
        assert_eq!(super::truncate_str("short", 100), "short");
    }
}
