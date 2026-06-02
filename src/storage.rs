use anyhow::{Context, Result};
use once_cell::sync::OnceCell;
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
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
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
/// (e5 semantic) and a sparse vector (lexical, TF with server-side IDF).
const DENSE_VEC: &str = "dense";
const SPARSE_VEC: &str = "sparse";

/// Values for the `type` payload field on the memories collection (phase Д2):
/// the single collection holds both ordinary notes and error→fix playbooks, so a
/// keyword-indexed `type` lets a query scope to one kind. `"memory"` is a plain
/// note; `"procedure"` is a procedural-memory record (phase Д6).
pub const TYPE_MEMORY: &str = "memory";
/// Written by procedural memory (PR4); consolidation already skips it.
pub const TYPE_PROCEDURE: &str = "procedure";

/// Target chunk size in characters for `add_memory` (audit #3/#20). Kept well under
/// the 512-token model cap so a chunk never gets silently truncated at embed time.
const CHUNK_CHARS: usize = 500;

/// Split text into chunks of about `max_chars`, with a small overlap between
/// consecutive chunks and a hard split of any single line longer than `max_chars`.
pub(crate) fn chunk_text(text: &str, max_chars: usize) -> Vec<String> {
    if text.chars().count() <= max_chars {
        return vec![text.to_string()];
    }
    let overlap = (max_chars / 8).max(32);
    let mut units: Vec<String> = Vec::new();
    for line in text.lines() {
        if line.chars().count() <= max_chars {
            units.push(line.to_string());
        } else {
            let chars: Vec<char> = line.chars().collect();
            for piece in chars.chunks(max_chars) {
                units.push(piece.iter().collect());
            }
        }
    }
    let mut chunks = Vec::new();
    let mut current = String::new();
    for unit in &units {
        if !current.is_empty() && current.chars().count() + unit.chars().count() + 1 > max_chars {
            chunks.push(current.clone());
            let count = current.chars().count();
            current = current
                .chars()
                .skip(count.saturating_sub(overlap))
                .collect();
        }
        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(unit);
    }
    if !current.trim().is_empty() {
        chunks.push(current);
    }
    chunks
}

/// Stable token id for a sparse term. Hash the term to a u32 index (collisions
/// only become likely past ~65k distinct tokens; fine at personal scale). Qdrant's
/// IDF modifier weights the term frequencies server-side.
fn token_id(token: &str) -> u32 {
    let h = blake3::hash(token.as_bytes());
    let b = h.as_bytes();
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// Build a lexical sparse vector (raw term frequencies) from text. Unicode-aware:
/// splits on non-alphanumeric (handles Cyrillic), lowercases, drops 1-char tokens.
/// IDF is applied server-side via the collection's IDF modifier (audit #23). This
/// is TF + IDF, not full BM25 (no k1 saturation or length normalization).
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

/// Process-global Qdrant client (audit: single-process). Building a client sets
/// up a gRPC channel; in the old "spawn a CLI per call" model the process died
/// after one op, so caching was pointless. Now `mgimind mcp` lives for the whole
/// session, so we build the client once and reuse the warm channel for every
/// operation - the cheapest per-call win, mirroring the embedder's `OnceCell`.
static CLIENT: OnceCell<Arc<Qdrant>> = OnceCell::new();

pub async fn get_client(config: &MindConfig) -> Result<Arc<Qdrant>> {
    // Building the client is synchronous and does not open a connection (the gRPC
    // channel connects lazily), so memoizing on first use is safe even before
    // Qdrant is up. Callers keep using `&client` - `&Arc<Qdrant>` derefs to
    // `&Qdrant`, so no call site changes.
    CLIENT
        .get_or_try_init(|| {
            let url = format!("http://localhost:{}", config.qdrant_port);
            let mut builder = Qdrant::from_url(&url);
            // Authenticate when an API key is configured (audit #7).
            if let Some(key) = &config.qdrant_api_key {
                builder = builder.api_key(key.clone());
            }
            let client = builder.build().context("Failed to connect to Qdrant")?;
            Ok::<_, anyhow::Error>(Arc::new(client))
        })
        .cloned()
}

fn deterministic_id(library: &str, content: &str) -> String {
    let key = format!("{library}\u{0}{content}");
    Uuid::new_v5(&MGI_NAMESPACE, key.as_bytes()).to_string()
}

/// Deterministic id a candidate WILL get if quarantined — same formula as
/// `deterministic_id`. Exposed so callers (ingest) can pre-compute the id and
/// check "is this content already in quarantine?" before deciding between
/// quarantine and promote. This is what closes the loop between the two gates:
/// re-asserted content lands on the same id and signals promotion.
pub fn quarantine_id_for(library: &str, content: &str) -> String {
    deterministic_id(library, content.trim())
}

/// Filter that restricts a query to one library (audit #18).
fn library_filter(library: &str) -> Filter {
    Filter::must([Condition::matches("library", library.to_string())])
}

/// Filter for ordinary-memory queries (phase Д6 / v0.11): exclude procedures
/// (Д6) and quarantined points (v0.11) so they never pollute a normal
/// `mind_search`. Uses `must_not type=procedure` (NOT `must type=memory`) so
/// the 12k legacy points that predate the `type` field are still included.
/// Same for `quarantined`: legacy points have no flag and stay visible.
fn memory_query_filter(library: Option<&str>) -> Filter {
    let mut f = Filter {
        must_not: vec![
            Condition::matches("type", TYPE_PROCEDURE.to_string()),
            Condition::matches("quarantined", true),
        ],
        ..Default::default()
    };
    if let Some(lib) = library {
        f.must.push(Condition::matches("library", lib.to_string()));
    }
    f
}

/// Filter for explicitly-quarantined queries (v0.11). Used when looking up
/// whether a re-submitted candidate already lives in quarantine — that's the
/// promotion signal: user re-asserting an earlier filtered fact.
pub(crate) fn quarantine_filter(library: &str) -> Filter {
    Filter::must([
        Condition::matches("library", library.to_string()),
        Condition::matches("quarantined", true),
    ])
}

/// Filter that selects only procedures (phase Д6 recall).
fn procedure_filter() -> Filter {
    Filter::must([Condition::matches("type", TYPE_PROCEDURE.to_string())])
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

/// In-memory cache of the library registry (audit: single-process). `is_registered`
/// runs on every `add`; re-reading and re-parsing `libraries.json` from disk each
/// time is wasted work in the long-lived MCP process. All mutations go through
/// `register_library`/`unregister_library`, which update both disk and this
/// cache, so it stays authoritative for this process's own writes.
static LIB_CACHE: OnceCell<Mutex<Vec<String>>> = OnceCell::new();

fn load_libraries_from_disk() -> Vec<String> {
    std::fs::read_to_string(libraries_path())
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .unwrap_or_default()
}

fn lib_cache() -> &'static Mutex<Vec<String>> {
    LIB_CACHE.get_or_init(|| Mutex::new(load_libraries_from_disk()))
}

fn registered_libraries() -> Vec<String> {
    lib_cache()
        .lock()
        .map(|g| g.clone())
        .unwrap_or_else(|_| load_libraries_from_disk())
}

fn is_registered(name: &str) -> bool {
    lib_cache()
        .lock()
        .map(|g| g.iter().any(|l| l == name))
        .unwrap_or(false)
}

fn register_library(name: &str) -> Result<()> {
    let mut libs = lib_cache().lock().expect("library cache mutex poisoned");
    if !libs.iter().any(|l| l == name) {
        libs.push(name.to_string());
        libs.sort();
        crate::util::atomic_write_str(&libraries_path(), &serde_json::to_string_pretty(&*libs)?)?;
    }
    Ok(())
}

fn unregister_library(name: &str) -> Result<()> {
    let mut libs = lib_cache().lock().expect("library cache mutex poisoned");
    let before = libs.len();
    libs.retain(|l| l != name);
    if libs.len() != before {
        crate::util::atomic_write_str(&libraries_path(), &serde_json::to_string_pretty(&*libs)?)?;
    }
    Ok(())
}

// --- Collection setup -------------------------------------------------------

/// True if a `create_collection` failure is just "collection already exists".
/// That happens when two callers create the same collection concurrently - the
/// `collection_exists` check passes for both, then one create wins and the other
/// gets this error (seen as a parallel-test race against a shared Qdrant, but
/// also possible with two concurrent CLI invocations). Treating it as success
/// makes creation idempotent and closes the check-then-create race.
fn is_collection_exists_error<E: std::fmt::Display>(e: &E) -> bool {
    e.to_string().contains("already exists")
}

/// Create a payload-only (vectorless) collection (audit #6). Qdrant supports
/// collections with no vector config - points are pure payload. Used for facts,
/// which are looked up by exact/lexical payload match, never by vector.
/// Idempotent: a concurrent "already exists" is treated as success.
async fn create_vectorless_collection(client: &Qdrant, name: &str) -> Result<()> {
    match client
        .create_collection(CreateCollectionBuilder::new(name))
        .await
    {
        Ok(_) => Ok(()),
        Err(e) if is_collection_exists_error(&e) => Ok(()),
        Err(e) => Err(e).with_context(|| format!("Failed to create collection {name}")),
    }
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
    // `type` (keyword) so a search can scope to memory vs procedure (phase Д2).
    let _ = client
        .create_field_index(CreateFieldIndexCollectionBuilder::new(
            collection,
            "type",
            FieldType::Keyword,
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

    // Idempotent: a concurrent "already exists" (check-then-create race) is success.
    match client
        .create_collection(
            CreateCollectionBuilder::new(MEMORIES_COLLECTION)
                .vectors_config(dense)
                .sparse_vectors_config(sparse),
        )
        .await
    {
        Ok(_) => Ok(()),
        Err(e) if is_collection_exists_error(&e) => Ok(()),
        Err(e) => Err(e).context("Failed to create memories collection"),
    }
}

/// "Collection is ready" flags (audit: single-process memoization). On every
/// `add`/`search` we used to call `collection_exists` (a round-trip) and re-issue
/// idempotent `create_field_index` calls. The schema can't change under us within
/// a session, so once we've ensured a collection + its indexes, we skip all of it.
static MEMORIES_READY: AtomicBool = AtomicBool::new(false);
static FACTS_READY: AtomicBool = AtomicBool::new(false);

pub async fn ensure_memories_collection(client: &Qdrant, dim: u64) -> Result<()> {
    if MEMORIES_READY.load(Ordering::Acquire) {
        return Ok(());
    }
    if !client
        .collection_exists(MEMORIES_COLLECTION)
        .await
        .unwrap_or(false)
    {
        create_memories_collection(client, dim).await?;
    }
    ensure_payload_indexes(client, MEMORIES_COLLECTION).await;
    MEMORIES_READY.store(true, Ordering::Release);
    Ok(())
}

/// Payload indexes for the vectorless facts collection (audit #6): full-text on
/// subject/predicate/object so `query_facts` filters server-side instead of
/// scrolling everything into RAM; keyword on `valid` for the validity filter;
/// datetime on `created_at` so the context briefing can `order_by` newest. All
/// idempotent.
async fn ensure_facts_indexes(client: &Qdrant) {
    for field in ["subject", "predicate", "object"] {
        let _ = client
            .create_field_index(CreateFieldIndexCollectionBuilder::new(
                FACTS_COLLECTION,
                field,
                FieldType::Text,
            ))
            .await;
    }
    let _ = client
        .create_field_index(CreateFieldIndexCollectionBuilder::new(
            FACTS_COLLECTION,
            "valid",
            FieldType::Keyword,
        ))
        .await;
    let _ = client
        .create_field_index(CreateFieldIndexCollectionBuilder::new(
            FACTS_COLLECTION,
            "created_at",
            FieldType::Datetime,
        ))
        .await;
}

/// Does the existing facts collection carry a (legacy, unused) vector config?
/// True for the old layout that embedded every fact; false once it's vectorless.
async fn facts_collection_has_vectors(client: &Qdrant) -> bool {
    use qdrant_client::qdrant::vectors_config::Config;
    let Ok(info) = client.collection_info(FACTS_COLLECTION).await else {
        return false;
    };
    let cfg = info
        .result
        .and_then(|r| r.config)
        .and_then(|c| c.params)
        .and_then(|p| p.vectors_config);
    match cfg.and_then(|c| c.config) {
        Some(Config::Params(_)) => true,
        Some(Config::ParamsMap(m)) => !m.map.is_empty(),
        None => false,
    }
}

/// Migrate a legacy vector-bearing facts collection to vectorless in place
/// (audit #6). The fact vector was always dead weight (never queried), so we drop
/// it: read every fact's payload, recreate the collection without vectors, and
/// re-insert the payloads under their original IDs. Facts are few, so a single
/// in-memory pass is fine.
async fn migrate_facts_to_vectorless(client: &Qdrant) -> Result<()> {
    let points = scroll_all(client, FACTS_COLLECTION)
        .await
        .unwrap_or_default();
    let count = points.len();

    client
        .delete_collection(DeleteCollectionBuilder::new(FACTS_COLLECTION))
        .await
        .context("Failed to drop legacy facts collection during migration")?;
    create_vectorless_collection(client, FACTS_COLLECTION).await?;

    let new_points: Vec<PointStruct> = points
        .into_iter()
        .filter_map(|p| {
            let id = p.id?;
            // No vector: payload-only point (NamedVectors::default() is empty).
            Some(PointStruct::new(id, NamedVectors::default(), p.payload))
        })
        .collect();

    for batch in new_points.chunks(SCROLL_PAGE as usize) {
        client
            .upsert_points(UpsertPointsBuilder::new(FACTS_COLLECTION, batch.to_vec()).wait(true))
            .await
            .context("Failed to re-insert facts during vectorless migration")?;
    }

    eprintln!("mgimind: migrated facts collection to vectorless ({count} facts)");
    Ok(())
}

pub async fn ensure_facts_collection(client: &Qdrant) -> Result<()> {
    if FACTS_READY.load(Ordering::Acquire) {
        return Ok(());
    }
    if client
        .collection_exists(FACTS_COLLECTION)
        .await
        .unwrap_or(false)
    {
        // Auto-migrate an old vector-bearing facts collection so vectorless
        // upserts succeed (the cutover is transparent; facts are few).
        if facts_collection_has_vectors(client).await {
            migrate_facts_to_vectorless(client).await?;
        }
    } else {
        create_vectorless_collection(client, FACTS_COLLECTION).await?;
    }
    ensure_facts_indexes(client).await;
    FACTS_READY.store(true, Ordering::Release);
    Ok(())
}

pub async fn init(config: &MindConfig) -> Result<()> {
    let client = get_client(config).await?;
    ensure_memories_collection(&client, config.vector_size).await?;
    ensure_facts_collection(&client).await?;
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
    crate::audit::record(crate::audit::AuditEvent::new(
        crate::audit::AuditOp::LibraryCreate,
        "",
        name,
    ));
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
    crate::audit::record(crate::audit::AuditEvent::new(
        crate::audit::AuditOp::LibraryDrop,
        "",
        name,
    ));
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

/// Batch GET the `created_at` payload of existing points by id (audit #4). One
/// round-trip for all chunk IDs, instead of a per-chunk read-before-write. Used
/// to preserve the original `created_at` across idempotent re-upserts of
/// content-addressed points. Missing ids (new content) are simply absent.
async fn existing_created_at_map(
    client: &Qdrant,
    collection: &str,
    ids: &[String],
) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if ids.is_empty() {
        return out;
    }
    let pids: Vec<qdrant_client::qdrant::PointId> =
        ids.iter().map(|id| id.clone().into()).collect();
    let Ok(resp) = client
        .get_points(GetPointsBuilder::new(collection, pids).with_payload(true))
        .await
    else {
        return out;
    };
    for point in resp.result {
        let id = point.id.as_ref().map(format_point_id).unwrap_or_default();
        if let Some(ca) = extract_string(&point.payload, "created_at") {
            out.insert(id, ca);
        }
    }
    out
}

/// Store a memory. Long content is split into chunks so nothing is silently lost
/// to the embedder's 512-token cap (audit #3/#20): the main write path no longer
/// drops the tail of a long note. All chunks are embedded in ONE ONNX pass
/// (audit #2) and written with a SINGLE batch upsert after a SINGLE batch GET for
/// existing `created_at` (audit #4) - no per-chunk model run or round-trip.
/// Returns the number of chunks stored.
pub async fn add_memory(
    config: &MindConfig,
    library: &str,
    content: &str,
    source: Option<&str>,
) -> Result<usize> {
    if !is_registered(library) {
        anyhow::bail!(
            "{}",
            crate::error::MindError::LibraryNotFound(library.to_string())
        );
    }

    // Secret scrub (phase Д2): never let a key/password/.env land in searchable
    // memory - it would sit in plaintext and surface in future searches. Refuse
    // the whole write and point at the terminal-only vault. Conservative detector
    // (low false positives), so ordinary prose is unaffected.
    if let Some(hit) = crate::secrets::scan(content) {
        anyhow::bail!(
            "Refusing to store: content looks like a secret ({}). Secrets never go \
             into searchable memory.\nStore it in the terminal-only vault instead:\n    \
             mgimind vault store <key> <value> --category <password|ssh|api-key|token>",
            hit.reason
        );
    }

    let client = get_client(config).await?;
    ensure_memories_collection(&client, config.vector_size).await?;

    // Chunk, dropping trivially short fragments.
    let chunks: Vec<String> = chunk_text(content, CHUNK_CHARS)
        .into_iter()
        .filter(|c| c.trim().chars().count() >= 3)
        .collect();
    if chunks.is_empty() {
        return Ok(0);
    }

    // Embed every chunk in a single batched pass (audit #2).
    let embeddings = embedder::embed_passages(config, &chunks).await?;
    for e in &embeddings {
        check_dim(e, config)?;
    }

    // One batch GET for existing created_at across all chunk IDs (audit #4).
    let ids: Vec<String> = chunks
        .iter()
        .map(|c| deterministic_id(library, c))
        .collect();
    let existing = existing_created_at_map(&client, MEMORIES_COLLECTION, &ids).await;

    let now = chrono::Utc::now().to_rfc3339();
    let mut points = Vec::with_capacity(chunks.len());
    for ((chunk, id), embedding) in chunks.iter().zip(ids.iter()).zip(embeddings) {
        let hash = blake3::hash(chunk.as_bytes()).to_hex().to_string();
        let created_at = existing.get(id).cloned().unwrap_or_else(|| now.clone());

        // Named dense (semantic) + sparse (lexical) vectors for hybrid search (#23).
        let (s_idx, s_val) = sparse_vector(chunk);
        let vectors = NamedVectors::default()
            .add_vector(DENSE_VEC, Vector::new_dense(embedding))
            .add_vector(SPARSE_VEC, Vector::new_sparse(s_idx, s_val));
        points.push(PointStruct::new(
            id.clone(),
            vectors,
            build_payload(
                chunk,
                &hash,
                &created_at,
                &now,
                library,
                source,
                TYPE_MEMORY,
            ),
        ));
    }

    let stored = points.len();
    client
        .upsert_points(UpsertPointsBuilder::new(MEMORIES_COLLECTION, points).wait(true))
        .await
        .context("Failed to add memory")?;
    // Audit the write. Empty target — one logical add can produce N point ids
    // for a long note; the (library, content) tuple is the meaningful trail.
    crate::audit::record(
        crate::audit::AuditEvent::new(crate::audit::AuditOp::Add, library, "")
            .after(truncate_for_audit(content))
            .note(format!("{stored} chunks")),
    );
    Ok(stored)
}

/// Write a candidate that did NOT pass the relevance gate. It still goes
/// into Qdrant — silently dropping filtered candidates causes the loop the
/// critic flagged (user re-asserts a fact, gate filters again, user never
/// gets it stored). Quarantined points carry `quarantined=true` so the normal
/// search filter excludes them, but they're discoverable when the same
/// content arrives again: that's the promotion signal.
///
/// Returns the deterministic id of the written point (used by the caller to
/// audit the quarantine and to detect re-submissions on the next ingest).
pub async fn add_quarantined(
    config: &MindConfig,
    library: &str,
    content: &str,
    source: Option<&str>,
    reason: &str,
) -> Result<String> {
    if !is_registered(library) {
        anyhow::bail!(
            "{}",
            crate::error::MindError::LibraryNotFound(library.to_string())
        );
    }
    // Secret-scrub still applies — even a quarantined memory must never
    // contain a key/password. Quarantine is about relevance, not safety.
    if let Some(hit) = crate::secrets::scan(content) {
        anyhow::bail!(
            "Refusing to quarantine: content looks like a secret ({}). Use the vault.",
            hit.reason
        );
    }

    let client = get_client(config).await?;
    ensure_memories_collection(&client, config.vector_size).await?;

    // One point per quarantined candidate (no chunking) — these are short
    // by definition (the gate filtered them precisely because they were
    // too short / too noisy / too lacking signal). Keeping them single-chunk
    // makes promotion trivial later: same id → simple flag flip.
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    let embedding = embedder::embed_passages(config, &[trimmed.to_string()])
        .await?
        .into_iter()
        .next()
        .context("embedder returned no vector for quarantine candidate")?;
    check_dim(&embedding, config)?;

    let id = deterministic_id(library, trimmed);
    let now = chrono::Utc::now().to_rfc3339();
    let hash = blake3::hash(trimmed.as_bytes()).to_hex().to_string();
    let (s_idx, s_val) = sparse_vector(trimmed);

    let vectors = NamedVectors::default()
        .add_vector(DENSE_VEC, Vector::new_dense(embedding))
        .add_vector(SPARSE_VEC, Vector::new_sparse(s_idx, s_val));

    let payload = build_payload_full(
        trimmed,
        &hash,
        &now,
        &now,
        library,
        source,
        TYPE_MEMORY,
        Some(true),
        Some(reason),
    );

    client
        .upsert_points(
            UpsertPointsBuilder::new(
                MEMORIES_COLLECTION,
                vec![PointStruct::new(id.clone(), vectors, payload)],
            )
            .wait(true),
        )
        .await
        .context("Failed to quarantine candidate")?;

    crate::audit::record(
        crate::audit::AuditEvent::new(crate::audit::AuditOp::Add, library, &id)
            .actor("relevance-gate")
            .after(truncate_for_audit(trimmed))
            .note(format!("quarantined: {reason}")),
    );

    Ok(id)
}

/// Promote a quarantined point to ordinary memory. Called when the same
/// content is re-submitted by the user — that's the signal "user is
/// insistent, raise confidence". Flips `quarantined=false`, clears the
/// reason, updates `updated_at`. Idempotent.
pub async fn promote_from_quarantine(config: &MindConfig, id: &str) -> Result<bool> {
    let client = get_client(config).await?;
    let pid: qdrant_client::qdrant::PointId = id.to_string().into();
    // Fetch the existing point so we can confirm it's actually quarantined
    // (and not accidentally clobber a regular memory through this API).
    let resp = client
        .get_points(
            qdrant_client::qdrant::GetPointsBuilder::new(MEMORIES_COLLECTION, vec![pid.clone()])
                .with_payload(true),
        )
        .await
        .context("Failed to fetch quarantine candidate")?;
    let Some(point) = resp.result.into_iter().next() else {
        return Ok(false);
    };
    let q = extract_bool(&point.payload, "quarantined").unwrap_or(false);
    if !q {
        return Ok(false);
    }
    let library = extract_string(&point.payload, "library").unwrap_or_default();
    let now = chrono::Utc::now().to_rfc3339();
    let mut new_payload: HashMap<String, qdrant_client::qdrant::Value> = HashMap::new();
    new_payload.insert("quarantined".into(), false.into());
    new_payload.insert("updated_at".into(), now.into());
    // Leave `quarantine_reason` in place as historical trail; the audit entry
    // below records the promotion explicitly.
    use qdrant_client::qdrant::SetPayloadPointsBuilder;
    client
        .set_payload(
            SetPayloadPointsBuilder::new(MEMORIES_COLLECTION, new_payload)
                .points_selector(PointsIdsList { ids: vec![pid] })
                .wait(true),
        )
        .await
        .context("Failed to promote from quarantine")?;
    crate::audit::record(
        crate::audit::AuditEvent::new(crate::audit::AuditOp::Update, library, id)
            .actor("relevance-gate")
            .note("promoted from quarantine (re-asserted)"),
    );
    Ok(true)
}

/// A quarantined memory entry, surfaced explicitly through the quarantine
/// commands. Distinct from `SearchResult` because the gate reason matters
/// here (it's the whole point of inspecting the quarantine).
#[derive(Debug, Clone)]
pub struct QuarantineEntry {
    pub id: String,
    pub library: String,
    pub content: String,
    pub source: Option<String>,
    pub reason: String,
    pub created_at: Option<String>,
}

/// List quarantined entries, newest first. `library = None` lists across all
/// libraries; otherwise scoped. The store's `must_not quarantined=true` on
/// normal search means this is the only surface that ever returns these.
pub async fn quarantine_list(
    config: &MindConfig,
    library: Option<&str>,
    limit: usize,
) -> Result<Vec<QuarantineEntry>> {
    let client = get_client(config).await?;
    if !client
        .collection_exists(MEMORIES_COLLECTION)
        .await
        .unwrap_or(false)
    {
        return Ok(Vec::new());
    }

    let filter = match library {
        Some(lib) => quarantine_filter(lib),
        None => Filter::must([Condition::matches("quarantined", true)]),
    };

    let order = OrderBy {
        key: "created_at".to_string(),
        direction: Some(Direction::Desc as i32),
        start_from: None,
    };

    let response = client
        .scroll(
            ScrollPointsBuilder::new(MEMORIES_COLLECTION)
                .filter(filter)
                .limit(limit as u32)
                .with_payload(true)
                .order_by(order),
        )
        .await
        .context("quarantine scroll failed")?;

    let results = response
        .result
        .into_iter()
        .map(|point| {
            let payload = &point.payload;
            let content = extract_string(payload, "content").unwrap_or_default();
            QuarantineEntry {
                id: point.id.as_ref().map(format_point_id).unwrap_or_default(),
                library: extract_string(payload, "library").unwrap_or_default(),
                content: truncate_str(&content, 200),
                source: extract_string(payload, "source"),
                reason: extract_string(payload, "quarantine_reason").unwrap_or_default(),
                created_at: extract_string(payload, "created_at"),
            }
        })
        .collect();

    Ok(results)
}

/// Fetch a single quarantined entry with full (untruncated) content. Returns
/// `None` if the id is unknown OR if the point is not actually quarantined
/// (so callers can't use this to peek at ordinary memories through the
/// quarantine surface — keep the surfaces honest).
pub async fn quarantine_get(config: &MindConfig, id: &str) -> Result<Option<QuarantineEntry>> {
    let client = get_client(config).await?;
    if !client
        .collection_exists(MEMORIES_COLLECTION)
        .await
        .unwrap_or(false)
    {
        return Ok(None);
    }
    let pid: qdrant_client::qdrant::PointId = id.to_string().into();
    let resp = client
        .get_points(
            qdrant_client::qdrant::GetPointsBuilder::new(MEMORIES_COLLECTION, vec![pid.clone()])
                .with_payload(true),
        )
        .await
        .context("quarantine get failed")?;
    let Some(point) = resp.result.into_iter().next() else {
        return Ok(None);
    };
    let q = extract_bool(&point.payload, "quarantined").unwrap_or(false);
    if !q {
        return Ok(None);
    }
    let payload = &point.payload;
    let content = extract_string(payload, "content").unwrap_or_default();
    Ok(Some(QuarantineEntry {
        id: point.id.as_ref().map(format_point_id).unwrap_or_default(),
        library: extract_string(payload, "library").unwrap_or_default(),
        content,
        source: extract_string(payload, "source"),
        reason: extract_string(payload, "quarantine_reason").unwrap_or_default(),
        created_at: extract_string(payload, "created_at"),
    }))
}

/// Helper to read a bool from Qdrant payload.
fn extract_bool(payload: &HashMap<String, qdrant_client::qdrant::Value>, key: &str) -> Option<bool> {
    payload.get(key).and_then(|v| v.kind.as_ref().and_then(|k| match k {
        qdrant_client::qdrant::value::Kind::BoolValue(b) => Some(*b),
        _ => None,
    }))
}

/// Cap audit log lines: a single memory can be a long article. 500 chars is
/// enough to remember what was added without ballooning the log file.
pub(crate) fn truncate_for_audit(s: &str) -> String {
    const MAX: usize = 500;
    if s.chars().count() <= MAX {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(MAX).collect();
        out.push_str("… [truncated]");
        out
    }
}

/// Batched ingest of many (content, source) pairs in one go. The embedding pass
/// runs as a single padded ONNX batch over ALL chunks across all items, which is
/// what makes the bench harness usable: 150 sessions × ~1.5s per-call embedding
/// vs ~3-5s for the whole batch on GPU.
pub async fn add_memories_batch(
    config: &MindConfig,
    library: &str,
    items: &[(String, Option<String>)],
) -> Result<usize> {
    if !is_registered(library) {
        anyhow::bail!(
            "{}",
            crate::error::MindError::LibraryNotFound(library.to_string())
        );
    }
    if items.is_empty() {
        return Ok(0);
    }

    let client = get_client(config).await?;
    ensure_memories_collection(&client, config.vector_size).await?;

    // Collect (chunk, source) pairs across all items. Each item is chunked
    // independently, but all chunks are embedded in a single padded batch.
    // Secret-scrub runs per-item: a hit just skips that item, doesn't poison
    // the whole batch.
    let mut all_chunks: Vec<String> = Vec::with_capacity(items.len());
    let mut all_sources: Vec<Option<String>> = Vec::with_capacity(items.len());
    for (content, source) in items {
        if crate::secrets::scan(content).is_some() {
            continue;
        }
        for chunk in chunk_text(content, CHUNK_CHARS) {
            if chunk.trim().chars().count() < 3 {
                continue;
            }
            all_chunks.push(chunk);
            all_sources.push(source.clone());
        }
    }
    if all_chunks.is_empty() {
        return Ok(0);
    }

    // Sub-batch embedding: ALL chunks in one ONNX padded pass would blow up VRAM
    // on questions with 50+ sessions (a single [batch, seq, hidden] tensor goes
    // multi-GB at seq=512). Embed in groups of 16 — keeps GPU memory bounded
    // while still amortizing the cuDNN warmup that made per-call embedding cost
    // 1.5-3s each. Configurable via MGIMIND_EMBED_BATCH if needed.
    let batch_size: usize = std::env::var("MGIMIND_EMBED_BATCH")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(16);
    let mut embeddings: Vec<Vec<f32>> = Vec::with_capacity(all_chunks.len());
    for sub in all_chunks.chunks(batch_size) {
        let part = embedder::embed_passages(config, sub).await?;
        embeddings.extend(part);
    }
    for e in &embeddings {
        check_dim(e, config)?;
    }

    let ids: Vec<String> = all_chunks
        .iter()
        .map(|c| deterministic_id(library, c))
        .collect();
    let existing = existing_created_at_map(&client, MEMORIES_COLLECTION, &ids).await;

    let now = chrono::Utc::now().to_rfc3339();
    let mut points = Vec::with_capacity(all_chunks.len());
    for (((chunk, id), src), embedding) in all_chunks
        .iter()
        .zip(ids.iter())
        .zip(all_sources.iter())
        .zip(embeddings)
    {
        let hash = blake3::hash(chunk.as_bytes()).to_hex().to_string();
        let created_at = existing.get(id).cloned().unwrap_or_else(|| now.clone());

        let (s_idx, s_val) = sparse_vector(chunk);
        let vectors = NamedVectors::default()
            .add_vector(DENSE_VEC, Vector::new_dense(embedding))
            .add_vector(SPARSE_VEC, Vector::new_sparse(s_idx, s_val));
        points.push(PointStruct::new(
            id.clone(),
            vectors,
            build_payload(
                chunk,
                &hash,
                &created_at,
                &now,
                library,
                src.as_deref(),
                TYPE_MEMORY,
            ),
        ));
    }

    let stored = points.len();
    // wait=false: bench uploads ~150 sessions per question; waiting on each
    // upsert's HNSW indexation serializes the batch. We search the library
    // immediately after — Qdrant handles in-flight points correctly for our
    // single-collection layout.
    client
        .upsert_points(UpsertPointsBuilder::new(MEMORIES_COLLECTION, points).wait(true))
        .await
        .context("Failed to add memories batch")?;
    Ok(stored)
}

fn build_payload(
    content: &str,
    hash: &str,
    created_at: &str,
    updated_at: &str,
    library: &str,
    source: Option<&str>,
    mem_type: &str,
) -> HashMap<String, qdrant_client::qdrant::Value> {
    build_payload_full(content, hash, created_at, updated_at, library, source, mem_type, None, None)
}

/// Full-form payload builder. `quarantined=Some(true)` flags a point as not
/// surfaced in normal search (v0.11); `quarantine_reason` is the human-readable
/// label of which relevance-gate filter rejected it. Both stay None for
/// ordinary writes to keep the on-disk payload size unchanged for the 12k
/// legacy points that predate v0.11.
#[allow(clippy::too_many_arguments)]
fn build_payload_full(
    content: &str,
    hash: &str,
    created_at: &str,
    updated_at: &str,
    library: &str,
    source: Option<&str>,
    mem_type: &str,
    quarantined: Option<bool>,
    quarantine_reason: Option<&str>,
) -> HashMap<String, qdrant_client::qdrant::Value> {
    let mut payload: HashMap<String, qdrant_client::qdrant::Value> = HashMap::new();
    payload.insert("content".into(), content.into());
    payload.insert("hash".into(), hash.into());
    payload.insert("created_at".into(), created_at.into());
    payload.insert("updated_at".into(), updated_at.into());
    payload.insert("library".into(), library.into());
    payload.insert("type".into(), mem_type.into());
    if let Some(src) = source {
        payload.insert("source".into(), src.into());
    }
    if let Some(q) = quarantined {
        payload.insert("quarantined".into(), q.into());
    }
    if let Some(r) = quarantine_reason {
        payload.insert("quarantine_reason".into(), r.into());
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
    // fused with Reciprocal Rank Fusion. Both arms exclude procedures and apply
    // the optional library scope (phase Д6).
    let mf = memory_query_filter(library);
    let dense_pf = PrefetchQueryBuilder::default()
        .query(Query::new_nearest(VectorInput::new_dense(embedding)))
        .using(DENSE_VEC)
        .filter(mf.clone())
        .limit(fetch_k);
    let sparse_pf = PrefetchQueryBuilder::default()
        .query(Query::new_nearest(VectorInput::new_sparse(s_idx, s_val)))
        .using(SPARSE_VEC)
        .filter(mf)
        .limit(fetch_k);

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
            // Map the cross-encoder logit through a sigmoid to 0..1 so the `score`
            // field stays a consistent relevance scale whether or not reranking ran.
            for (c, s) in cands.iter_mut().zip(scores) {
                c.score = 1.0 / (1.0 + (-s).exp());
            }
            cands.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }
    }

    cands.truncate(limit);

    // Record which memories were surfaced, for decay (phase Д2/Д4). In-process
    // only - NOT a Qdrant write on the read path (audit #5). IDs are unaffected
    // by the display-only truncation below, so collect them first.
    let now = chrono::Utc::now().to_rfc3339();
    let surfaced: Vec<String> = cands.iter().map(|c| c.id.clone()).collect();
    crate::access::record(&surfaced, &now);

    for c in &mut cands {
        c.content = match tier {
            1 => truncate_str(&c.content, 100),
            2 => truncate_str(&c.content, 500),
            _ => std::mem::take(&mut c.content),
        };
    }

    Ok(cands)
}

/// Top-1 dense cosine similarity of `content` against existing memories
/// (optionally scoped to a library). Pure read: no write, no rerank, no sparse -
/// just the single nearest neighbor's cosine score. This is the near-duplicate
/// check the original audit listed as the still-missing #8; auto-ingest
/// (PR3) and consolidation (PR2) use it to spot a near-dup of new content before
/// writing it. Returns `None` when there is nothing to compare against (empty or
/// missing collection). Score is cosine in 0..1 (Qdrant cosine similarity);
/// callers compare it against a threshold. The near-dup primitive the original
/// audit listed as the still-missing #8; used by auto-ingest (PR3).
pub async fn nearest_score(
    config: &MindConfig,
    library: Option<&str>,
    content: &str,
) -> Result<Option<f32>> {
    let client = get_client(config).await?;
    if !client
        .collection_exists(MEMORIES_COLLECTION)
        .await
        .unwrap_or(false)
    {
        return Ok(None);
    }

    let embedding = embedder::embed_passage(config, content).await?;
    check_dim(&embedding, config)?;

    let mut q = QueryPointsBuilder::new(MEMORIES_COLLECTION)
        .query(Query::new_nearest(VectorInput::new_dense(embedding)))
        .using(DENSE_VEC)
        .limit(1u64);
    if let Some(lib) = library {
        q = q.filter(library_filter(lib));
    }

    let response = client.query(q).await.context("near-dup query failed")?;
    Ok(response.result.into_iter().next().map(|p| p.score))
}

/// Top-k semantic neighbors of `content` by embedding lookup, returning only
/// their stored content strings. The v0.11 novelty gate uses this to pull the
/// candidate's neighborhood, tokenize it, and check whether the candidate adds
/// any new tokens at all — a low-novelty candidate is quarantined under the
/// "low_novelty" reason. The cost is one embedding inference (same as
/// `nearest_score`); the upside vs `nearest_score` is full content instead of
/// just a similarity score.
pub async fn top_k_neighbor_content(
    config: &MindConfig,
    library: Option<&str>,
    content: &str,
    k: u64,
) -> Result<Vec<String>> {
    let client = get_client(config).await?;
    if !client
        .collection_exists(MEMORIES_COLLECTION)
        .await
        .unwrap_or(false)
    {
        return Ok(Vec::new());
    }

    let embedding = embedder::embed_passage(config, content).await?;
    check_dim(&embedding, config)?;

    let mut q = QueryPointsBuilder::new(MEMORIES_COLLECTION)
        .query(Query::new_nearest(VectorInput::new_dense(embedding)))
        .using(DENSE_VEC)
        .with_payload(true)
        .limit(k);
    if let Some(lib) = library {
        q = q.filter(library_filter(lib));
    }

    let response = client
        .query(q)
        .await
        .context("top_k_neighbor_content query failed")?;
    Ok(response
        .result
        .into_iter()
        .filter_map(|p| extract_string(&p.payload, "content"))
        .collect())
}

/// Nearest neighbors of an EXISTING point, found by its stored dense vector (no
/// re-embedding) - the engine for consolidation's near-dup detection (phase Д2).
/// Returns `(id, cosine_score)` for up to `limit` neighbors, excluding the point
/// itself, optionally scoped to one library. Querying by `VectorInput::new_id`
/// reuses the on-disk vector, so a full-store dedup pass costs one ANN lookup per
/// point instead of one embedding inference per point.
pub async fn near_neighbors_by_id(
    config: &MindConfig,
    id: &str,
    library: Option<&str>,
    limit: u64,
) -> Result<Vec<(String, f32)>> {
    let client = get_client(config).await?;
    let pid: qdrant_client::qdrant::PointId = id.to_string().into();
    let q = QueryPointsBuilder::new(MEMORIES_COLLECTION)
        .query(Query::new_nearest(VectorInput::new_id(pid)))
        .using(DENSE_VEC)
        // Exclude procedures so consolidation never merges a playbook into a note.
        .filter(memory_query_filter(library))
        // +1 because the point itself comes back as the top hit; we drop it below.
        .limit(limit + 1);
    let response = client
        .query(q)
        .await
        .context("near-neighbor query failed")?;
    Ok(response
        .result
        .into_iter()
        .filter_map(|p| {
            let nid = p.id.as_ref().map(format_point_id)?;
            (nid != id).then_some((nid, p.score))
        })
        .take(limit as usize)
        .collect())
}

/// Batch-delete memories by id (consolidation merge step). IDs are globally
/// unique, so this is unambiguous across libraries. No-op on an empty list.
pub async fn delete_memories(config: &MindConfig, ids: &[String]) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let client = get_client(config).await?;
    let pids: Vec<qdrant_client::qdrant::PointId> = ids.iter().map(|i| i.clone().into()).collect();
    client
        .delete_points(
            DeletePointsBuilder::new(MEMORIES_COLLECTION)
                .points(PointsIdsList { ids: pids })
                .wait(true),
        )
        .await
        .context("Failed to delete memories during consolidation")?;
    Ok(())
}

/// Lightweight metadata for one memory point, for consolidation (phase Д2). Keeps
/// raw Qdrant types inside this module; the consolidator works with plain data.
pub struct MemoryMeta {
    pub id: String,
    pub library: String,
    pub content: String,
    pub created_at: Option<String>,
    pub hash: Option<String>,
}

/// Scroll every `memory`-typed point's metadata (no vectors) for consolidation.
/// Procedures (`type = procedure`) are skipped - they are not deduped/decayed by
/// this pass. Points predating the `type` field (no `type` payload) are treated
/// as ordinary memories.
pub async fn scroll_memory_meta(config: &MindConfig) -> Result<Vec<MemoryMeta>> {
    let client = get_client(config).await?;
    if !client
        .collection_exists(MEMORIES_COLLECTION)
        .await
        .unwrap_or(false)
    {
        return Ok(Vec::new());
    }
    let points = scroll_all(&client, MEMORIES_COLLECTION).await?;
    Ok(points
        .into_iter()
        .filter_map(|p| {
            let id = p.id.as_ref().map(format_point_id)?;
            if extract_string(&p.payload, "type").as_deref() == Some(TYPE_PROCEDURE) {
                return None;
            }
            Some(MemoryMeta {
                id,
                library: extract_string(&p.payload, "library").unwrap_or_default(),
                content: extract_string(&p.payload, "content").unwrap_or_default(),
                created_at: extract_string(&p.payload, "created_at"),
                hash: extract_string(&p.payload, "hash"),
            })
        })
        .collect())
}

// --- Procedural memory (phase Д6) -------------------------------------------
// Procedures live in the memories collection as `type = procedure` points, so
// they reuse the hybrid dense+sparse index: the DENSE vector embeds the task
// context (semantic "similar task" match) and the SPARSE vector indexes the
// normalized error signature (lexical match on exact error codes/identifiers,
// nearly free given the existing sparse branch). Normal search excludes them via
// `memory_query_filter`; recall selects them via `procedure_filter`.

/// Namespace for deterministic procedure IDs: one per (normalized error, fix), so
/// re-learning the same fix dedups, while a different fix for the same error is a
/// separate playbook (multiple candidate fixes can be ranked).
const PROC_NAMESPACE: Uuid = Uuid::from_u128(0x6d676900_7072_6f63_0000_000000000001);
/// Library tag for procedures (namespaced; not a user library, never registered).
const PROCEDURE_LIBRARY: &str = "_procedures";

fn procedure_id(norm_error: &str, fix: &str) -> String {
    let key = format!("{norm_error}\u{0}{fix}");
    Uuid::new_v5(&PROC_NAMESPACE, key.as_bytes()).to_string()
}

fn extract_int(payload: &HashMap<String, qdrant_client::qdrant::Value>, key: &str) -> Option<i64> {
    payload.get(key).and_then(|v| {
        if let Some(qdrant_client::qdrant::value::Kind::IntegerValue(i)) = &v.kind {
            Some(*i)
        } else {
            None
        }
    })
}

/// One recalled procedure with its ranking signals.
pub struct ProcedureHit {
    pub id: String,
    pub trigger_error: String,
    pub trigger_context: String,
    pub fix: String,
    pub provenance: Option<String>,
    pub verified: bool,
    pub success_count: i64,
    pub fail_count: i64,
    pub score: f32,
}

/// Fetch an existing procedure's preserved fields (counts, created_at, verified)
/// so a re-learn keeps history instead of resetting it.
async fn existing_procedure(client: &Qdrant, id: &str) -> (Option<String>, i64, i64, bool) {
    let pid: qdrant_client::qdrant::PointId = id.to_string().into();
    let Ok(resp) = client
        .get_points(GetPointsBuilder::new(MEMORIES_COLLECTION, vec![pid]).with_payload(true))
        .await
    else {
        return (None, 0, 0, false);
    };
    let Some(point) = resp.result.into_iter().next() else {
        return (None, 0, 0, false);
    };
    (
        extract_string(&point.payload, "created_at"),
        extract_int(&point.payload, "success_count").unwrap_or(0),
        extract_int(&point.payload, "fail_count").unwrap_or(0),
        extract_string(&point.payload, "verified").as_deref() == Some("true"),
    )
}

/// Store (or update) a procedure. `norm_error` is the already-normalized error
/// signature; `verified` is only ever set true by a caller holding a real truth
/// signal (test green / exit 0) - manual `mind_learn` passes false. Preserves
/// counts and created_at on re-learn; `verified` latches true once set.
pub async fn add_procedure(
    config: &MindConfig,
    norm_error: &str,
    context: &str,
    fix: &str,
    provenance: Option<&str>,
    verified: bool,
) -> Result<String> {
    let client = get_client(config).await?;
    ensure_memories_collection(&client, config.vector_size).await?;

    let id = procedure_id(norm_error, fix);
    let (existing_created, succ, fail, was_verified) = existing_procedure(&client, &id).await;
    let now = chrono::Utc::now().to_rfc3339();
    let created_at = existing_created.unwrap_or_else(|| now.clone());

    // Dense = task context (semantic similar-task match); fall back to the error
    // signature if no context was given. Sparse = normalized error signature.
    let dense_text = if context.trim().is_empty() {
        norm_error
    } else {
        context
    };
    let embedding = embedder::embed_passage(config, dense_text).await?;
    check_dim(&embedding, config)?;
    let (s_idx, s_val) = sparse_vector(norm_error);
    let vectors = NamedVectors::default()
        .add_vector(DENSE_VEC, Vector::new_dense(embedding))
        .add_vector(SPARSE_VEC, Vector::new_sparse(s_idx, s_val));

    let mut payload: HashMap<String, qdrant_client::qdrant::Value> = HashMap::new();
    payload.insert("type".into(), TYPE_PROCEDURE.into());
    payload.insert("library".into(), PROCEDURE_LIBRARY.into());
    payload.insert("trigger_error".into(), norm_error.into());
    payload.insert("trigger_context".into(), context.into());
    payload.insert("fix".into(), fix.into());
    if let Some(p) = provenance {
        payload.insert("provenance".into(), p.into());
    }
    // verified latches: once true (a real signal), a manual re-learn won't unset it.
    let verified = verified || was_verified;
    payload.insert(
        "verified".into(),
        if verified { "true" } else { "false" }.into(),
    );
    payload.insert("success_count".into(), succ.into());
    payload.insert("fail_count".into(), fail.into());
    payload.insert("created_at".into(), created_at.into());
    payload.insert("updated_at".into(), now.into());

    client
        .upsert_points(
            UpsertPointsBuilder::new(
                MEMORIES_COLLECTION,
                vec![PointStruct::new(id.clone(), vectors, payload)],
            )
            .wait(true),
        )
        .await
        .context("Failed to store procedure")?;
    Ok(id)
}

/// Recall procedures matching an error signature and/or a task context. `norm_error`
/// (already normalized) drives the sparse/lexical arm; `context` drives the dense/
/// semantic arm. Returns raw hits with ranking signals; the caller orders them
/// (verified first, then by success vs fail). Pure retrieval - no mutation.
pub async fn recall_procedures(
    config: &MindConfig,
    norm_error: Option<&str>,
    context: Option<&str>,
    limit: usize,
) -> Result<Vec<ProcedureHit>> {
    let client = get_client(config).await?;
    if !client
        .collection_exists(MEMORIES_COLLECTION)
        .await
        .unwrap_or(false)
    {
        return Ok(Vec::new());
    }

    // Dense over context (preferred) or the error signature as a fallback.
    let dense_text = context.or(norm_error).unwrap_or("");
    let embedding = embedder::embed_query(config, dense_text).await?;
    check_dim(&embedding, config)?;

    let mut qb = QueryPointsBuilder::new(MEMORIES_COLLECTION).add_prefetch(
        PrefetchQueryBuilder::default()
            .query(Query::new_nearest(VectorInput::new_dense(embedding)))
            .using(DENSE_VEC)
            .filter(procedure_filter())
            .limit(limit as u64 * 2),
    );
    // Add the lexical arm only when an error signature is provided.
    if let Some(err) = norm_error.filter(|e| !e.trim().is_empty()) {
        let (s_idx, s_val) = sparse_vector(err);
        qb = qb.add_prefetch(
            PrefetchQueryBuilder::default()
                .query(Query::new_nearest(VectorInput::new_sparse(s_idx, s_val)))
                .using(SPARSE_VEC)
                .filter(procedure_filter())
                .limit(limit as u64 * 2),
        );
    }

    let response = client
        .query(
            qb.query(Query::new_fusion(Fusion::Rrf))
                .limit(limit as u64 * 2)
                .with_payload(true),
        )
        .await
        .context("Procedure recall failed")?;

    Ok(response
        .result
        .into_iter()
        .map(|point| {
            let p = &point.payload;
            ProcedureHit {
                id: point.id.as_ref().map(format_point_id).unwrap_or_default(),
                trigger_error: extract_string(p, "trigger_error").unwrap_or_default(),
                trigger_context: extract_string(p, "trigger_context").unwrap_or_default(),
                fix: extract_string(p, "fix").unwrap_or_default(),
                provenance: extract_string(p, "provenance"),
                verified: extract_string(p, "verified").as_deref() == Some("true"),
                success_count: extract_int(p, "success_count").unwrap_or(0),
                fail_count: extract_int(p, "fail_count").unwrap_or(0),
                score: point.score,
            }
        })
        .collect())
}

/// Record the outcome of reusing a procedure: bump success or fail count and
/// stamp `last_used`. A failure (`worked = false`) raises fail_count so recall
/// can demote a fix that stopped working - the store self-corrects instead of
/// ossifying on a bad playbook. Manual success does NOT set `verified` (that
/// needs a deterministic signal, not a human "seems fine").
pub async fn procedure_outcome(config: &MindConfig, id: &str, worked: bool) -> Result<()> {
    use qdrant_client::qdrant::SetPayloadPointsBuilder;
    let client = get_client(config).await?;
    let (_, succ, fail, _) = existing_procedure(&client, id).await;
    let now = chrono::Utc::now().to_rfc3339();

    let mut payload: HashMap<String, qdrant_client::qdrant::Value> = HashMap::new();
    if worked {
        payload.insert("success_count".into(), (succ + 1).into());
    } else {
        payload.insert("fail_count".into(), (fail + 1).into());
    }
    payload.insert("last_used".into(), now.into());

    let pid: qdrant_client::qdrant::PointId = id.to_string().into();
    client
        .set_payload(
            SetPayloadPointsBuilder::new(MEMORIES_COLLECTION, payload)
                .points_selector(PointsIdsList { ids: vec![pid] })
                .wait(true),
        )
        .await
        .context("Failed to record procedure outcome")?;
    Ok(())
}

pub async fn delete_memory(config: &MindConfig, _library: &str, id: &str) -> Result<()> {
    let client = get_client(config).await?;
    // IDs are globally unique (UUIDv5 of library+content), so a delete by id in
    // the single collection is unambiguous - the library arg is kept only for
    // CLI/MCP signature compatibility.

    // Best-effort: snapshot the content first so the audit log keeps a copy of
    // what was deleted. A read failure here is non-fatal — the delete itself
    // must still run, and an empty `before` is better than refusing to delete
    // because we couldn't snapshot.
    let before = fetch_content_by_id(&client, id).await;

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

    let mut ev =
        crate::audit::AuditEvent::new(crate::audit::AuditOp::Delete, _library, id);
    if let Some(content) = before {
        ev = ev.before(truncate_for_audit(&content));
    }
    crate::audit::record(ev);
    Ok(())
}

/// Best-effort content fetch by point id, used by audit pre-delete snapshot.
async fn fetch_content_by_id(client: &Qdrant, id: &str) -> Option<String> {
    let pid: qdrant_client::qdrant::PointId = id.to_string().into();
    let resp = client
        .get_points(
            qdrant_client::qdrant::GetPointsBuilder::new(MEMORIES_COLLECTION, vec![pid])
                .with_payload(true),
        )
        .await
        .ok()?;
    let p = resp.result.into_iter().next()?;
    let kind = p.payload.get("content")?.kind.clone()?;
    match kind {
        qdrant_client::qdrant::value::Kind::StringValue(s) => Some(s),
        _ => None,
    }
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
/// One memory as the viewer needs it. Full content, all metadata, no score
/// (this isn't a search result). Returned by `list_memories` and
/// `find_by_source`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MemoryRecord {
    pub id: String,
    pub content: String,
    pub source: Option<String>,
    pub r#type: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Filter that selects all memories in a library whose `source` payload field
/// equals the given value. Used by md-reconcile import to find existing points
/// for a file path.
fn library_source_filter(library: &str, source: &str) -> Filter {
    Filter::must([
        Condition::matches("library", library.to_string()),
        Condition::matches("source", source.to_string()),
    ])
}

/// Find every memory in `library` whose `source` tag matches `source`. Returns
/// the full record so callers can diff before deciding what to do. Unindexed
/// scan over `source` — fine at our scale; import is a rare operation by design
/// (escape hatch, not steady-state).
pub async fn find_by_source(
    config: &MindConfig,
    library: &str,
    source: &str,
) -> Result<Vec<MemoryRecord>> {
    let client = get_client(config).await?;
    if !client
        .collection_exists(MEMORIES_COLLECTION)
        .await
        .unwrap_or(false)
    {
        return Ok(Vec::new());
    }
    let response = client
        .scroll(
            ScrollPointsBuilder::new(MEMORIES_COLLECTION)
                .filter(library_source_filter(library, source))
                .limit(256)
                .with_payload(true),
        )
        .await
        .context("find_by_source scroll failed")?;
    Ok(response
        .result
        .into_iter()
        .map(|p| {
            let pl = &p.payload;
            MemoryRecord {
                id: p.id.as_ref().map(format_point_id).unwrap_or_default(),
                content: extract_string(pl, "content").unwrap_or_default(),
                source: extract_string(pl, "source"),
                r#type: extract_string(pl, "type").unwrap_or_else(|| "memory".into()),
                created_at: extract_string(pl, "created_at").unwrap_or_default(),
                updated_at: extract_string(pl, "updated_at").unwrap_or_default(),
            }
        })
        .collect())
}

/// List memories of a single library, newest first. Returns full content
/// (no truncation) and all payload metadata. Used by the viewer.
pub async fn list_memories(
    config: &MindConfig,
    library: &str,
    limit: usize,
) -> Result<Vec<MemoryRecord>> {
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
                .filter(library_filter(library))
                .limit(limit as u32)
                .with_payload(true)
                .order_by(order),
        )
        .await
        .context("list_memories scroll failed")?;
    Ok(response
        .result
        .into_iter()
        .map(|point| {
            let p = &point.payload;
            MemoryRecord {
                id: point.id.as_ref().map(format_point_id).unwrap_or_default(),
                content: extract_string(p, "content").unwrap_or_default(),
                source: extract_string(p, "source"),
                r#type: extract_string(p, "type").unwrap_or_else(|| "memory".into()),
                created_at: extract_string(p, "created_at").unwrap_or_default(),
                updated_at: extract_string(p, "updated_at").unwrap_or_default(),
            }
        })
        .collect())
}

/// List recent memories whose `source` matches `source_match` (typically
/// "ingest") and whose `created_at` is at or after `since_iso` (RFC3339).
/// Used by the viewer UI to show "what auto-ingest wrote in this session".
///
/// Implementation: scroll the source-filtered subset newest-first up to
/// `max_scan`, then drop the tail that's older than `since_iso`. The
/// server-side filter narrows to ingest-tagged points; the client-side date
/// cut is one string compare per row (RFC3339 sorts lexicographically when
/// timezone is uniform UTC, which is how we write timestamps).
pub async fn recent_by_source_since(
    config: &MindConfig,
    source_match: &str,
    since_iso: &str,
    max_scan: usize,
) -> Result<Vec<MemoryRecord>> {
    let client = get_client(config).await?;
    if !client
        .collection_exists(MEMORIES_COLLECTION)
        .await
        .unwrap_or(false)
    {
        return Ok(Vec::new());
    }
    let filter = Filter::must([Condition::matches("source", source_match.to_string())]);
    let order = OrderBy {
        key: "created_at".to_string(),
        direction: Some(Direction::Desc as i32),
        start_from: None,
    };
    let response = client
        .scroll(
            ScrollPointsBuilder::new(MEMORIES_COLLECTION)
                .filter(filter)
                .limit(max_scan as u32)
                .with_payload(true)
                .order_by(order),
        )
        .await
        .context("recent_by_source_since scroll failed")?;

    let results: Vec<MemoryRecord> = response
        .result
        .into_iter()
        .filter_map(|point| {
            let p = &point.payload;
            let created_at = extract_string(p, "created_at").unwrap_or_default();
            // Inclusive lower bound, lexicographic compare (RFC3339 UTC).
            if created_at.as_str() < since_iso {
                return None;
            }
            Some(MemoryRecord {
                id: point.id.as_ref().map(format_point_id).unwrap_or_default(),
                content: extract_string(p, "content").unwrap_or_default(),
                source: extract_string(p, "source"),
                r#type: extract_string(p, "type").unwrap_or_else(|| "memory".into()),
                created_at,
                updated_at: extract_string(p, "updated_at").unwrap_or_default(),
            })
        })
        .collect();

    Ok(results)
}

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
            let payload = build_payload(
                &content,
                &hash,
                &created_at,
                &now,
                lib,
                source.as_deref(),
                TYPE_MEMORY,
            );
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

    #[test]
    fn chunk_short_text_is_single_chunk() {
        assert_eq!(chunk_text("hello", 100), vec!["hello".to_string()]);
    }

    #[test]
    fn chunk_long_text_is_bounded() {
        let line = "word ".repeat(400);
        let chunks = chunk_text(&line, 200);
        assert!(chunks.len() > 1);
        for c in &chunks {
            assert!(c.chars().count() <= 200 + 64);
        }
    }

    #[test]
    fn chunk_overlong_single_line_is_hard_split() {
        let giant = "x".repeat(1000);
        let chunks = chunk_text(&giant, 200);
        assert!(chunks.len() >= 5);
        for c in &chunks {
            assert!(c.chars().count() <= 200 + 64);
        }
    }

    #[test]
    fn sparse_vector_is_unicode_and_dedups_terms() {
        // Cyrillic tokenizes; repeated term accumulates frequency.
        let (idx, val) = sparse_vector("Aurora aurora ИИ");
        assert_eq!(idx.len(), val.len());
        // "aurora" (lowercased) appears twice -> one index with value 2.
        assert!(val.iter().any(|&v| (v - 2.0).abs() < 1e-6));
    }
}
