use anyhow::{Context, Result};
use qdrant_client::Qdrant;
use qdrant_client::qdrant::{
    Condition, CountPointsBuilder, CreateCollectionBuilder, CreateFieldIndexCollectionBuilder,
    DeleteCollectionBuilder, DeletePointsBuilder, Direction, Distance, FieldType, Filter, Fusion,
    GetPointsBuilder, Modifier, NamedVectors, OrderBy, PointStruct, PointsIdsList,
    PrefetchQueryBuilder, Query, QueryPointsBuilder, ScrollPointsBuilder,
    SparseVectorParamsBuilder, SparseVectorsConfigBuilder, UpsertPointsBuilder, Vector,
    VectorInput, VectorParamsBuilder, VectorsConfigBuilder,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

use crate::config::MindConfig;
use crate::embedder;

/// Single unified collection for all memories (audit #18). Libraries are a
/// `library` payload field + filter, not separate collections - so a search can
/// rank globally in one query and `history` can `order_by` an indexed
/// `created_at` instead of scrolling everything.
pub const MEMORIES_COLLECTION: &str = "memories";
pub const FACTS_COLLECTION: &str = "_kg_facts";

/// Legacy per-library collection prefix (`mem_<library>`), kept only so
/// `migrate` can find and import old-layout data.
const LEGACY_PREFIX: &str = "mem_";

/// Fixed namespace for deterministic content IDs (audit #15).
/// UUIDv5(namespace, library + content) → identical content yields the same
/// point ID, so re-adding is an idempotent upsert (no duplicates, no TOCTOU race).
const MGI_NAMESPACE: Uuid = Uuid::from_u128(0x6d676900_6d69_6e64_0000_000000000001);

/// How many points to pull per scroll page (export/migrate pagination, audit #10).
const SCROLL_PAGE: u32 = 256;

/// Named vectors on the memories collection (audit #23 hybrid): a dense vector
/// (e5 semantic) and a sparse vector (BM25-style lexical).
const DENSE_VEC: &str = "dense";
const SPARSE_VEC: &str = "sparse";

/// Stable token id for a sparse term. Hash the term to a u32 index; with Qdrant's
/// IDF modifier on the sparse vector, term frequencies become BM25-ish scores.
fn token_id(token: &str) -> u32 {
    let h = blake3::hash(token.as_bytes());
    let b = h.as_bytes();
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// Build a BM25-style sparse vector (term-frequency) from text. Unicode-aware:
/// splits on non-alphanumeric (handles Cyrillic), lowercases, drops 1-char tokens.
/// IDF is applied server-side via the collection's IDF modifier (audit #23).
fn sparse_vector(text: &str) -> (Vec<u32>, Vec<f32>) {
    let mut counts: HashMap<u32, f32> = HashMap::new();
    for token in text.to_lowercase().split(|c: char| !c.is_alphanumeric()) {
        if token.chars().take(2).count() < 2 {
            continue;
        }
        *counts.entry(token_id(token)).or_insert(0.0) += 1.0;
    }
    counts.into_iter().unzip()
}

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

fn deterministic_id(library: &str, content: &str) -> String {
    let key = format!("{library}\u{0}{content}");
    Uuid::new_v5(&MGI_NAMESPACE, key.as_bytes()).to_string()
}

/// Filter that restricts a query to one library (audit #18).
fn library_filter(library: &str) -> Filter {
    Filter::must([Condition::matches("library", library.to_string())])
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

// --- Library registry -------------------------------------------------------
// With a single collection, libraries no longer map to Qdrant collections, so we
// track the known set in a small JSON file. All mutations go through our API, so
// it stays in sync; counts always come from live data (never from this file).

fn libraries_path() -> std::path::PathBuf {
    crate::config::mind_home().join("libraries.json")
}

fn registered_libraries() -> Vec<String> {
    std::fs::read_to_string(libraries_path())
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .unwrap_or_default()
}

fn is_registered(name: &str) -> bool {
    registered_libraries().iter().any(|l| l == name)
}

fn register_library(name: &str) -> Result<()> {
    let mut libs = registered_libraries();
    if !libs.iter().any(|l| l == name) {
        libs.push(name.to_string());
        libs.sort();
        crate::util::atomic_write_str(&libraries_path(), &serde_json::to_string_pretty(&libs)?)?;
    }
    Ok(())
}

fn unregister_library(name: &str) -> Result<()> {
    let mut libs = registered_libraries();
    let before = libs.len();
    libs.retain(|l| l != name);
    if libs.len() != before {
        crate::util::atomic_write_str(&libraries_path(), &serde_json::to_string_pretty(&libs)?)?;
    }
    Ok(())
}

// --- Collection setup -------------------------------------------------------

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

/// Create the payload indexes the single-collection layout relies on (audit #18):
/// `library` (keyword) for fast per-library filtering, `created_at` (datetime)
/// for `order_by` in `history`. Idempotent - "already exists" errors are ignored.
async fn ensure_payload_indexes(client: &Qdrant, collection: &str) {
    let _ = client
        .create_field_index(CreateFieldIndexCollectionBuilder::new(
            collection,
            "library",
            FieldType::Keyword,
        ))
        .await;
    let _ = client
        .create_field_index(CreateFieldIndexCollectionBuilder::new(
            collection,
            "created_at",
            FieldType::Datetime,
        ))
        .await;
}

/// Create the memories collection with named vectors (audit #23 hybrid): a dense
/// vector (`dense`, cosine) for semantic search and a sparse vector (`sparse`,
/// IDF modifier) for BM25-style lexical search, fused at query time via RRF.
async fn create_memories_collection(client: &Qdrant, dim: u64) -> Result<()> {
    let mut dense = VectorsConfigBuilder::default();
    dense.add_named_vector_params(DENSE_VEC, VectorParamsBuilder::new(dim, Distance::Cosine));

    let mut sparse = SparseVectorsConfigBuilder::default();
    sparse.add_named_vector_params(
        SPARSE_VEC,
        SparseVectorParamsBuilder::default().modifier(Modifier::Idf as i32),
    );

    client
        .create_collection(
            CreateCollectionBuilder::new(MEMORIES_COLLECTION)
                .vectors_config(dense)
                .sparse_vectors_config(sparse),
        )
        .await
        .context("Failed to create memories collection")?;
    Ok(())
}

pub async fn ensure_memories_collection(client: &Qdrant, dim: u64) -> Result<()> {
    if !client
        .collection_exists(MEMORIES_COLLECTION)
        .await
        .unwrap_or(false)
    {
        create_memories_collection(client, dim).await?;
    }
    ensure_payload_indexes(client, MEMORIES_COLLECTION).await;
    Ok(())
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

pub async fn init(config: &MindConfig) -> Result<()> {
    let client = get_client(config).await?;
    ensure_memories_collection(&client, config.vector_size).await?;
    ensure_facts_collection(&client, config.vector_size).await?;
    Ok(())
}

// --- Library management -----------------------------------------------------

pub async fn create_library(config: &MindConfig, name: &str) -> Result<()> {
    if is_registered(name) {
        anyhow::bail!(
            "{}",
            crate::error::MindError::LibraryExists(name.to_string())
        );
    }
    let client = get_client(config).await?;
    ensure_memories_collection(&client, config.vector_size).await?;
    register_library(name)?;
    Ok(())
}

pub async fn drop_library(config: &MindConfig, name: &str) -> Result<()> {
    let client = get_client(config).await?;
    if client
        .collection_exists(MEMORIES_COLLECTION)
        .await
        .unwrap_or(false)
    {
        // Delete every point belonging to this library (audit #18: a library is
        // a payload filter, not a whole collection).
        client
            .delete_points(
                DeletePointsBuilder::new(MEMORIES_COLLECTION)
                    .points(library_filter(name))
                    .wait(true),
            )
            .await
            .context("Failed to delete library points")?;
    }
    unregister_library(name)?;
    Ok(())
}

pub async fn list_libraries(_config: &MindConfig) -> Result<Vec<String>> {
    Ok(registered_libraries())
}

// --- Dimension guards (audit #11) ------------------------------------------

/// Validate that an embedding matches the configured dimension (audit #11).
pub(crate) fn check_dim(embedding: &[f32], config: &MindConfig) -> Result<()> {
    if embedding.len() as u64 != config.vector_size {
        anyhow::bail!(
            "Embedding dimension {} does not match configured vector_size {} \
             (model '{}' may have changed - run a reindex)",
            embedding.len(),
            config.vector_size,
            config.model_name
        );
    }
    Ok(())
}

/// Read the configured vector dimension of an existing collection, if it can be
/// determined. Returns `None` for named-vector layouts or any shape we can't
/// confidently parse - callers treat `None` as "unknown", never as a mismatch.
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

/// Best-effort check that the memories/facts collections' on-disk vector
/// dimension matches `config.vector_size` (audit #11). A mismatch means the
/// embedding model changed without a reindex - upserts would fail with a raw
/// Qdrant error. Returns `(collection, actual_dim)` for each disagreement;
/// collections whose dimension can't be parsed are skipped, never falsely flagged.
pub async fn dimension_mismatches(config: &MindConfig) -> Result<Vec<(String, u64)>> {
    let client = get_client(config).await?;
    let mut out = Vec::new();

    for name in [MEMORIES_COLLECTION, FACTS_COLLECTION] {
        if let Ok(info) = client.collection_info(name).await
            && let Some(ci) = info.result.as_ref()
            && let Some(dim) = collection_dim(ci)
            && dim != config.vector_size
        {
            out.push((name.to_string(), dim));
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

// --- Core memory operations -------------------------------------------------

pub async fn add_memory(
    config: &MindConfig,
    library: &str,
    content: &str,
    source: Option<&str>,
) -> Result<String> {
    if !is_registered(library) {
        anyhow::bail!(
            "{}",
            crate::error::MindError::LibraryNotFound(library.to_string())
        );
    }

    let client = get_client(config).await?;
    ensure_memories_collection(&client, config.vector_size).await?;

    let embedding = embedder::embed_passage(config, content).await?;
    check_dim(&embedding, config)?;

    // Deterministic ID → idempotent upsert. Identical content overwrites the same
    // point instead of creating a duplicate, with no read-before-write race (audit #8, #15).
    let id = deterministic_id(library, content);
    let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    // Preserve the original first-seen timestamp on re-add; only `updated_at`
    // moves. Otherwise re-adding identical content would reset created_at=now
    // and the entry would jump to the top of chronological history.
    let created_at = existing_payload_string(&client, MEMORIES_COLLECTION, &id, "created_at")
        .await
        .unwrap_or_else(|| now.clone());

    // Named dense (semantic) + sparse (BM25-style lexical) vectors for hybrid (#23).
    let (s_idx, s_val) = sparse_vector(content);
    let vectors = NamedVectors::default()
        .add_vector(DENSE_VEC, Vector::new_dense(embedding))
        .add_vector(SPARSE_VEC, Vector::new_sparse(s_idx, s_val));
    let point = PointStruct::new(
        id.clone(),
        vectors,
        build_payload(content, &hash, &created_at, &now, library, source),
    );

    client
        .upsert_points(UpsertPointsBuilder::new(MEMORIES_COLLECTION, vec![point]).wait(true))
        .await
        .context("Failed to add memory")?;

    Ok(id)
}

fn build_payload(
    content: &str,
    hash: &str,
    created_at: &str,
    updated_at: &str,
    library: &str,
    source: Option<&str>,
) -> HashMap<String, qdrant_client::qdrant::Value> {
    let mut payload: HashMap<String, qdrant_client::qdrant::Value> = HashMap::new();
    payload.insert("content".into(), content.into());
    payload.insert("hash".into(), hash.into());
    payload.insert("created_at".into(), created_at.into());
    payload.insert("updated_at".into(), updated_at.into());
    payload.insert("library".into(), library.into());
    if let Some(src) = source {
        payload.insert("source".into(), src.into());
    }
    payload
}

pub async fn search(
    config: &MindConfig,
    query: &str,
    library: Option<&str>,
    limit: usize,
    tier: u8,
) -> Result<Vec<SearchResult>> {
    let client = get_client(config).await?;
    if !client
        .collection_exists(MEMORIES_COLLECTION)
        .await
        .unwrap_or(false)
    {
        return Ok(Vec::new());
    }

    let embedding = embedder::embed_query(config, query).await?;
    check_dim(&embedding, config)?;
    let (s_idx, s_val) = sparse_vector(query);

    // Fetch a wider candidate set so the reranker has room to re-order (#22).
    let fetch_k = if config.rerank_enabled {
        config.rerank_top_k.max(limit)
    } else {
        limit
    } as u64;

    // Hybrid retrieval (audit #23): dense (semantic) + sparse (BM25) prefetches
    // fused with Reciprocal Rank Fusion. A library filter applies to both arms.
    let mut dense_pf = PrefetchQueryBuilder::default()
        .query(Query::new_nearest(VectorInput::new_dense(embedding)))
        .using(DENSE_VEC)
        .limit(fetch_k);
    let mut sparse_pf = PrefetchQueryBuilder::default()
        .query(Query::new_nearest(VectorInput::new_sparse(s_idx, s_val)))
        .using(SPARSE_VEC)
        .limit(fetch_k);
    if let Some(lib) = library {
        dense_pf = dense_pf.filter(library_filter(lib));
        sparse_pf = sparse_pf.filter(library_filter(lib));
    }

    let response = client
        .query(
            QueryPointsBuilder::new(MEMORIES_COLLECTION)
                .add_prefetch(dense_pf)
                .add_prefetch(sparse_pf)
                .query(Query::new_fusion(Fusion::Rrf))
                .limit(fetch_k)
                .with_payload(true),
        )
        .await
        .context("Hybrid search failed")?;

    // Keep the FULL content for the reranker; tier truncation is display-only and
    // applied after the final ordering.
    let mut cands: Vec<SearchResult> = response
        .result
        .into_iter()
        .map(|point| {
            let payload = &point.payload;
            SearchResult {
                id: point.id.as_ref().map(format_point_id).unwrap_or_default(),
                library: extract_string(payload, "library").unwrap_or_default(),
                content: extract_string(payload, "content").unwrap_or_default(),
                source: extract_string(payload, "source"),
                created_at: extract_string(payload, "created_at"),
                score: point.score,
            }
        })
        .collect();

    // Cross-encoder rerank (audit #22). Best-effort: on any reranker failure the
    // dense order is kept (reranking is a quality boost, not a dependency).
    if config.rerank_enabled && cands.len() > 1 {
        let texts: Vec<String> = cands.iter().map(|c| c.content.clone()).collect();
        if let Ok(scores) = crate::reranker::scores(config, query, &texts).await
            && scores.len() == cands.len()
        {
            for (c, s) in cands.iter_mut().zip(scores) {
                c.score = s;
            }
            cands.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }
    }

    cands.truncate(limit);
    for c in &mut cands {
        c.content = match tier {
            1 => truncate_str(&c.content, 100),
            2 => truncate_str(&c.content, 500),
            _ => std::mem::take(&mut c.content),
        };
    }

    Ok(cands)
}

pub async fn delete_memory(config: &MindConfig, _library: &str, id: &str) -> Result<()> {
    let client = get_client(config).await?;
    // IDs are globally unique (UUIDv5 of library+content), so a delete by id in
    // the single collection is unambiguous - the library arg is kept only for
    // CLI/MCP signature compatibility.
    let point_id: qdrant_client::qdrant::PointId = id.to_string().into();
    client
        .delete_points(
            DeletePointsBuilder::new(MEMORIES_COLLECTION)
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

/// Recent memories, newest first. Single collection + a datetime index on
/// `created_at` let Qdrant return the newest `limit` via `order_by` - no longer
/// O(total memories) (audit #18, fixes the post-0.2 review's `history` finding).
pub async fn history(config: &MindConfig, limit: usize) -> Result<Vec<SearchResult>> {
    let client = get_client(config).await?;
    if !client
        .collection_exists(MEMORIES_COLLECTION)
        .await
        .unwrap_or(false)
    {
        return Ok(Vec::new());
    }

    let order = OrderBy {
        key: "created_at".to_string(),
        direction: Some(Direction::Desc as i32),
        start_from: None,
    };

    let response = client
        .scroll(
            ScrollPointsBuilder::new(MEMORIES_COLLECTION)
                .limit(limit as u32)
                .with_payload(true)
                .order_by(order),
        )
        .await
        .context("history scroll failed")?;

    let results = response
        .result
        .into_iter()
        .map(|point| {
            let payload = &point.payload;
            let content = extract_string(payload, "content").unwrap_or_default();
            SearchResult {
                id: point.id.as_ref().map(format_point_id).unwrap_or_default(),
                library: extract_string(payload, "library").unwrap_or_default(),
                content: truncate_str(&content, 200),
                source: extract_string(payload, "source"),
                created_at: extract_string(payload, "created_at"),
                score: 0.0,
            }
        })
        .collect();

    Ok(results)
}

pub async fn export_all(config: &MindConfig, format: &str, output_dir: &str) -> Result<usize> {
    if format != "json" && format != "md" {
        anyhow::bail!("Unsupported format: {format}. Use json or md");
    }

    let client = get_client(config).await?;
    std::fs::create_dir_all(output_dir)?;

    if !client
        .collection_exists(MEMORIES_COLLECTION)
        .await
        .unwrap_or(false)
    {
        return Ok(0);
    }

    // Group every point by its `library` payload, then write one file per library.
    let points = scroll_all(&client, MEMORIES_COLLECTION)
        .await
        .unwrap_or_default();
    let mut by_library: HashMap<String, Vec<serde_json::Value>> = HashMap::new();

    for point in &points {
        let payload = &point.payload;
        let Some(content) = extract_string(payload, "content") else {
            continue;
        };
        let library = extract_string(payload, "library").unwrap_or_else(|| "default".to_string());
        by_library
            .entry(library)
            .or_default()
            .push(serde_json::json!({
                "id": point.id.as_ref().map(format_point_id),
                "content": content,
                "source": extract_string(payload, "source"),
                "created_at": extract_string(payload, "created_at"),
            }));
    }

    let mut total = 0;
    for (lib_name, entries) in &by_library {
        total += entries.len();
        let file_path = format!("{output_dir}/{lib_name}.{format}");
        let path = std::path::Path::new(&file_path);

        match format {
            "json" => {
                let json = serde_json::to_string_pretty(entries)?;
                crate::util::atomic_write_str(path, &json)?;
            }
            "md" => {
                let mut md = format!("# {lib_name}\n\n");
                for entry in entries {
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

async fn count_library(client: &Qdrant, library: &str) -> u64 {
    client
        .count(CountPointsBuilder::new(MEMORIES_COLLECTION).filter(library_filter(library)))
        .await
        .ok()
        .and_then(|r| r.result)
        .map(|c| c.count)
        .unwrap_or(0)
}

pub async fn stats(config: &MindConfig) -> Result<(Vec<(String, u64)>, u64)> {
    let client = get_client(config).await?;

    let mut libraries = Vec::new();
    if client
        .collection_exists(MEMORIES_COLLECTION)
        .await
        .unwrap_or(false)
    {
        for lib in registered_libraries() {
            let count = count_library(&client, &lib).await;
            libraries.push((lib, count));
        }
    }

    let facts_count = client
        .collection_info(FACTS_COLLECTION)
        .await
        .ok()
        .and_then(|i| i.result)
        .and_then(|r| r.points_count)
        .unwrap_or(0);

    Ok((libraries, facts_count))
}

/// Migrate legacy per-library collections (`mem_<library>`) into the single
/// `memories` collection (audit #18). Re-embeds each entry from its stored
/// `content` (no fragile raw-vector extraction) while preserving the original
/// `created_at`; deterministic IDs make it idempotent (safe to re-run). With
/// `purge`, the old collections are deleted after a successful copy.
pub async fn migrate(config: &MindConfig, purge: bool) -> Result<(usize, usize, Vec<String>)> {
    let client = get_client(config).await?;
    ensure_memories_collection(&client, config.vector_size).await?;

    let collections = client.list_collections().await?;
    let mut moved = 0usize;
    let mut skipped = 0usize;
    let mut libs: Vec<String> = Vec::new();

    for col in &collections.collections {
        let Some(lib) = col.name.strip_prefix(LEGACY_PREFIX) else {
            continue;
        };
        register_library(lib)?;
        libs.push(lib.to_string());

        let points = scroll_all(&client, &col.name).await.unwrap_or_default();
        for p in points {
            let Some(content) = extract_string(&p.payload, "content") else {
                continue;
            };
            let source = extract_string(&p.payload, "source");
            let now = chrono::Utc::now().to_rfc3339();
            let created_at =
                extract_string(&p.payload, "created_at").unwrap_or_else(|| now.clone());

            // Skip (don't abort) on a per-entry failure, so one bad record can't
            // kill a long migration. Truncation in the embedder handles overlong
            // inputs; this catches anything else.
            let embedding = match embedder::embed_passage(config, &content).await {
                Ok(e) if check_dim(&e, config).is_ok() => e,
                Ok(_) => {
                    eprintln!("  [skip] dimension mismatch for one entry in {}", col.name);
                    skipped += 1;
                    continue;
                }
                Err(e) => {
                    eprintln!("  [skip] embed failed in {}: {e}", col.name);
                    skipped += 1;
                    continue;
                }
            };
            let id = deterministic_id(lib, &content);
            let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
            let payload = build_payload(&content, &hash, &created_at, &now, lib, source.as_deref());
            let (s_idx, s_val) = sparse_vector(&content);
            let vectors = NamedVectors::default()
                .add_vector(DENSE_VEC, Vector::new_dense(embedding))
                .add_vector(SPARSE_VEC, Vector::new_sparse(s_idx, s_val));

            if let Err(e) = client
                .upsert_points(
                    UpsertPointsBuilder::new(
                        MEMORIES_COLLECTION,
                        vec![PointStruct::new(id, vectors, payload)],
                    )
                    .wait(true),
                )
                .await
            {
                eprintln!("  [skip] upsert failed in {}: {e}", col.name);
                skipped += 1;
                continue;
            }
            moved += 1;
        }

        if purge {
            let _ = client
                .delete_collection(DeleteCollectionBuilder::new(&col.name))
                .await;
        }
    }

    libs.sort();
    libs.dedup();
    Ok((moved, skipped, libs))
}

/// Native gzip+tar backup of the data dir - no `tar` shellout (audit #19).
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

/// Native gzip+tar restore - no `tar` shellout (audit #19).
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
        assert!(uuid::Uuid::parse_str(&a).is_ok());
    }

    #[test]
    fn truncate_breaks_on_word_boundary() {
        let s = "alpha beta gamma delta epsilon";
        let t = super::truncate_str(s, 12);
        assert!(t.ends_with("..."));
        let body = t.trim_end_matches('.');
        assert!(s.starts_with(body.trim_end()));
        assert!(!body.ends_with("gam"));
    }

    #[test]
    fn truncate_noop_when_short() {
        assert_eq!(super::truncate_str("short", 100), "short");
    }
}
