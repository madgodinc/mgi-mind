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
/// v1.4: per-predicate cardinality registry (Single / TemporalSingle / Multi).
/// Lazily created on first cardinality registration; absent predicate = Multi.
pub const PREDICATES_COLLECTION: &str = "_kg_predicates";
/// Derived-state side collection (ADR 0006) for procedure outcome stats:
/// success/fail counts, verified flag, last_used. Vectorless, keyed by the core
/// procedure point's id. Droppable — recall degrades to "no trust boost" when it
/// is absent, never errors, never hides a procedure.
pub const PROCSTATS_COLLECTION: &str = "_mod_procstats";

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
    /// Which agent wrote this memory (multi-agent writes). None for the 12k
    /// legacy points and any single-agent write that did not tag an author.
    pub author: Option<String>,
    pub created_at: Option<String>,
    pub score: f32,
}

/// Optional query-time metadata filters for `search_filtered` (and the
/// inventory `list` path). Every field is additive: a set field narrows the
/// result set, an unset field is ignored. Each field maps onto a Qdrant payload
/// index built in `ensure_payload_indexes` (`library`/`author`/`source`/`type`
/// keyword, `created_at` datetime), so filtering runs in-process against the
/// bundled Qdrant with no full scan and no data leaving the box.
/// Whether a query sees archived (soft-forgotten) memories. The default
/// (`Exclude`) is what every search has always done; `Only` is the inventory
/// path for "show me what was forgotten" (so a memory can be restored without
/// digging the audit log); `Include` is for completeness tools that want both.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ArchivedScope {
    /// Hide archived memories (normal search). The default — unchanged behavior.
    #[default]
    Exclude,
    /// Return ONLY archived memories (the "what did I forget" listing).
    Only,
    /// Return both archived and live memories.
    Include,
}

#[derive(Debug, Default, Clone)]
pub struct MemoryFilter {
    /// Restrict to these libraries (OR). Empty = all libraries. Folds in the old
    /// single-`library` scope and the multi-library-OR case in one field.
    pub libraries: Vec<String>,
    /// Restrict to memories written by this agent (the `author` payload tag).
    pub author: Option<String>,
    /// Restrict by ingest source tag (e.g. `"ingest"`, a session id, a URL).
    pub source: Option<String>,
    /// Only memories created at or after this instant (INCLUSIVE, `gte`).
    /// RFC3339 (`2026-06-09T12:00:00Z`) or a bare `YYYY-MM-DD` date, which means
    /// midnight UTC that day. A date filter excludes legacy points that predate
    /// the `created_at` field (unlike author/source, where legacy stays visible).
    pub created_since: Option<String>,
    /// Only memories created strictly before this instant (EXCLUSIVE, `lt`).
    /// Same formats as `created_since`. Note the exclusivity with bare dates:
    /// `created_before = "2026-06-10"` excludes everything on the 10th, since it
    /// resolves to `2026-06-10T00:00:00Z` and the bound is `< that`.
    pub created_before: Option<String>,
    /// Whether archived (soft-forgotten) memories are seen. Defaults to `Exclude`
    /// so every existing caller keeps its behavior unchanged.
    pub archived: ArchivedScope,
}

impl MemoryFilter {
    /// A filter scoped to a single optional library — the exact behavior of the
    /// old `library: Option<&str>` argument, so `search` can delegate to
    /// `search_filtered` without changing its result for existing callers.
    pub fn for_library(library: Option<&str>) -> Self {
        Self {
            libraries: library.map(|l| vec![l.to_string()]).unwrap_or_default(),
            ..Default::default()
        }
    }

    /// True when no field narrows anything — lets the hot path skip the extra
    /// condition-building entirely.
    fn is_empty(&self) -> bool {
        self.libraries.is_empty()
            && self.author.is_none()
            && self.source.is_none()
            && self.created_since.is_none()
            && self.created_before.is_none()
    }
}

/// Parse an RFC3339 timestamp or a bare `YYYY-MM-DD` date (interpreted as
/// midnight UTC) into a protobuf Timestamp for a Qdrant datetime range.
fn parse_datetime_bound(s: &str) -> Result<qdrant_client::qdrant::Timestamp> {
    use chrono::{DateTime, NaiveDate, Utc};
    let dt = DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .or_else(|_| {
            NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .map(|d| d.and_hms_opt(0, 0, 0).unwrap().and_utc())
        })
        .with_context(|| {
            format!("invalid date '{s}': expected RFC3339 (2026-06-09T12:00:00Z) or YYYY-MM-DD")
        })?;
    Ok(qdrant_client::qdrant::Timestamp {
        seconds: dt.timestamp(),
        nanos: dt.timestamp_subsec_nanos() as i32,
    })
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
    //
    // Use 127.0.0.1 literal, NOT "localhost". `Qdrant::from_url` forwards to
    // tonic/hyper, which does its own DNS resolution against `/etc/hosts`. On
    // most modern Ubuntu container images `/etc/hosts` lists `::1 localhost`
    // before `127.0.0.1 localhost`, so hyper picks IPv6 first. But we spawn the
    // bundled qdrant with `QDRANT__SERVICE__HOST=127.0.0.1` (IPv4-only), so the
    // kernel loopback ESTABs `::1:6334` against nothing on the listener side
    // and the gRPC channel pool wedges forever in HTTP/2 SETTINGS exchange:
    // 8 futex_wait + 1 ep_poll threads, 3 ESTAB-with-zero-bytes connections,
    // no timeout, no error. Forcing IPv4 keeps the client agreeing with the
    // server's bind. The timeouts below are insurance: any future surprise
    // surfaces as a clean error instead of an immortal futex hang.
    CLIENT
        .get_or_try_init(|| {
            let url = format!("http://127.0.0.1:{}", config.qdrant_port);
            let mut builder = Qdrant::from_url(&url)
                .connect_timeout(std::time::Duration::from_secs(5))
                .timeout(std::time::Duration::from_secs(30))
                .keep_alive_while_idle();
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
    // Existing single-library behavior, kept so the 6 non-filtered callers and
    // their tests are untouched. Delegates to the general builder.
    memory_query_filter_ex(&MemoryFilter::for_library(library))
        .expect("library-only filter never has a date bound to parse")
}

/// Build the memory-query filter from a full `MemoryFilter`. Always excludes
/// procedures and quarantined points (the invariant `memory_query_filter`
/// guaranteed); then layers the optional metadata narrowings on top. Returns an
/// error only when a date bound fails to parse — every other field is infallible.
fn memory_query_filter_ex(mf: &MemoryFilter) -> Result<Filter> {
    let mut f = Filter {
        must_not: vec![
            Condition::matches("type", TYPE_PROCEDURE.to_string()),
            Condition::matches("quarantined", true),
        ],
        ..Default::default()
    };

    // Archived (soft-forgotten) scope. Default `Exclude` hides them like quarantine
    // (legacy points lack the field, so `must_not archived=true` keeps them
    // visible). `Only` lists what was forgotten (so it can be restored without
    // digging the audit log); `Include` returns both.
    match mf.archived {
        ArchivedScope::Exclude => f.must_not.push(Condition::matches("archived", true)),
        ArchivedScope::Only => f.must.push(Condition::matches("archived", true)),
        ArchivedScope::Include => {}
    }

    // Fast path: no metadata narrowing beyond the base set — the hot search path
    // for the 6 unfiltered callers. A non-default archived scope already added a
    // condition above, so it is NOT empty.
    if mf.is_empty() {
        return Ok(f);
    }

    // Library scope: one → `must matches`; many → a `should` (OR) sub-filter so
    // a point in ANY of the named libraries qualifies.
    match mf.libraries.as_slice() {
        [] => {}
        [one] => f.must.push(Condition::matches("library", one.clone())),
        many => f.must.push(
            Filter::should(
                many.iter()
                    .map(|l| Condition::matches("library", l.clone())),
            )
            .into(),
        ),
    }

    if let Some(author) = &mf.author {
        f.must.push(Condition::matches("author", author.clone()));
    }
    if let Some(source) = &mf.source {
        f.must.push(Condition::matches("source", source.clone()));
    }

    // Date window on the indexed `created_at` datetime field.
    if mf.created_since.is_some() || mf.created_before.is_some() {
        let range = qdrant_client::qdrant::DatetimeRange {
            gte: mf
                .created_since
                .as_deref()
                .map(parse_datetime_bound)
                .transpose()?,
            lt: mf
                .created_before
                .as_deref()
                .map(parse_datetime_bound)
                .transpose()?,
            ..Default::default()
        };
        f.must.push(Condition::datetime_range("created_at", range));
    }

    Ok(f)
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

pub(crate) fn format_point_id(pid: &qdrant_client::qdrant::PointId) -> String {
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

// --- Pinned memory blocks (Letta-style core memory) -------------------------
//
// A small, ordered set of named blocks (persona / user / current-project) that
// the user or agent edits explicitly and that are injected at the TOP of every
// context render — core memory, NOT a second searchable store. Kept in a plain
// JSON file (blocks are tiny and edited rarely, so no cache / no Qdrant). Caps
// keep the always-on injection cheap.

/// Max bytes of a single block's content; it rides every context render.
pub const MAX_BLOCK_BYTES: usize = 4096;
/// Max number of pinned blocks.
pub const MAX_BLOCKS: usize = 32;

fn blocks_path() -> std::path::PathBuf {
    crate::config::mind_home().join("blocks.json")
}

/// Normalize + validate a block name: trimmed, lowercased, 1–64 chars of
/// `[a-z0-9_-]` (a tag, same spirit as a library name). Returns the canonical
/// form used as the map key.
pub fn normalize_block_name(name: &str) -> Result<String> {
    let n = name.trim().to_lowercase();
    if n.is_empty()
        || n.len() > 64
        || !n
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        anyhow::bail!("block name must be 1-64 chars of [a-z0-9_-], got '{name}'");
    }
    Ok(n)
}

/// Load the pinned-blocks map (name → content). A missing or corrupt file reads
/// as empty — a bad blocks file must never wedge a context render.
pub fn load_blocks() -> std::collections::BTreeMap<String, String> {
    std::fs::read_to_string(blocks_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_blocks(blocks: &std::collections::BTreeMap<String, String>) -> Result<()> {
    crate::util::atomic_write_str(&blocks_path(), &serde_json::to_string_pretty(blocks)?)
}

/// Create or overwrite a pinned block, enforcing the size and count caps.
/// Returns the canonical block name.
pub fn set_block(name: &str, content: &str) -> Result<String> {
    let name = normalize_block_name(name)?;
    if content.len() > MAX_BLOCK_BYTES {
        anyhow::bail!(
            "block '{name}' is {} bytes; cap is {MAX_BLOCK_BYTES}",
            content.len()
        );
    }
    let mut blocks = load_blocks();
    if !blocks.contains_key(&name) && blocks.len() >= MAX_BLOCKS {
        anyhow::bail!("too many pinned blocks ({MAX_BLOCKS} max); remove one first");
    }
    blocks.insert(name.clone(), content.to_string());
    save_blocks(&blocks)?;
    Ok(name)
}

/// Remove a pinned block; returns whether it existed.
pub fn remove_block(name: &str) -> Result<bool> {
    let name = normalize_block_name(name)?;
    let mut blocks = load_blocks();
    let existed = blocks.remove(&name).is_some();
    if existed {
        save_blocks(&blocks)?;
    }
    Ok(existed)
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
pub(crate) async fn create_vectorless_collection(client: &Qdrant, name: &str) -> Result<()> {
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
    for (field, ty) in [
        ("library", FieldType::Keyword),
        ("created_at", FieldType::Datetime),
        // `type` (keyword) so a search can scope to memory vs procedure (phase Д2).
        ("type", FieldType::Keyword),
        // `author` (keyword) so a multi-agent deployment can scope "what did
        // agent X contribute". Legacy points lack the field and are unaffected.
        ("author", FieldType::Keyword),
        // `source` (keyword) so a query-time `source=` filter builds its mask
        // from the index instead of a full payload scan. Legacy points without a
        // source tag lack the field and are simply not matched by the filter.
        ("source", FieldType::Keyword),
        // `quarantined` (bool) so the `must_not quarantined` filter on every
        // search and the quarantine count don't degrade to a full payload scan.
        ("quarantined", FieldType::Bool),
        // `quarantine_reason` (keyword) so the per-reason breakdown in the
        // context digest counts each reason without a payload scan.
        ("quarantine_reason", FieldType::Keyword),
        // `archived` (bool) so the `must_not archived` filter on every search
        // (soft-forgotten cold memories) doesn't degrade to a full payload scan.
        ("archived", FieldType::Bool),
    ] {
        tracing::debug!(collection, field, "ensure_payload_index: start");
        match client
            .create_field_index(CreateFieldIndexCollectionBuilder::new(
                collection, field, ty,
            ))
            .await
        {
            Ok(_) => tracing::debug!(collection, field, "ensure_payload_index: ok"),
            // Idempotent: "already exists" is success. Anything else surfaces so
            // the next infrastructure surprise lands diagnosable, not as an
            // immortal-futex hang on a discarded error (see v0.12.2 PR).
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("already exists") || msg.contains("AlreadyExists") {
                    tracing::debug!(collection, field, "ensure_payload_index: already exists");
                } else {
                    tracing::warn!(collection, field, error = %e, "ensure_payload_index: failed");
                }
            }
        }
    }
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
static PROCSTATS_READY: AtomicBool = AtomicBool::new(false);

/// Idempotently ensure `_mod_procstats` exists, cached per-process so the hot
/// outcome/learn paths don't pay a Qdrant round-trip on every call.
async fn ensure_procstats_collection(client: &Qdrant) -> Result<()> {
    if PROCSTATS_READY.load(Ordering::Acquire) {
        return Ok(());
    }
    create_vectorless_collection(client, PROCSTATS_COLLECTION).await?;
    PROCSTATS_READY.store(true, Ordering::Release);
    Ok(())
}

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
    let fields: &[(&str, FieldType)] = &[
        ("subject", FieldType::Text),
        ("predicate", FieldType::Text),
        ("object", FieldType::Text),
        ("valid", FieldType::Keyword),
        ("created_at", FieldType::Datetime),
        // Which agent asserted the fact — keyword index so a multi-agent
        // deployment can scope "facts asserted by agent X".
        ("author", FieldType::Keyword),
    ];
    for (field, ty) in fields {
        tracing::debug!(field, "ensure_facts_index: start");
        match client
            .create_field_index(CreateFieldIndexCollectionBuilder::new(
                FACTS_COLLECTION,
                *field,
                *ty,
            ))
            .await
        {
            Ok(_) => tracing::debug!(field, "ensure_facts_index: ok"),
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("already exists") || msg.contains("AlreadyExists") {
                    tracing::debug!(field, "ensure_facts_index: already exists");
                } else {
                    tracing::warn!(field, error = %e, "ensure_facts_index: failed");
                }
            }
        }
    }
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

/// Reserved library names a caller may not create: the internal procedure
/// namespace, and the collection-name strings (so a `library` tag can't be
/// confused with an internal collection). Untrusted callers reach this via the
/// HTTP `/library/create` route, so the guard lives here, not at the CLI.
const RESERVED_LIBRARY_NAMES: &[&str] = &[
    PROCEDURE_LIBRARY,
    MEMORIES_COLLECTION,
    FACTS_COLLECTION,
    PREDICATES_COLLECTION,
    PROCSTATS_COLLECTION,
];

/// Validate an untrusted library name. Keeps the registry sane (bounded length,
/// safe charset) and blocks reserved names. A library name is a payload tag, not
/// a Qdrant collection, so this is hygiene + namespace protection, not a clobber
/// guard — but it's the right place to stop both.
fn validate_library_name(name: &str) -> Result<()> {
    let n = name.trim();
    if n.is_empty() {
        anyhow::bail!("library name must not be empty");
    }
    if n.chars().count() > 128 {
        anyhow::bail!("library name too long (max 128 chars)");
    }
    if !n
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
    {
        anyhow::bail!("library name may only contain [A-Za-z0-9._-]");
    }
    if RESERVED_LIBRARY_NAMES
        .iter()
        .any(|r| r.eq_ignore_ascii_case(n))
    {
        anyhow::bail!("'{n}' is a reserved library name");
    }
    Ok(())
}

pub async fn create_library(config: &MindConfig, name: &str) -> Result<()> {
    validate_library_name(name)?;
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
             (model '{}' may have changed - run `mgimind reindex`)",
            embedding.len(),
            config.vector_size,
            config.model_name
        );
    }
    Ok(())
}

/// v2.0 fail-closed embedding-space guard. Samples stored memory points and, if
/// any carries an `embed_model` stamp that disagrees with the configured model,
/// refuses to start: a same-dimension model swap silently returns garbage
/// neighbours (check_dim only catches a dimension change). Absence of a stamp is
/// never a mismatch — legacy points and facts predate stamping, so an unstamped
/// corpus starts normally. Off the hot path; runs once at startup.
pub async fn assert_embedding_space(config: &MindConfig) -> Result<()> {
    let client = match get_client(config).await {
        Ok(c) => c,
        // No Qdrant reachable yet (fresh install): nothing to check.
        Err(_) => return Ok(()),
    };
    if !client
        .collection_exists(MEMORIES_COLLECTION)
        .await
        .unwrap_or(false)
    {
        return Ok(());
    }
    // Sample only STAMPED points (must_not is_empty). Sampling unfiltered would
    // let a large legacy unstamped corpus hide the handful of newly-stamped points
    // behind a model swap, so the guard would pass on a corrupted store — exactly
    // the failure it exists to catch. Filtering makes detection fire the moment any
    // stamped point disagrees.
    let builder = ScrollPointsBuilder::new(MEMORIES_COLLECTION)
        .filter(Filter {
            must_not: vec![Condition::is_empty("embed_model")],
            ..Default::default()
        })
        .limit(64)
        .with_payload(true);
    let response = match client.scroll(builder).await {
        Ok(r) => r,
        // A transient scroll error must not brick startup; the dim guard on the
        // write path still protects correctness.
        Err(_) => return Ok(()),
    };
    for point in &response.result {
        if let Some(stamp) = extract_string(&point.payload, "embed_model")
            && stamp != config.model_name
        {
            anyhow::bail!(
                "embedding-model mismatch: stored points were embedded with '{}', \
                 config now uses '{}'. Search would return garbage neighbours. Run \
                 `mgimind reindex` to re-embed, or restore the previous model.",
                stamp,
                config.model_name
            );
        }
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

/// Read SEVERAL payload fields of one point in a SINGLE round-trip — for callers
/// that need a few fields of the same point (e.g. the s/p/o triple of a fact),
/// instead of one `get_points` per field. Returns one `Option<String>` per key,
/// in order; an absent point yields all `None`.
pub(crate) async fn existing_payload_strings(
    client: &Qdrant,
    collection: &str,
    id: &str,
    keys: &[&str],
) -> Vec<Option<String>> {
    let pid: qdrant_client::qdrant::PointId = id.to_string().into();
    let payload = client
        .get_points(GetPointsBuilder::new(collection, vec![pid]).with_payload(true))
        .await
        .ok()
        .and_then(|resp| resp.result.into_iter().next())
        .map(|p| p.payload);
    keys.iter()
        .map(|k| payload.as_ref().and_then(|pl| extract_string(pl, k)))
        .collect()
}

/// True if the given id is a stored procedure (`type = procedure`). Used by
/// `mind_outcome` to decide whether a test/compile signal should also bump the
/// procedural success/fail counters.
pub async fn is_procedure(config: &MindConfig, id: &str) -> Result<bool> {
    let client = get_client(config).await?;
    Ok(
        existing_payload_string(&client, MEMORIES_COLLECTION, id, "type")
            .await
            .as_deref()
            == Some(TYPE_PROCEDURE),
    )
}

/// Full payload of one memory core, by id — the lazy "zoom inside the core"
/// fetch for the graph viewer. Returns the content + metadata fields, or None
/// if the id is not a memory point.
pub async fn memory_detail(
    config: &MindConfig,
    id: &str,
) -> Result<Option<HashMap<String, String>>> {
    let client = get_client(config).await?;
    Ok(read_point_payload_strings(
        &client,
        MEMORIES_COLLECTION,
        id,
        &[
            "content",
            "library",
            "type",
            "source",
            "author",
            "created_at",
            "updated_at",
        ],
    )
    .await)
}

/// True if `content` already exists in `library` as a LIVE (non-quarantined)
/// memory. Because ids are content-addressed, a re-asserted memory lands on the
/// same id; this lets ingest tell "already kept" apart from "new" so it never
/// demotes a kept memory into quarantine on a low-novelty re-write. Best-effort:
/// a fetch failure returns false (treat as "not known live"), never an error
/// that would abort the write path.
///
/// Single-chunk assumption: this checks `deterministic_id(library, trimmed)`,
/// which only equals a stored point when the content fits in ONE chunk (the
/// common case for facts/notes). Content longer than `CHUNK_CHARS` is stored as
/// N per-chunk points with no whole-content id, so this returns false for it —
/// a long re-assertion is not recognized here (it falls through to the normal
/// quarantine path). Acceptable: low_novelty rarely fires on long content.
pub async fn live_memory_exists(config: &MindConfig, library: &str, content: &str) -> bool {
    let id = deterministic_id(library, content.trim());
    let Ok(client) = get_client(config).await else {
        return false;
    };
    let pid: qdrant_client::qdrant::PointId = id.into();
    let Ok(resp) = client
        .get_points(
            qdrant_client::qdrant::GetPointsBuilder::new(MEMORIES_COLLECTION, vec![pid])
                .with_payload(true),
        )
        .await
    else {
        return false;
    };
    match resp.result.into_iter().next() {
        // Present and NOT quarantined → it's a live memory we already keep.
        Some(point) => !extract_bool(&point.payload, "quarantined").unwrap_or(false),
        None => false,
    }
}

/// Edit a memory core's content (the viewer's edit mode). Because point IDs are
/// content-addressed (`deterministic_id`), changing the text changes the ID — so
/// an edit is "delete the old point, write the new content" while CARRYING OVER
/// the library/author so the core keeps its place and attribution. Returns the
/// new id. Errors if the id is not an existing memory.
pub async fn edit_memory(config: &MindConfig, id: &str, new_content: &str) -> Result<String> {
    let detail = memory_detail(config, id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("no memory with id {id}"))?;
    let library = detail
        .get("library")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("memory {id} has no library"))?;
    let author = detail.get("author").cloned();
    // Write the new content (carries author), then delete the old point.
    add_memory_authored(config, &library, new_content, None, author.as_deref()).await?;
    delete_memory(config, &library, id).await?;
    Ok(deterministic_id(&library, new_content.trim()))
}

/// v1.6 step 1: batched payload read — one `get_points` call returns
/// every requested string-shaped payload field for a single point.
///
/// Replaces N round-trips of `existing_payload_string` with one.
/// The v1.5 retest_fact_step82 made four such calls per fact; at
/// BACKGROUND_PER_TICK_CAP=50 that's 200 Qdrant round-trips per
/// tick. After this lands the same tick costs 50 round-trips.
///
/// Returns a HashMap. Missing keys are simply absent — the caller
/// applies defaults the same way they would after the per-key API.
/// Returns None on a Qdrant fetch error (point missing, collection
/// gone, network blip). Callers fall back to defaults uniformly,
/// which preserves the v1.5 best-effort semantics.
pub(crate) async fn read_point_payload_strings(
    client: &Qdrant,
    collection: &str,
    id: &str,
    keys: &[&str],
) -> Option<HashMap<String, String>> {
    let pid: qdrant_client::qdrant::PointId = id.to_string().into();
    let resp = client
        .get_points(GetPointsBuilder::new(collection, vec![pid]).with_payload(true))
        .await
        .ok()?;
    let point = resp.result.into_iter().next()?;
    let mut out: HashMap<String, String> = HashMap::with_capacity(keys.len());
    for &key in keys {
        if let Some(value) = extract_string(&point.payload, key) {
            out.insert(key.to_string(), value);
        }
    }
    Some(out)
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
    add_memory_authored(config, library, content, source, None).await
}

/// Like `add_memory` but records which agent wrote the memory (payload `author`
/// field). The point ID is unchanged — `author` is provenance, not identity —
/// so a second agent writing identical content still lands on the same point
/// (idempotent), with the latest writer's author tag. Use from the multi-agent
/// HTTP surface; the plain `add_memory` keeps single-agent callers untouched.
pub async fn add_memory_authored(
    config: &MindConfig,
    library: &str,
    content: &str,
    source: Option<&str>,
    author: Option<&str>,
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

    tracing::debug!(library, "add_memory: get_client start");
    let client = get_client(config).await?;
    tracing::debug!(
        library,
        "add_memory: get_client done; ensure_memories_collection start"
    );
    ensure_memories_collection(&client, config.vector_size).await?;
    tracing::debug!(library, "add_memory: ensure_memories_collection done");

    // Chunk, trimming each fragment and dropping trivially short ones. Trimming
    // the STORED chunk (not just testing the trimmed length) is what keeps the
    // content-addressed id trim-stable: `deterministic_id` of a stored chunk then
    // matches `quarantine_id_for` and `live_memory_exists`, which both trim. Skip
    // it and a whitespace-padded re-assertion lands on a different id and dodges
    // the dedup/re-assertion guards entirely.
    let chunks: Vec<String> = chunk_text(content, CHUNK_CHARS)
        .into_iter()
        .map(|c| c.trim().to_string())
        .filter(|c| c.chars().count() >= 3)
        .collect();
    if chunks.is_empty() {
        return Ok(0);
    }

    // Embed every chunk in a single batched pass (audit #2).
    tracing::debug!(
        library,
        n_chunks = chunks.len(),
        "add_memory: embed_passages start"
    );
    let embeddings = embedder::embed_passages(config, &chunks).await?;
    tracing::debug!(library, "add_memory: embed_passages done");
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
                author,
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
    // When an author is present (per-agent HTTP token, multi-agent runs), stamp
    // it as the actor so the audit log self-sources the "who" — the Track-3
    // "prove every decision" trail then needs no join against the payload author
    // index. Absent an author (CLI/MCP single user) the actor defaults to "cli".
    let mut event = crate::audit::AuditEvent::new(crate::audit::AuditOp::Add, library, "")
        .after(truncate_for_audit(content))
        .note(format!("{stored} chunks"));
    if let Some(a) = author {
        event = event.actor(a);
    }
    crate::audit::record(event);
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
        None,
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

    // Use the dedicated Quarantine op (not Add) so the "where did writes go"
    // tally can tell a quarantined candidate apart from a real store — this is
    // the only audit event for a quarantine, content + reason included.
    crate::audit::record(
        crate::audit::AuditEvent::new(crate::audit::AuditOp::Quarantine, library, &id)
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

/// Expire (delete) a quarantined entry — the explicit "yes, the gate was right
/// to reject this" verb. Guarded: it only deletes a point that is ACTUALLY
/// quarantined, so this surface can never remove a live memory (that stays the
/// job of `delete_memory`). The audit entry keeps the original
/// `quarantine_reason` and a content snapshot, so an accidental expire is
/// recoverable from the log and the reason becomes a real negative-label
/// signal about which gate rule fired. Returns false if the id is unknown or
/// the point is not quarantined.
pub async fn expire_from_quarantine(config: &MindConfig, id: &str) -> Result<bool> {
    let client = get_client(config).await?;
    let pid: qdrant_client::qdrant::PointId = id.to_string().into();
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
    if !extract_bool(&point.payload, "quarantined").unwrap_or(false) {
        // Not quarantined — refuse, so this can't be used to delete live memory.
        return Ok(false);
    }
    let library = extract_string(&point.payload, "library").unwrap_or_default();
    let reason = extract_string(&point.payload, "quarantine_reason").unwrap_or_default();
    let content = extract_string(&point.payload, "content").unwrap_or_default();

    // Record the audit event (with the content snapshot already in hand) BEFORE
    // the delete, so a deletion is never left unrecorded — the verb advertises
    // recoverability, so the record has to land first.
    let mut ev = crate::audit::AuditEvent::new(crate::audit::AuditOp::Delete, library, id)
        .actor("relevance-gate")
        .note(format!("expired from quarantine (reason: {reason})"));
    if !content.is_empty() {
        ev = ev.before(truncate_for_audit(&content));
    }
    crate::audit::record(ev);

    // Delete conditional on (this id AND still quarantined), not a bare id, so a
    // concurrent promote between the fetch above and here can't make us delete a
    // now-live point (closes the TOCTOU window at zero extra round-trip).
    let guarded = Filter {
        must: vec![
            Condition::has_id(vec![pid]),
            Condition::matches("quarantined", true),
        ],
        ..Default::default()
    };
    client
        .delete_points(
            DeletePointsBuilder::new(MEMORIES_COLLECTION)
                .points(guarded)
                .wait(true),
        )
        .await
        .context("Failed to expire quarantined entry")?;
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

/// Result of a paginated quarantine list. `next_cursor` is None when the
/// caller has reached the end; otherwise pass it back in to get the next
/// page. Cursor is opaque (a Qdrant point-id sentinel) — callers should not
/// inspect it.
#[derive(Debug, Clone)]
pub struct QuarantinePage {
    pub entries: Vec<QuarantineEntry>,
    pub next_cursor: Option<String>,
}

/// List quarantined entries, newest first, with cursor-based pagination.
/// `library = None` lists across all libraries; otherwise scoped. The store's
/// `must_not quarantined=true` on normal search means this is the only surface
/// that ever returns these. Pass `cursor = None` for the first page; the
/// returned `next_cursor` (if any) feeds back in for the next page.
pub async fn quarantine_list_page(
    config: &MindConfig,
    library: Option<&str>,
    limit: usize,
    cursor: Option<&str>,
) -> Result<QuarantinePage> {
    let client = get_client(config).await?;
    if !client
        .collection_exists(MEMORIES_COLLECTION)
        .await
        .unwrap_or(false)
    {
        return Ok(QuarantinePage {
            entries: Vec::new(),
            next_cursor: None,
        });
    }

    let filter = match library {
        Some(lib) => quarantine_filter(lib),
        None => Filter::must([Condition::matches("quarantined", true)]),
    };

    // Qdrant's ordered scroll does NOT populate next_page_offset (cursor
    // pagination is for unordered scroll only). We implement cursor manually
    // by passing the last seen `created_at` of the previous page as
    // `start_from` on the next call. The cursor string is the RFC3339
    // timestamp of the last returned entry; the next call resumes strictly
    // before it (Desc order). Fetch limit+1 to detect end-of-data without an
    // extra round-trip — drop the extra before returning.
    let order = OrderBy {
        key: "created_at".to_string(),
        direction: Some(Direction::Desc as i32),
        start_from: cursor
            .map(|c| qdrant_client::qdrant::start_from::Value::Datetime(c.to_string()).into()),
    };

    let fetch_limit = limit + 1;
    let builder = ScrollPointsBuilder::new(MEMORIES_COLLECTION)
        .filter(filter)
        .limit(fetch_limit as u32)
        .with_payload(true)
        .order_by(order);

    let response = client
        .scroll(builder)
        .await
        .context("quarantine scroll failed")?;

    let mut points = response.result;
    // If we got more than `limit`, there's a next page; the extra row's
    // created_at becomes the cursor and we drop it from the returned set.
    let next_cursor = if points.len() > limit {
        points
            .pop()
            .and_then(|p| extract_string(&p.payload, "created_at"))
    } else {
        None
    };

    let entries = points
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

    Ok(QuarantinePage {
        entries,
        next_cursor,
    })
}

/// Backwards-compatible single-page lister. Used by the CLI/MCP surfaces
/// where pagination doesn't make sense (one screenful is enough); the
/// HTTP/UI surface uses `quarantine_list_page` directly.
pub async fn quarantine_list(
    config: &MindConfig,
    library: Option<&str>,
    limit: usize,
) -> Result<Vec<QuarantineEntry>> {
    let page = quarantine_list_page(config, library, limit, None).await?;
    Ok(page.entries)
}

/// How many memories are quarantined right now. Used by `mind_context` to make
/// the quarantine surface visible (otherwise it is a black hole no agent ever
/// checks). Best-effort: 0 on any error so context-building never fails.
pub async fn quarantine_count(config: &MindConfig) -> Result<u64> {
    let client = get_client(config).await?;
    let filter = Filter {
        must: vec![Condition::matches("quarantined", true)],
        ..Default::default()
    };
    let resp = client
        .count(
            qdrant_client::qdrant::CountPointsBuilder::new(MEMORIES_COLLECTION)
                .filter(filter)
                .exact(true),
        )
        .await
        .context("quarantine count")?;
    Ok(resp.result.map(|r| r.count).unwrap_or(0))
}

/// Count quarantined entries grouped by gate reason. Returns `(reason, count)`
/// pairs sorted by count descending, omitting zero-count reasons, plus a final
/// `("other", n)` pair when some quarantined points carry a reason outside the
/// canonical `relevance::KNOWN_REASONS` set (so nothing is silently dropped from
/// the tally). Each reason is an exact Qdrant count — bounded (≈8 cheap queries),
/// not a full scan. `total` is returned separately so callers don't re-count.
pub async fn quarantine_reason_breakdown(config: &MindConfig) -> Result<(Vec<(String, u64)>, u64)> {
    let client = get_client(config).await?;
    let total = quarantine_count(config).await?;
    if total == 0 {
        return Ok((Vec::new(), 0));
    }
    let mut counts: Vec<(String, u64)> = Vec::new();
    let mut known_sum = 0u64;
    for reason in crate::relevance::KNOWN_REASONS {
        let filter = Filter {
            must: vec![
                Condition::matches("quarantined", true),
                Condition::matches("quarantine_reason", reason.to_string()),
            ],
            ..Default::default()
        };
        let resp = client
            .count(
                qdrant_client::qdrant::CountPointsBuilder::new(MEMORIES_COLLECTION)
                    .filter(filter)
                    .exact(true),
            )
            .await
            .with_context(|| format!("quarantine count for reason {reason}"))?;
        let n = resp.result.map(|r| r.count).unwrap_or(0);
        if n > 0 {
            counts.push((reason.to_string(), n));
            known_sum += n;
        }
    }
    counts.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    // Anything quarantined with a reason we don't enumerate (or none at all)
    // lands in "other" so the breakdown always reconciles with `total`.
    if total > known_sum {
        counts.push(("other".to_string(), total - known_sum));
    }
    Ok((counts, total))
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
fn extract_bool(
    payload: &HashMap<String, qdrant_client::qdrant::Value>,
    key: &str,
) -> Option<bool> {
    payload.get(key).and_then(|v| {
        v.kind.as_ref().and_then(|k| match k {
            qdrant_client::qdrant::value::Kind::BoolValue(b) => Some(*b),
            _ => None,
        })
    })
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
                None,
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

#[allow(clippy::too_many_arguments)]
fn build_payload(
    content: &str,
    hash: &str,
    created_at: &str,
    updated_at: &str,
    library: &str,
    source: Option<&str>,
    mem_type: &str,
    author: Option<&str>,
) -> HashMap<String, qdrant_client::qdrant::Value> {
    build_payload_full(
        content, hash, created_at, updated_at, library, source, mem_type, None, None, author,
    )
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
    author: Option<&str>,
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
    // Author = which agent wrote this (provenance, NOT identity). Kept out of
    // deterministic_id deliberately: the point ID stays content-addressed so
    // re-ingest idempotency, quarantine-promote, and _links resolution are
    // unaffected. Legacy points simply lack the key.
    if let Some(a) = author {
        payload.insert("author".into(), a.into());
    }
    // v2.0 embedding-space stamp: record which model embedded this point. A model
    // swap that keeps the same vector dimension is invisible to check_dim (the
    // dims still match) yet makes search return garbage neighbours; startup samples
    // these stamps and refuses to run on a mismatch. Read from the process-global
    // cached config (the one the running server embeds with); if it can't be read
    // the point is left unstamped, which the startup guard treats as "unknown",
    // never as a mismatch.
    if let Ok(cfg) = crate::config::load_cached() {
        payload.insert("embed_model".into(), cfg.model_name.as_str().into());
    }
    payload
}

/// Library-scoped semantic search — the original surface, unchanged for the
/// existing callers. Thin wrapper over `search_filtered`.
/// Per-query override of the reranker config, so an agent can ask for the raw
/// hybrid order (no rerank) or a different rerank depth on ONE query without
/// touching global config. `None` fields fall back to `config.rerank_*`, so the
/// default (all-None) is byte-identical to the pre-override behavior.
#[derive(Debug, Default, Clone, Copy)]
pub struct RerankOverride {
    /// `Some(true/false)` forces rerank on/off for this query; `None` = config.
    pub enabled: Option<bool>,
    /// `Some(k)` overrides how many candidates the reranker re-orders; `None` =
    /// config.
    pub top_k: Option<usize>,
}

impl RerankOverride {
    /// Resolve against config: returns (enabled, top_k) actually used this query.
    fn resolve(&self, config: &MindConfig) -> (bool, usize) {
        (
            self.enabled.unwrap_or(config.rerank_enabled),
            self.top_k.unwrap_or(config.rerank_top_k),
        )
    }
}

pub async fn search(
    config: &MindConfig,
    query: &str,
    library: Option<&str>,
    limit: usize,
    tier: u8,
) -> Result<Vec<SearchResult>> {
    search_filtered(
        config,
        query,
        &MemoryFilter::for_library(library),
        limit,
        tier,
        RerankOverride::default(),
    )
    .await
}

/// Semantic search with optional query-time metadata filters (author, type,
/// source, date window, multi-library OR). All filtering runs in-process against
/// the bundled Qdrant — no data leaves the machine. With an empty filter this is
/// byte-for-byte the old `search` behavior.
pub async fn search_filtered(
    config: &MindConfig,
    query: &str,
    mfilter: &MemoryFilter,
    limit: usize,
    tier: u8,
    rerank: RerankOverride,
) -> Result<Vec<SearchResult>> {
    let (rerank_enabled, rerank_top_k) = rerank.resolve(config);
    // Live read pulse for the viewer (one per search, not per result). Cheap
    // when no viewer is open (broadcast drops to no subscribers).
    crate::pulse::emit(crate::pulse::PulseEvent::new(
        crate::pulse::PulseKind::Read,
        match mfilter.libraries.as_slice() {
            [] => "search".into(),
            [one] => format!("lib:{one}"),
            many => format!("lib:{}", many.join("+")),
        },
        {
            let q: String = query.chars().take(40).collect();
            format!("search: {q}")
        },
    ));

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
    let fetch_k = if rerank_enabled {
        rerank_top_k.max(limit)
    } else {
        limit
    } as u64;

    // Hybrid retrieval (audit #23): dense (semantic) + sparse (BM25) prefetches
    // fused with Reciprocal Rank Fusion. Both arms exclude procedures and apply
    // the optional library scope (phase Д6).
    let mf = memory_query_filter_ex(mfilter)?;
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
                author: extract_string(payload, "author"),
                created_at: extract_string(payload, "created_at"),
                score: point.score,
            }
        })
        .collect();

    // Cross-encoder rerank (audit #22). Best-effort: on any reranker failure the
    // dense order is kept (reranking is a quality boost, not a dependency).
    if rerank_enabled && cands.len() > 1 {
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

/// Fetch an existing procedure's preserved fields (created_at from the core
/// point; counts + verified from the derived `_mod_procstats` collection per
/// ADR 0006) so a re-learn keeps history instead of resetting it. Returns
/// `(created_at, success_count, fail_count, verified)`.
///
/// Upgrade safety: a pre-Step-10 procedure carries its counts on the CORE point
/// and has no `_mod_procstats` row. Seed from the core fields, then override with
/// the side row when present — so a re-learn of a legacy procedure preserves its
/// real history instead of resetting it to zero (then `add_procedure` writes it
/// back into the side collection, healing it forward).
async fn existing_procedure(client: &Qdrant, id: &str) -> (Option<String>, i64, i64, bool) {
    let pid: qdrant_client::qdrant::PointId = id.to_string().into();
    let (created_at, mut succ, mut fail, mut verified) = match client
        .get_points(GetPointsBuilder::new(MEMORIES_COLLECTION, vec![pid]).with_payload(true))
        .await
    {
        Ok(resp) => match resp.result.into_iter().next() {
            Some(p) => (
                extract_string(&p.payload, "created_at"),
                extract_int(&p.payload, "success_count").unwrap_or(0),
                extract_int(&p.payload, "fail_count").unwrap_or(0),
                extract_string(&p.payload, "verified").as_deref() == Some("true"),
            ),
            None => (None, 0, 0, false),
        },
        Err(_) => (None, 0, 0, false),
    };
    let stats = procstats_one(client, id).await;
    // The side row, when present, is the live source — it overrides the legacy seed.
    if stats.success_count != 0 || stats.fail_count != 0 || stats.verified {
        succ = stats.success_count;
        fail = stats.fail_count;
        verified = stats.verified;
    }
    (created_at, succ, fail, verified)
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
    let (existing_created, mut succ, mut fail, mut was_verified) =
        existing_procedure(&client, &id).await;
    // Genuinely-new procedure (no core point at this id): any _mod_procstats row
    // is a stale orphan from a procedure that was deleted then re-learned with the
    // same (error, fix). The delete was an explicit "drop this history" signal, so
    // do not resurrect it — clear the orphan and start fresh.
    if existing_created.is_none() {
        delete_procstats(&client, &id).await;
        succ = 0;
        fail = 0;
        was_verified = false;
    }
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
    // Derived stats (counts, verified) live in _mod_procstats (ADR 0006), NOT on
    // this core point. created_at/updated_at are raw lifecycle fields and stay.
    payload.insert("created_at".into(), created_at.into());
    payload.insert("updated_at".into(), now.clone().into());

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

    // verified latches: once a real signal set it true, a manual re-learn won't
    // unset it. Persist counts + verified to the side collection so a re-learn
    // preserves history. Only write when there's history to preserve or this
    // learn carries the verified signal, so a plain manual learn doesn't create
    // an all-zero stats row.
    let verified = verified || was_verified;
    if verified || succ != 0 || fail != 0 {
        ensure_procstats_collection(&client)
            .await
            .context("ensure _mod_procstats")?;
        let mut stats: HashMap<String, qdrant_client::qdrant::Value> = HashMap::new();
        stats.insert("success_count".into(), succ.into());
        stats.insert("fail_count".into(), fail.into());
        stats.insert(
            "verified".into(),
            if verified { "true" } else { "false" }.into(),
        );
        stats.insert("last_used".into(), now.into());
        let spoint = PointStruct::new(id.clone(), NamedVectors::default(), stats);
        client
            .upsert_points(UpsertPointsBuilder::new(PROCSTATS_COLLECTION, vec![spoint]).wait(true))
            .await
            .context("Failed to persist procedure stats")?;
    }
    Ok(id)
}

/// Recall procedures matching an error signature and/or a task context. `norm_error`
/// (already normalized) drives the sparse/lexical arm; `context` drives the dense/
/// semantic arm. Returns hits with [0,1]-normalized scores + ranking signals;
/// the caller orders them by combined relevance + trust. Pure retrieval - no mutation.
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

    let mut hits: Vec<ProcedureHit> = response
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
                // Seed trust signals from LEGACY core-point fields. Post-upgrade
                // procedures have none here (zeros) and get them from
                // _mod_procstats below; PRE-upgrade procedures still carry them on
                // the core point and would otherwise reset to unverified on the
                // first recall after this change. The side-collection read below
                // overrides when a row exists, so this is a no-data-loss fallback,
                // not a second source of truth.
                verified: extract_string(p, "verified").as_deref() == Some("true"),
                success_count: extract_int(p, "success_count").unwrap_or(0),
                fail_count: extract_int(p, "fail_count").unwrap_or(0),
                score: point.score,
            }
        })
        .collect();

    // Batch-fetch derived stats for the candidate ids from _mod_procstats. O(k)
    // by id-set, not a scan. Existence-guarded + default-on-miss. When a row
    // exists it OVERRIDES the legacy seed (the side collection is the live source
    // for any procedure touched since the upgrade); when absent, the legacy seed
    // stands. Dropping the collection leaves post-upgrade procedures at zeros
    // (degrade, never error) while pre-upgrade ones keep their real historical
    // counts — neither is a ghost.
    let ids: Vec<String> = hits.iter().map(|h| h.id.clone()).collect();
    let stats = procstats_for(&client, &ids).await;
    for h in &mut hits {
        if let Some(s) = stats.get(&h.id) {
            h.verified = s.verified;
            h.success_count = s.success_count;
            h.fail_count = s.fail_count;
        }
    }

    // Normalize the raw RRF scores into [0,1]. RRF fractions are tiny (~0.016
    // for a rank-0 hit, k=60), so the verified/worked boosts in `rank_score`
    // (tuned for a [0,1] base) would otherwise dwarf relevance and turn the
    // weighted rank back into a hard verified-first gate. Min-max within the
    // returned set keeps relevance and the boosts on the same scale.
    let max = hits.iter().map(|h| h.score).fold(0.0f32, f32::max);
    if max > 0.0 {
        for h in &mut hits {
            h.score /= max;
        }
    }
    Ok(hits)
}

/// Scroll all procedures (memories with `type=procedure`) for Phase 1.3
/// backfill. Returns `(point_id, success_count)` pairs — Phase 1.3 uses
/// the success_count as the derivable confirmation signal. Failures are
/// not subtracted here (Phase 4 will calibrate whether net-positive vs
/// raw-positive performs better as a confirmation count).
pub async fn list_procedures_for_backfill(config: &MindConfig) -> Result<Vec<(String, i64)>> {
    let client = get_client(config).await?;
    if !client
        .collection_exists(MEMORIES_COLLECTION)
        .await
        .unwrap_or(false)
    {
        return Ok(Vec::new());
    }

    let filter = Filter {
        must: vec![Condition::matches("type", "procedure".to_string())],
        ..Default::default()
    };

    let mut out = Vec::new();
    let mut offset: Option<qdrant_client::qdrant::PointId> = None;
    loop {
        let mut builder = ScrollPointsBuilder::new(MEMORIES_COLLECTION)
            .filter(filter.clone())
            .limit(256)
            .with_payload(true);
        if let Some(o) = offset.clone() {
            builder = builder.offset(o);
        }
        let response = client.scroll(builder).await?;
        for point in &response.result {
            let id = point.id.as_ref().map(format_point_id).unwrap_or_default();
            // Legacy seed from the core point (pre-Step-10 procedures); overridden
            // below by _mod_procstats for any procedure with stats since.
            let succ = extract_int(&point.payload, "success_count").unwrap_or(0);
            out.push((id, succ));
        }
        match response.next_page_offset {
            Some(next) => offset = Some(next),
            None => break,
        }
    }
    // Post-Step-10 (ADR 0006) counts live in _mod_procstats; override the legacy
    // core-point seed so this backfill isn't half-blind to procedures touched
    // since the move. Existence-guarded — absent collection leaves the seed.
    let ids: Vec<String> = out.iter().map(|(id, _)| id.clone()).collect();
    let stats = procstats_for(&client, &ids).await;
    for (id, succ) in &mut out {
        if let Some(s) = stats.get(id) {
            *succ = s.success_count;
        }
    }
    Ok(out)
}

/// Scroll every VERIFIED procedure with its full trigger/fix payload, for the
/// `mgimind export instructions` render. Mirrors `list_procedures_for_backfill`'s
/// pagination, then overrides the legacy core-point `verified`/counts with the
/// live `_mod_procstats` row (ADR 0006) before keeping only verified ones,
/// most-proven first. Existence-guarded: an absent collection yields an empty
/// list, never an error.
pub async fn list_verified_procedures(config: &MindConfig) -> Result<Vec<ProcedureHit>> {
    let client = get_client(config).await?;
    if !client
        .collection_exists(MEMORIES_COLLECTION)
        .await
        .unwrap_or(false)
    {
        return Ok(Vec::new());
    }

    let filter = Filter {
        must: vec![Condition::matches("type", "procedure".to_string())],
        ..Default::default()
    };

    let mut all: Vec<ProcedureHit> = Vec::new();
    let mut offset: Option<qdrant_client::qdrant::PointId> = None;
    loop {
        let mut builder = ScrollPointsBuilder::new(MEMORIES_COLLECTION)
            .filter(filter.clone())
            .limit(256)
            .with_payload(true);
        if let Some(o) = offset.clone() {
            builder = builder.offset(o);
        }
        let response = client.scroll(builder).await?;
        for point in &response.result {
            let p = &point.payload;
            all.push(ProcedureHit {
                id: point.id.as_ref().map(format_point_id).unwrap_or_default(),
                trigger_error: extract_string(p, "trigger_error").unwrap_or_default(),
                trigger_context: extract_string(p, "trigger_context").unwrap_or_default(),
                fix: extract_string(p, "fix").unwrap_or_default(),
                provenance: extract_string(p, "provenance"),
                // Legacy core-point seed; overridden by _mod_procstats below.
                verified: extract_string(p, "verified").as_deref() == Some("true"),
                success_count: extract_int(p, "success_count").unwrap_or(0),
                fail_count: extract_int(p, "fail_count").unwrap_or(0),
                score: 0.0,
            });
        }
        match response.next_page_offset {
            Some(next) => offset = Some(next),
            None => break,
        }
    }

    // The side-collection row, when present, is the live source of truth — it
    // overrides the legacy seed, exactly as `recall_procedures` does.
    let ids: Vec<String> = all.iter().map(|h| h.id.clone()).collect();
    let stats = procstats_for(&client, &ids).await;
    for hit in &mut all {
        let Some(s) = stats.get(&hit.id) else {
            continue;
        };
        if s.success_count != 0 || s.fail_count != 0 || s.verified {
            hit.success_count = s.success_count;
            hit.fail_count = s.fail_count;
            hit.verified = s.verified;
        }
    }

    all.retain(|h| h.verified);
    all.sort_by_key(|h| std::cmp::Reverse(h.success_count));
    Ok(all)
}

/// v1.5 Phase 6 step 6.2: scroll every procedure and return its
/// `last_used` timestamp (RFC3339), if present. The install-mode
/// detector counts procedures with `last_used` inside a recent
/// window as a proxy for external-signal frequency.
///
/// Returns an empty vec when the collection doesn't exist yet
/// (fresh install). Procedures without `last_used` are skipped —
/// they have never been re-used so they don't count toward the
/// signal-frequency proxy.
pub async fn list_procedure_last_used(config: &MindConfig) -> Result<Vec<String>> {
    let client = get_client(config).await?;
    if !client
        .collection_exists(MEMORIES_COLLECTION)
        .await
        .unwrap_or(false)
    {
        return Ok(Vec::new());
    }

    let filter = Filter {
        must: vec![Condition::matches("type", "procedure".to_string())],
        ..Default::default()
    };

    let mut out = Vec::new();
    let mut offset: Option<qdrant_client::qdrant::PointId> = None;
    loop {
        let mut builder = ScrollPointsBuilder::new(MEMORIES_COLLECTION)
            .filter(filter.clone())
            .limit(256)
            .with_payload(true);
        if let Some(o) = offset.clone() {
            builder = builder.offset(o);
        }
        let response = client.scroll(builder).await?;
        for point in &response.result {
            if let Some(ts) = extract_string(&point.payload, "last_used")
                && !ts.is_empty()
            {
                out.push(ts);
            }
        }
        match response.next_page_offset {
            Some(next) => offset = Some(next),
            None => break,
        }
    }
    Ok(out)
}

/// Set a payload field on a memory by id. Phase 1.3 uses this to write
/// `confirmations_count` back into procedure points (and possibly other
/// memory types in a future revision). The shape mirrors
/// `knowledge::set_fact_payload_field` but lives over MEMORIES_COLLECTION.
pub async fn set_memory_payload_field(
    config: &MindConfig,
    memory_id: &str,
    field: &str,
    value: String,
) -> Result<()> {
    use qdrant_client::qdrant::SetPayloadPointsBuilder;
    let client = get_client(config).await?;
    let mut payload: HashMap<String, qdrant_client::qdrant::Value> = HashMap::new();
    payload.insert(field.into(), value.into());
    let point_id: qdrant_client::qdrant::PointId = memory_id.to_string().into();
    client
        .set_payload(
            SetPayloadPointsBuilder::new(MEMORIES_COLLECTION, payload)
                .points_selector(PointsIdsList {
                    ids: vec![point_id],
                })
                .wait(true),
        )
        .await
        .context("Failed to set memory payload field")?;
    // v1.5 Phase 8 step 8.1D: signal graph change to the background
    // re-test loop. See knowledge::set_fact_payload_field for the
    // mirror call on the facts collection.
    crate::doubt::record_edit();
    Ok(())
}

/// Soft-forget memories by setting `archived = true`: they drop out of default
/// search (the base `must_not archived` filter) but stay in the store, fully
/// restorable. The reversible counterpart to `delete_memories` — cold-memory
/// hygiene without destroying data. Returns the count flagged.
pub async fn archive_memories(config: &MindConfig, ids: &[String]) -> Result<usize> {
    use qdrant_client::qdrant::SetPayloadPointsBuilder;
    if ids.is_empty() {
        return Ok(0);
    }
    let client = get_client(config).await?;
    let mut payload: HashMap<String, qdrant_client::qdrant::Value> = HashMap::new();
    payload.insert("archived".into(), true.into());
    let point_ids: Vec<qdrant_client::qdrant::PointId> =
        ids.iter().map(|id| id.to_string().into()).collect();
    client
        .set_payload(
            SetPayloadPointsBuilder::new(MEMORIES_COLLECTION, payload)
                .points_selector(PointsIdsList { ids: point_ids })
                .wait(true),
        )
        .await
        .context("Failed to archive memories")?;
    // One audit row per archived id — a soft-forget is reversible, so the log is
    // how you find what was hidden (and its id, to restore it).
    for id in ids {
        crate::audit::record(
            crate::audit::AuditEvent::new(
                crate::audit::AuditOp::Archive,
                "_memories".to_string(),
                id.clone(),
            )
            .actor("consolidate")
            .note("cold: archived (restorable)"),
        );
    }
    Ok(ids.len())
}

/// Restore an archived memory by id: clear `archived`, returning it to default
/// search. Returns true if the memory existed and was archived (false if not
/// found or already live). The inverse of `archive_memories`.
pub async fn restore_memory(config: &MindConfig, id: &str) -> Result<bool> {
    use qdrant_client::qdrant::{GetPointsBuilder, SetPayloadPointsBuilder};
    let client = get_client(config).await?;
    let pid: qdrant_client::qdrant::PointId = id.to_string().into();

    // Only act on a memory that is actually archived — so a typo'd id or an
    // already-live memory is reported honestly rather than silently "succeeding".
    let resp = client
        .get_points(
            GetPointsBuilder::new(MEMORIES_COLLECTION, vec![pid.clone()]).with_payload(true),
        )
        .await
        .context("Failed to look up memory for restore")?;
    let Some(point) = resp.result.into_iter().next() else {
        return Ok(false);
    };
    if !extract_bool(&point.payload, "archived").unwrap_or(false) {
        return Ok(false);
    }

    let mut payload: HashMap<String, qdrant_client::qdrant::Value> = HashMap::new();
    payload.insert("archived".into(), false.into());
    client
        .set_payload(
            SetPayloadPointsBuilder::new(MEMORIES_COLLECTION, payload)
                .points_selector(PointsIdsList { ids: vec![pid] })
                .wait(true),
        )
        .await
        .context("Failed to restore memory")?;
    crate::audit::record(
        crate::audit::AuditEvent::new(
            crate::audit::AuditOp::Restore,
            "_memories".to_string(),
            id.to_string(),
        )
        .actor("cli")
        .note("restored from archive"),
    );
    Ok(true)
}

/// v1.5 Phase 7: read the `external_signals_v15` payload field on
/// a memory and decode it as `Vec<ExternalSignal>`. Returns an empty
/// vec on a fresh memory that has never received an outcome.
///
/// The slot is intentionally NEW (not the v1.4 `external_signals: Vec<String>`)
/// because Phase 1 migration writes legacy counts into the old slot
/// and we must not lose them when Phase 7 lands. v1.6 will migrate the
/// legacy slot into the new one and drop the duplicate.
pub async fn read_external_signals(
    config: &MindConfig,
    memory_id: &str,
) -> Result<Vec<crate::outcome::ExternalSignal>> {
    let client = get_client(config).await?;
    let Some(raw) = existing_payload_string(
        &client,
        MEMORIES_COLLECTION,
        memory_id,
        "external_signals_v15",
    )
    .await
    else {
        return Ok(Vec::new());
    };
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    let signals: Vec<crate::outcome::ExternalSignal> = serde_json::from_str(&raw)
        .with_context(|| format!("invalid external_signals_v15 JSON on memory {memory_id}"))?;
    Ok(signals)
}

/// v1.5 Phase 7: write back the deduplicated signal log. Caller is
/// responsible for running `outcome::dedup_keep_latest` first — this
/// helper just persists.
pub async fn write_external_signals(
    config: &MindConfig,
    memory_id: &str,
    signals: &[crate::outcome::ExternalSignal],
) -> Result<()> {
    let serialised =
        serde_json::to_string(signals).context("failed to serialise external_signals_v15")?;
    set_memory_payload_field(config, memory_id, "external_signals_v15", serialised).await
}

/// Record the outcome of reusing a procedure: bump success or fail count and
/// stamp `last_used`. A failure (`worked = false`) raises fail_count so recall
/// can demote a fix that stopped working - the store self-corrects instead of
/// ossifying on a bad playbook.
///
/// `verify` promotes the procedure to `verified = true` on a successful
/// outcome. Pass it ONLY for a deterministic signal (a passing test, a clean
/// compile) - never for a human "seems fine". This is what closes the
/// error-learning loop: a green test after a mind_learn fix marks that playbook
/// trustworthy so recall surfaces it first. Without it, every recorded fix
/// stayed unverified forever and never ranked above noise.
/// Derived outcome stats for one procedure (ADR 0006). Lives in
/// `_mod_procstats`, not on the core procedure point. `Default` is the
/// degrade-on-miss value: zero counts, unverified — so a dropped collection
/// ranks every procedure by relevance alone, never errors.
#[derive(Debug, Clone, Default)]
pub struct ProcStats {
    pub success_count: i64,
    pub fail_count: i64,
    pub verified: bool,
}

/// Read stats for a set of procedure ids from `_mod_procstats`. Existence-guarded
/// and default-on-miss: if the collection was dropped, or an id has no row, that
/// id maps to `ProcStats::default()`. This is the read pattern ADR 0006's
/// toggle-test depends on — a bare get_points on a missing collection would error.
pub async fn procstats_for(
    client: &Qdrant,
    ids: &[String],
) -> std::collections::HashMap<String, ProcStats> {
    let mut out = std::collections::HashMap::new();
    if ids.is_empty() {
        return out;
    }
    // Dropped/never-created collection → everything defaults. Never an error.
    if !client
        .collection_exists(PROCSTATS_COLLECTION)
        .await
        .unwrap_or(false)
    {
        return out;
    }
    let pids: Vec<qdrant_client::qdrant::PointId> =
        ids.iter().map(|i| i.to_string().into()).collect();
    let Ok(resp) = client
        .get_points(GetPointsBuilder::new(PROCSTATS_COLLECTION, pids).with_payload(true))
        .await
    else {
        return out;
    };
    for point in resp.result {
        let Some(id) = point.id.as_ref().map(format_point_id) else {
            continue;
        };
        out.insert(
            id,
            ProcStats {
                success_count: extract_int(&point.payload, "success_count").unwrap_or(0),
                fail_count: extract_int(&point.payload, "fail_count").unwrap_or(0),
                verified: extract_string(&point.payload, "verified").as_deref() == Some("true"),
            },
        );
    }
    out
}

/// Read stats for a single procedure id (convenience over `procstats_for`).
async fn procstats_one(client: &Qdrant, id: &str) -> ProcStats {
    procstats_for(client, std::slice::from_ref(&id.to_string()))
        .await
        .remove(id)
        .unwrap_or_default()
}

/// Best-effort delete of a procedure's derived stats row (ADR 0006 orphan
/// cleanup). Silent on a missing collection or a failed delete — an orphan stats
/// row for a deleted procedure is never read, so this must never fail a delete.
async fn delete_procstats(client: &Qdrant, id: &str) {
    if !client
        .collection_exists(PROCSTATS_COLLECTION)
        .await
        .unwrap_or(false)
    {
        return;
    }
    let pid: qdrant_client::qdrant::PointId = id.to_string().into();
    let _ = client
        .delete_points(
            DeletePointsBuilder::new(PROCSTATS_COLLECTION)
                .points(PointsIdsList { ids: vec![pid] })
                .wait(true),
        )
        .await;
}

pub async fn procedure_outcome(
    config: &MindConfig,
    id: &str,
    worked: bool,
    verify: bool,
) -> Result<()> {
    let client = get_client(config).await?;
    // Stats are derived state: they live in _mod_procstats, not on the core
    // procedure point (ADR 0006). Read the current counts from there, bump, and
    // upsert back — so dropping the side collection drops the trust signal
    // cleanly without touching the procedure itself.
    ensure_procstats_collection(&client)
        .await
        .context("ensure _mod_procstats")?;
    let cur = procstats_one(&client, id).await;
    let now = chrono::Utc::now().to_rfc3339();

    let mut payload: HashMap<String, qdrant_client::qdrant::Value> = HashMap::new();
    // Carry both counts every time so the row is self-contained (a later read
    // of a single field doesn't depend on a prior partial upsert).
    let succ = if worked {
        cur.success_count + 1
    } else {
        cur.success_count
    };
    let fail = if worked {
        cur.fail_count
    } else {
        cur.fail_count + 1
    };
    payload.insert("success_count".into(), succ.into());
    payload.insert("fail_count".into(), fail.into());
    // verified latches true once a real signal sets it.
    let verified = cur.verified || (worked && verify);
    payload.insert(
        "verified".into(),
        if verified { "true" } else { "false" }.into(),
    );
    payload.insert("last_used".into(), now.into());

    let point = PointStruct::new(id.to_string(), NamedVectors::default(), payload);
    client
        .upsert_points(UpsertPointsBuilder::new(PROCSTATS_COLLECTION, vec![point]).wait(true))
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

    // Best-effort: drop any derived stats row for this id (ADR 0006 orphan
    // cleanup). A procedure carries one; a plain memory carries none, so this is
    // a no-op there. Non-fatal — an orphaned stats row is never read for a
    // missing procedure, so a failure here must not fail the delete.
    delete_procstats(&client, id).await;

    let mut ev = crate::audit::AuditEvent::new(crate::audit::AuditOp::Delete, _library, id);
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
    /// The library this memory lives in. Surfaced by the inventory `list` path
    /// so a cross-library listing is self-describing. Defaults to empty for the
    /// older per-library callers that already know the library from context.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub library: String,
    /// The agent that wrote it, when tagged (multi-agent writes). None for the
    /// legacy corpus and single-agent writes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// v1.4 confidence score — the cached output of the duel-rule machinery.
    /// `None` for legacy memories written before the v1.4 schema landed and
    /// for memories whose score has not yet been computed by the background
    /// re-test pass. Treated as "no calibration available, fall back to the
    /// v1.0 relevance formula" when consumed by ranking.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence_score: Option<f32>,
    /// Recency-weighted "coldness" in days since last relevance (the same metric
    /// the decay/consolidate path uses). Higher = colder = closer to being a
    /// forget candidate. Surfaced by `browse` so a user/agent can SEE the
    /// temperature of memory — pure observability, nothing is hidden or reordered
    /// by it. `None` when the timestamp can't be dated. Only computed on the
    /// inventory (browse) path, not on the hot search path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coldness: Option<f64>,
}

/// Phase 0 helper: store a confidence score into a Qdrant point payload.
///
/// The payload format mirrors the existing string-everywhere convention used
/// by the rest of `storage.rs` (Qdrant's typed payload is touched through
/// `qdrant_client::qdrant::Value`, and this codebase consistently writes
/// scalars as strings to dodge the small surface of typed-vs-untyped traps
/// the wrapper crate has). The serialized form is the f32 rendered with
/// `to_string()` so the reverse parse in `payload_get_confidence` is exact.
///
/// Allowed-dead until Phase 3 wires this into `add_memory`; landing it now
/// keeps the schema migration in one bisectable commit.
#[allow(dead_code)]
pub fn payload_set_confidence(
    payload: &mut std::collections::HashMap<String, qdrant_client::qdrant::Value>,
    score: f32,
) {
    payload.insert("confidence_score".into(), score.to_string().into());
}

/// Phase 0 helper: read a confidence score back from a Qdrant point payload.
///
/// Returns `None` for legacy points (the field was added in v1.4) and for
/// points whose stored value cannot be parsed as `f32` (which should never
/// happen unless the payload was corrupted; the parse failure is silent
/// here because the ranking layer is expected to fall back to the v1.0
/// formula in that case, not surface an error to the agent).
#[allow(dead_code)]
pub fn payload_get_confidence(
    payload: &std::collections::HashMap<String, qdrant_client::qdrant::Value>,
) -> Option<f32> {
    let v = payload.get("confidence_score")?;
    let s = match &v.kind {
        Some(qdrant_client::qdrant::value::Kind::StringValue(s)) => s.as_str(),
        _ => return None,
    };
    s.parse::<f32>().ok()
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
            point_to_record(
                p.id.as_ref().map(format_point_id).unwrap_or_default(),
                &p.payload,
            )
        })
        .collect())
}

/// Map one scroll/search point payload into a `MemoryRecord`. Shared so the
/// inventory listers don't drift on which payload keys they read.
fn point_to_record(
    id: String,
    payload: &HashMap<String, qdrant_client::qdrant::Value>,
) -> MemoryRecord {
    MemoryRecord {
        id,
        content: extract_string(payload, "content").unwrap_or_default(),
        source: extract_string(payload, "source"),
        r#type: extract_string(payload, "type").unwrap_or_else(|| "memory".into()),
        created_at: extract_string(payload, "created_at").unwrap_or_default(),
        updated_at: extract_string(payload, "updated_at").unwrap_or_default(),
        library: extract_string(payload, "library").unwrap_or_default(),
        author: extract_string(payload, "author"),
        confidence_score: payload_get_confidence(payload),
        // Set by `list_filtered` (the browse path) from the access journal; the
        // bare payload carries no coldness, so the mapper leaves it None.
        coldness: None,
    }
}

/// How many points `list_filtered` scans before sorting. Bounds the work on a
/// large collection; the newest `limit` of the scanned window are returned. A
/// browse that hits this cap is reported as truncated so the caller knows it saw
/// a window, not the whole corpus.
const BROWSE_MAX_SCAN: usize = 2000;

/// Browse memories by metadata WITHOUT a semantic query — the inventory path.
/// Same `MemoryFilter` as `search_filtered` (author, source, date window,
/// libraries), but lists newest-first instead of ranking by similarity, so
/// "show me everything agent X wrote this week" or "everything imported from
/// docs.example.com" works with no query vector. Excludes procedures and
/// quarantined points, like normal search. All local; nothing leaves the box.
///
/// Ordering is done in-memory, NOT via Qdrant `order_by`: an ordered scroll
/// silently drops points that lack the sort key, and the pre-`created_at` legacy
/// corpus has no such key — they would vanish from an "inventory" that promises
/// completeness. Here every matching point is scanned (up to `BROWSE_MAX_SCAN`),
/// then sorted by `created_at` descending with missing/empty dates sorted LAST,
/// so legacy points still appear (at the tail) instead of disappearing.
///
/// Returns the records plus a `scanned >= cap` truncation flag.
pub async fn list_filtered(
    config: &MindConfig,
    mfilter: &MemoryFilter,
    limit: usize,
) -> Result<(Vec<MemoryRecord>, bool)> {
    let client = get_client(config).await?;
    if !client
        .collection_exists(MEMORIES_COLLECTION)
        .await
        .unwrap_or(false)
    {
        return Ok((Vec::new(), false));
    }
    // No order_by: scan the whole matching set (capped) so legacy points without
    // created_at are included, then sort in-memory below.
    let response = client
        .scroll(
            ScrollPointsBuilder::new(MEMORIES_COLLECTION)
                .filter(memory_query_filter_ex(mfilter)?)
                .limit(BROWSE_MAX_SCAN as u32)
                .with_payload(true),
        )
        .await
        .context("list_filtered scroll failed")?;
    let scanned = response.result.len();
    let truncated = scanned >= BROWSE_MAX_SCAN;
    // Snapshot the access journal ONCE for this listing, then stamp each record's
    // recency-weighted coldness. Browse is the inventory path (not the hot search
    // path), so this extra read is acceptable; it makes decay observable.
    let access = crate::access::snapshot();
    let now = chrono::Utc::now().to_rfc3339();
    let mut records: Vec<MemoryRecord> = response
        .result
        .into_iter()
        .map(|p| {
            let id = p.id.as_ref().map(format_point_id).unwrap_or_default();
            let mut rec = point_to_record(id.clone(), &p.payload);
            let stat = access.get(&id);
            let count = stat.map(|s| s.count).unwrap_or(0);
            let last = stat.and_then(|s| s.last_access.as_deref());
            let created = if rec.created_at.is_empty() {
                None
            } else {
                Some(rec.created_at.as_str())
            };
            rec.coldness = crate::consolidate::coldness_score(created, last, count, &now);
            rec
        })
        .collect();
    // Newest first; empty created_at (legacy) sorts last so it still shows up.
    records.sort_by(
        |a, b| match (a.created_at.is_empty(), b.created_at.is_empty()) {
            (false, false) => b.created_at.cmp(&a.created_at),
            (true, false) => std::cmp::Ordering::Greater,
            (false, true) => std::cmp::Ordering::Less,
            (true, true) => std::cmp::Ordering::Equal,
        },
    );
    records.truncate(limit);
    Ok((records, truncated))
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
            point_to_record(
                point.id.as_ref().map(format_point_id).unwrap_or_default(),
                &point.payload,
            )
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
            Some(point_to_record(
                point.id.as_ref().map(format_point_id).unwrap_or_default(),
                p,
            ))
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
                author: extract_string(payload, "author"),
                created_at: extract_string(payload, "created_at"),
                score: 0.0,
            }
        })
        .collect();

    Ok(results)
}

/// What did a given agent contribute — memories tagged `author = agent`, newest
/// first. Uses the indexed `author` keyword field (no embedding, no query
/// vector). This is the reader that makes inter-agent writes visible: an
/// external coordinator can ask "show me what the Soloist wrote" without a
/// semantic query. Returns at most `limit` results.
pub async fn by_author(
    config: &MindConfig,
    agent: &str,
    limit: usize,
    libs: Option<&[String]>,
) -> Result<Vec<SearchResult>> {
    let client = get_client(config).await?;
    ensure_memories_collection(&client, config.vector_size).await?;

    let mut filter = Filter {
        must: vec![Condition::matches("author", agent.to_string())],
        must_not: vec![Condition::matches("quarantined", true)],
        ..Default::default()
    };
    // v2.4 confinement: a library-scoped token sees only its allowlist's
    // libraries, even when asking "what did agent X write". Same one→`must`,
    // many→`should` (OR) pattern as the memory filter. None/empty = unscoped.
    if let Some(libs) = libs.filter(|l| !l.is_empty()) {
        match libs {
            [one] => filter.must.push(Condition::matches("library", one.clone())),
            many => filter.must.push(
                Filter::should(
                    many.iter()
                        .map(|l| Condition::matches("library", l.clone())),
                )
                .into(),
            ),
        }
    }
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
        .context("by_author scroll failed")?;

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
                author: extract_string(payload, "author"),
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
                None,
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

/// Outcome of a reindex pass.
pub struct ReindexReport {
    /// How many points were re-embedded and written into the fresh collection.
    pub reindexed: usize,
    /// Points skipped (embed or upsert failure) — never aborts the whole pass.
    pub skipped: usize,
    /// The dimension the collection was rebuilt at (the current config value).
    pub new_dim: u64,
    /// JSON snapshot written before the drop. Recovery point if the pass dies
    /// mid-rebuild; `None` only when the collection was empty (nothing to back up).
    pub backup_path: Option<std::path::PathBuf>,
}

/// Re-embed every stored memory and procedure into a FRESH collection at the
/// current `config.vector_size`. This is the fix for a changed embedding model
/// (audit #11): swapping the model changes both the dimension and the vector
/// space, so the old vectors are meaningless even at the same size. `reindex`
/// reads each point's stored `content` (the source of truth — text is never
/// lost), recreates the collection at the new dimension, and re-embeds.
///
/// Metadata is preserved: `created_at`, `source`, `author`, `type` (procedures
/// reindex too), `quarantined` / `quarantine_reason`. The point ID stays the
/// content-addressed `deterministic_id`, so links and dedup survive.
///
/// Safety: before the collection is dropped, every point is both held in memory
/// AND written to a JSON snapshot under `backup_dir` (one file per library, the
/// same format as `export`). qdrant is an external service, so a file backup of
/// `mind_home` would NOT capture the memory store — this on-disk snapshot is the
/// real recovery point if the pass dies between the drop and the re-upsert. The
/// snapshot is the full JSON export (ids + content + metadata per library). The
/// operation is idempotent
/// (re-running re-embeds the same content to the same ids). Procedures keep
/// their own id namespace.
pub async fn reindex(config: &MindConfig, backup_dir: &std::path::Path) -> Result<ReindexReport> {
    let client = get_client(config).await?;

    // 1. Snapshot everything from the existing collection BEFORE touching it.
    let existing = if client
        .collection_exists(MEMORIES_COLLECTION)
        .await
        .unwrap_or(false)
    {
        scroll_all(&client, MEMORIES_COLLECTION).await?
    } else {
        Vec::new()
    };

    // Capture the fields we need to rebuild each point, from the payload.
    struct Captured {
        id: qdrant_client::qdrant::PointId,
        content: String,
        library: String,
        source: Option<String>,
        author: Option<String>,
        mem_type: String,
        created_at: String,
        quarantined: Option<bool>,
        quarantine_reason: Option<String>,
    }
    let now = chrono::Utc::now().to_rfc3339();
    let captured: Vec<Captured> = existing
        .into_iter()
        .filter_map(|p| {
            let id = p.id.clone()?;
            let content = extract_string(&p.payload, "content")?;
            Some(Captured {
                id,
                library: extract_string(&p.payload, "library").unwrap_or_default(),
                source: extract_string(&p.payload, "source"),
                author: extract_string(&p.payload, "author"),
                mem_type: extract_string(&p.payload, "type")
                    .unwrap_or_else(|| TYPE_MEMORY.to_string()),
                created_at: extract_string(&p.payload, "created_at").unwrap_or_else(|| now.clone()),
                quarantined: p.payload.get("quarantined").and_then(|v| v.as_bool()),
                quarantine_reason: extract_string(&p.payload, "quarantine_reason"),
                content,
            })
        })
        .collect();

    // 2. Persist the snapshot to disk BEFORE the drop. If the rebuild dies
    //    mid-pass the store is recoverable from these files via `mgimind import`.
    //    Skip when there is nothing to back up (fresh / empty collection).
    let backup_path = if captured.is_empty() {
        None
    } else {
        let written = export_all(config, "json", &backup_dir.to_string_lossy()).await?;
        if written == 0 {
            anyhow::bail!(
                "Refusing to reindex: {} points to rebuild but the safety snapshot \
                 wrote 0 files to {}. Aborting before any deletion.",
                captured.len(),
                backup_dir.display()
            );
        }
        Some(backup_dir.to_path_buf())
    };

    // 3. Recreate the collection at the CURRENT dimension. Drop only after the
    //    snapshot above is on disk, so the text is safe before any deletion.
    if client
        .collection_exists(MEMORIES_COLLECTION)
        .await
        .unwrap_or(false)
    {
        client
            .delete_collection(DeleteCollectionBuilder::new(MEMORIES_COLLECTION))
            .await
            .context("Failed to drop the memories collection during reindex")?;
    }
    MEMORIES_READY.store(false, Ordering::Release);
    create_memories_collection(&client, config.vector_size).await?;
    ensure_payload_indexes(&client, MEMORIES_COLLECTION).await;
    MEMORIES_READY.store(true, Ordering::Release);

    // 4. Re-embed each captured point from its stored content and re-insert.
    let mut reindexed = 0usize;
    let mut skipped = 0usize;
    for c in &captured {
        let embedding = match embedder::embed_passage(config, &c.content).await {
            Ok(e) if check_dim(&e, config).is_ok() => e,
            Ok(_) => {
                eprintln!("  [skip] dimension mismatch re-embedding one entry");
                skipped += 1;
                continue;
            }
            Err(e) => {
                eprintln!("  [skip] embed failed: {e}");
                skipped += 1;
                continue;
            }
        };
        let hash = blake3::hash(c.content.as_bytes()).to_hex().to_string();
        let payload = build_payload_full(
            &c.content,
            &hash,
            &c.created_at,
            &now,
            &c.library,
            c.source.as_deref(),
            &c.mem_type,
            c.quarantined,
            c.quarantine_reason.as_deref(),
            c.author.as_deref(),
        );
        let (s_idx, s_val) = sparse_vector(&c.content);
        let vectors = NamedVectors::default()
            .add_vector(DENSE_VEC, Vector::new_dense(embedding))
            .add_vector(SPARSE_VEC, Vector::new_sparse(s_idx, s_val));
        if let Err(e) = client
            .upsert_points(
                UpsertPointsBuilder::new(
                    MEMORIES_COLLECTION,
                    vec![PointStruct::new(c.id.clone(), vectors, payload)],
                )
                .wait(true),
            )
            .await
        {
            eprintln!("  [skip] upsert failed during reindex: {e}");
            skipped += 1;
            continue;
        }
        reindexed += 1;
    }

    Ok(ReindexReport {
        reindexed,
        skipped,
        new_dim: config.vector_size,
        backup_path,
    })
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

// ===== Encrypted backup (roadmap v1.2 local half) =====
// On-disk format of an encrypted backup file:
//   magic "MGIBK1\0" (7 bytes) | backup_salt (32 bytes) | nonce+ciphertext
// The ciphertext is AES-256-GCM over the gzip+tar of the data dir. The key is
// derived from the user's passphrase + the per-file `backup_salt` via the
// vault's pinned Argon2id — a SEPARATE salt from `vault.salt` so backup and
// secret-vault rotation stay independent (critic's key-separation requirement).
// S3/chunked transport (v1.2 full) sits on top of this blob unchanged.

const BACKUP_MAGIC: &[u8; 7] = b"MGIBK1\0";

/// Write an encrypted backup of the whole data dir to `output`, protected by
/// `passphrase`. The archive is built in memory, then encrypted; nothing
/// plaintext touches disk.
pub fn backup_encrypted(output: &str, passphrase: &str) -> Result<()> {
    use rand::RngCore;

    let home = crate::config::mind_home();

    // gzip+tar the data dir into an in-memory buffer.
    let mut archive: Vec<u8> = Vec::new();
    {
        let enc = flate2::write::GzEncoder::new(&mut archive, flate2::Compression::default());
        let mut tar = tar::Builder::new(enc);
        tar.append_dir_all(".", &home)
            .context("Failed to archive data directory")?;
        tar.into_inner()?.finish()?;
    }

    // Fresh per-backup salt → key, then encrypt the archive bytes.
    let mut salt = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut salt);
    let key = crate::vault::derive_key_with_salt(passphrase, &salt)
        .context("backup key derivation failed")?;
    let blob =
        crate::vault::encrypt_with_key(&archive, &key).context("backup encryption failed")?;

    // magic | salt | nonce+ciphertext. Wrapped in `Ciphertext` so the writer
    // below type-checks only against sealed bytes — a future refactor cannot
    // hand the plaintext `archive` to the file-write by accident.
    let mut framed = Vec::with_capacity(BACKUP_MAGIC.len() + 32 + blob.len());
    framed.extend_from_slice(BACKUP_MAGIC);
    framed.extend_from_slice(&salt);
    framed.extend_from_slice(&blob);
    write_ciphertext(std::path::Path::new(output), &Ciphertext(framed))
        .with_context(|| format!("Failed to write encrypted backup {output}"))?;
    Ok(())
}

/// The sealed bytes of an encrypted backup (magic | salt | nonce+ciphertext).
/// A newtype whose only constructor site is the encryption path above, so the
/// backup writer cannot be handed anything but already-encrypted bytes.
struct Ciphertext(Vec<u8>);

/// The ONLY writer for the encrypted-backup path. It accepts nothing but a
/// sealed `Ciphertext`, so a static read of this module proves no plaintext
/// leaves the process on the backup route (the round-trip test in this file
/// proves the same property dynamically).
fn write_ciphertext(path: &std::path::Path, ct: &Ciphertext) -> Result<()> {
    crate::util::atomic_write(path, &ct.0)
}

/// Restore an encrypted backup written by `backup_encrypted`.
pub fn restore_encrypted(input: &str, passphrase: &str) -> Result<()> {
    let data =
        std::fs::read(input).with_context(|| format!("Failed to read encrypted backup {input}"))?;
    if data.len() < BACKUP_MAGIC.len() + 32 {
        anyhow::bail!("Not a valid mgi-mind encrypted backup (too short)");
    }
    if &data[..BACKUP_MAGIC.len()] != BACKUP_MAGIC {
        anyhow::bail!("Not a valid mgi-mind encrypted backup (bad magic)");
    }
    let salt: [u8; 32] = data[BACKUP_MAGIC.len()..BACKUP_MAGIC.len() + 32]
        .try_into()
        .expect("32-byte slice");
    let blob = &data[BACKUP_MAGIC.len() + 32..];

    let key = crate::vault::derive_key_with_salt(passphrase, &salt)
        .context("backup key derivation failed")?;
    let archive = crate::vault::decrypt_with_key(blob, &key)
        .context("backup decryption failed — wrong passphrase?")?;

    let home = crate::config::mind_home();
    std::fs::create_dir_all(&home)?;
    let dec = flate2::read::GzDecoder::new(std::io::Cursor::new(archive));
    let mut tar = tar::Archive::new(dec);
    tar.unpack(&home)
        .context("Failed to extract encrypted backup")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_name_normalizes_and_rejects_bad_input() {
        // trimmed + lowercased
        assert_eq!(normalize_block_name("  Persona ").unwrap(), "persona");
        assert_eq!(
            normalize_block_name("current-project_1").unwrap(),
            "current-project_1"
        );
        // rejects empty, whitespace-only, too long, and disallowed chars
        assert!(normalize_block_name("").is_err());
        assert!(normalize_block_name("   ").is_err());
        assert!(normalize_block_name(&"x".repeat(65)).is_err());
        assert!(normalize_block_name("has space").is_err());
        assert!(normalize_block_name("slash/name").is_err());
        assert!(normalize_block_name("emoji😀").is_err());
    }

    #[test]
    fn rerank_override_resolves_against_config() {
        let cfg = MindConfig {
            rerank_enabled: true,
            rerank_top_k: 20,
            ..Default::default()
        };
        // All-None override = use config exactly (byte-identical old behavior).
        assert_eq!(RerankOverride::default().resolve(&cfg), (true, 20));
        // Force off for this query.
        assert_eq!(
            RerankOverride {
                enabled: Some(false),
                top_k: None
            }
            .resolve(&cfg),
            (false, 20)
        );
        // Override depth only.
        assert_eq!(
            RerankOverride {
                enabled: None,
                top_k: Some(50)
            }
            .resolve(&cfg),
            (true, 50)
        );
        // Both overridden.
        assert_eq!(
            RerankOverride {
                enabled: Some(false),
                top_k: Some(5)
            }
            .resolve(&cfg),
            (false, 5)
        );
    }

    #[test]
    fn parse_datetime_bound_accepts_rfc3339_and_bare_date() {
        // RFC3339 with timezone.
        let t = parse_datetime_bound("2026-06-09T12:00:00Z").unwrap();
        assert!(t.seconds > 0);
        // Bare date → midnight UTC.
        let d = parse_datetime_bound("2026-06-09").unwrap();
        assert_eq!(d.nanos, 0);
        // Garbage is rejected with an actionable message.
        let err = parse_datetime_bound("not-a-date").unwrap_err();
        assert!(err.to_string().contains("invalid date"));
    }

    #[test]
    fn empty_filter_is_just_the_base_exclusions() {
        // No metadata narrowing → only the base must_not (procedure, quarantined,
        // archived), no must conditions. Proves the fast path keeps the old
        // search behavior aside from also hiding archived (soft-forgotten) points.
        let f = memory_query_filter_ex(&MemoryFilter::default()).unwrap();
        assert_eq!(f.must_not.len(), 3);
        assert!(f.must.is_empty());
        // And matches the single-library legacy helper for the library case.
        let one = memory_query_filter_ex(&MemoryFilter::for_library(Some("p"))).unwrap();
        assert_eq!(one.must.len(), 1);
    }

    #[test]
    fn browse_sort_keeps_undated_legacy_at_the_tail() {
        // The fix for the ordered-scroll-drops-legacy bug: undated points must
        // sort LAST, not vanish. Mirror the in-memory sort list_filtered uses.
        fn rec(created_at: &str) -> MemoryRecord {
            MemoryRecord {
                id: created_at.into(),
                content: String::new(),
                source: None,
                r#type: "memory".into(),
                created_at: created_at.into(),
                updated_at: String::new(),
                library: String::new(),
                author: None,
                confidence_score: None,
                coldness: None,
            }
        }
        let mut v = [
            rec(""),                     // legacy, undated
            rec("2026-01-01T00:00:00Z"), // older
            rec("2026-06-01T00:00:00Z"), // newest
        ];
        v.sort_by(
            |a, b| match (a.created_at.is_empty(), b.created_at.is_empty()) {
                (false, false) => b.created_at.cmp(&a.created_at),
                (true, false) => std::cmp::Ordering::Greater,
                (false, true) => std::cmp::Ordering::Less,
                (true, true) => std::cmp::Ordering::Equal,
            },
        );
        // newest first, undated last — and the legacy point is still present.
        assert_eq!(v[0].created_at, "2026-06-01T00:00:00Z");
        assert_eq!(v[1].created_at, "2026-01-01T00:00:00Z");
        assert_eq!(v[2].created_at, "");
    }

    #[test]
    fn date_window_is_since_inclusive_before_exclusive() {
        use qdrant_client::qdrant::condition::ConditionOneOf;
        let mf = MemoryFilter {
            created_since: Some("2026-01-01".into()),
            created_before: Some("2026-02-01".into()),
            ..Default::default()
        };
        let f = memory_query_filter_ex(&mf).unwrap();
        // Find the datetime-range condition and assert gte (since) + lt (before)
        // — the documented half-open [since, before) window.
        let range = f
            .must
            .iter()
            .find_map(|c| match &c.condition_one_of {
                Some(ConditionOneOf::Field(fc)) if fc.key == "created_at" => fc.datetime_range,
                _ => None,
            })
            .expect("a created_at datetime_range condition");
        assert!(range.gte.is_some(), "since must map to gte (inclusive)");
        assert!(range.lt.is_some(), "before must map to lt (exclusive)");
        assert!(range.gt.is_none() && range.lte.is_none());
    }

    #[test]
    fn filter_layers_each_field_as_a_must_condition() {
        let mf = MemoryFilter {
            libraries: vec!["a".into(), "b".into()],
            author: Some("alice".into()),
            source: Some("ingest".into()),
            created_since: Some("2026-01-01".into()),
            created_before: Some("2026-12-31".into()),
            ..Default::default()
        };
        let f = memory_query_filter_ex(&mf).unwrap();
        // base exclusions stay (procedure, quarantined, archived).
        assert_eq!(f.must_not.len(), 3);
        // multi-library OR (1) + author (1) + source (1) + one datetime range (1)
        // = 4 must conditions (the date window is a single range condition).
        assert_eq!(f.must.len(), 4);
        // A bad date bound surfaces as an error, not a silent drop.
        let bad = MemoryFilter {
            created_since: Some("yesterday".into()),
            ..Default::default()
        };
        assert!(memory_query_filter_ex(&bad).is_err());
    }

    #[test]
    fn archived_scope_flips_the_archived_condition() {
        // Exclude (default) → archived in must_not (hidden, current behavior).
        let excl = memory_query_filter_ex(&MemoryFilter::default()).unwrap();
        assert!(
            excl.must_not.iter().any(|c| matches!(&c.condition_one_of,
                    Some(qdrant_client::qdrant::condition::ConditionOneOf::Field(fc))
                        if fc.key == "archived")),
            "Exclude must hide archived via must_not"
        );
        // Only → archived in must (list ONLY forgotten), NOT in must_not.
        let only = memory_query_filter_ex(&MemoryFilter {
            archived: ArchivedScope::Only,
            ..Default::default()
        })
        .unwrap();
        assert!(
            only.must.iter().any(|c| matches!(&c.condition_one_of,
                Some(qdrant_client::qdrant::condition::ConditionOneOf::Field(fc))
                    if fc.key == "archived")),
            "Only must require archived via must"
        );
        assert!(
            !only.must_not.iter().any(|c| matches!(&c.condition_one_of,
                Some(qdrant_client::qdrant::condition::ConditionOneOf::Field(fc))
                    if fc.key == "archived")),
            "Only must not also exclude archived"
        );
        // Include → no archived condition either way (both visible).
        let incl = memory_query_filter_ex(&MemoryFilter {
            archived: ArchivedScope::Include,
            ..Default::default()
        })
        .unwrap();
        let mentions_archived = incl.must.iter().chain(incl.must_not.iter()).any(|c| {
            matches!(&c.condition_one_of,
                Some(qdrant_client::qdrant::condition::ConditionOneOf::Field(fc))
                    if fc.key == "archived")
        });
        assert!(!mentions_archived, "Include must not constrain archived");
    }

    #[test]
    fn validate_library_name_accepts_sane_and_rejects_bad() {
        // Good names pass.
        for ok in ["projects", "my-lib", "lib_2", "a.b.c", "Personal"] {
            assert!(validate_library_name(ok).is_ok(), "{ok} should be valid");
        }
        // Empty / whitespace-only.
        assert!(validate_library_name("").is_err());
        assert!(validate_library_name("   ").is_err());
        // Over-long.
        assert!(validate_library_name(&"x".repeat(129)).is_err());
        // Illegal chars (path separators, spaces, control).
        for bad in ["a/b", "a b", "a\tb", "../etc", "name;drop"] {
            assert!(
                validate_library_name(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
        // Reserved names (case-insensitive) — the namespace-protection guard.
        for reserved in [
            "_procedures",
            "memories",
            "_kg_facts",
            "_mod_procstats",
            "MEMORIES",
        ] {
            assert!(
                validate_library_name(reserved).is_err(),
                "{reserved} must be reserved"
            );
        }
    }

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
    fn author_is_payload_not_identity() {
        // The critical invariant: author must NOT enter the point id. Two
        // agents writing identical content must collapse to the SAME point
        // (idempotent, quarantine-promote, _links all rely on this).
        let id_via_quarantine = quarantine_id_for("lib", "shared fact");
        let id_direct = deterministic_id("lib", "shared fact");
        assert_eq!(
            id_via_quarantine, id_direct,
            "quarantine id must equal the content-addressed id regardless of author"
        );
        // And the payload carries author as a plain field, independent of id.
        let p = build_payload(
            "shared fact",
            "h",
            "t0",
            "t1",
            "lib",
            None,
            TYPE_MEMORY,
            Some("agentB"),
        );
        assert_eq!(
            extract_string(&p, "author").as_deref(),
            Some("agentB"),
            "author must land in the payload"
        );
        // No author → no key (legacy points stay byte-identical).
        let p2 = build_payload(
            "shared fact",
            "h",
            "t0",
            "t1",
            "lib",
            None,
            TYPE_MEMORY,
            None,
        );
        assert!(
            extract_string(&p2, "author").is_none(),
            "absent author must not add a payload key"
        );
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

    // ===== v1.4 Phase 0: confidence_score payload round-trip =====

    #[test]
    fn confidence_score_round_trips_through_payload() {
        // The Phase 0 spec for the score field: write an f32 into a Qdrant
        // point payload via payload_set_confidence, read it back via
        // payload_get_confidence, get the same value within f32 precision.
        // No Qdrant connection needed — this is a pure HashMap test on the
        // same helpers the real write path will use in Phase 3.
        let mut payload = std::collections::HashMap::new();
        payload_set_confidence(&mut payload, 0.732_5);
        let read_back = payload_get_confidence(&payload).expect("score present");
        assert!(
            (read_back - 0.732_5).abs() < 1e-5,
            "round-trip diff {} too large",
            (read_back - 0.732_5).abs()
        );
    }

    #[test]
    fn confidence_score_absent_payload_reads_as_none() {
        // Legacy memories (written before v1.4) have no confidence_score in
        // their payload. The reader must return None — not 0.0, not a default
        // — so the ranking layer can distinguish "no score yet" from "low
        // confidence" and fall back cleanly.
        let payload = std::collections::HashMap::new();
        assert!(payload_get_confidence(&payload).is_none());
    }

    #[test]
    fn confidence_score_unparseable_payload_reads_as_none() {
        // Defensive: if some other write put a non-numeric string under the
        // same key (shouldn't happen, but the world is wide), the reader
        // must degrade to None rather than poison the ranker with a panic.
        let mut payload = std::collections::HashMap::new();
        payload.insert(
            "confidence_score".into(),
            qdrant_client::qdrant::Value::from("not-a-float"),
        );
        assert!(payload_get_confidence(&payload).is_none());
    }

    #[test]
    fn encrypted_backup_round_trips_and_rejects_wrong_passphrase() {
        // Isolate MGIMIND_HOME to a temp dir so backup/restore touch only it.
        // (Serial-safe: uses a unique dir; env is set/cleared within the test.)
        let base =
            std::env::temp_dir().join(format!("mgimind-bk-test-{}", uuid::Uuid::new_v4().simple()));
        let home = base.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let marker = home.join("marker.txt");
        std::fs::write(&marker, b"crescendo-secret-state").unwrap();

        let bk = base.join("out.enc");
        let bk_s = bk.to_str().unwrap();

        // Backup encrypted, then wipe the home, then restore.
        // SAFETY: test-only, single-threaded within this test body.
        unsafe { std::env::set_var("MGIMIND_HOME", &home) };
        backup_encrypted(bk_s, "correct horse battery staple").unwrap();

        // The blob must not contain the plaintext marker.
        let raw = std::fs::read(&bk).unwrap();
        assert_eq!(&raw[..BACKUP_MAGIC.len()], BACKUP_MAGIC, "magic header");
        assert!(
            !raw.windows(b"crescendo-secret-state".len())
                .any(|w| w == b"crescendo-secret-state"),
            "plaintext must not appear in the encrypted backup"
        );

        // Wrong passphrase must fail (AES-GCM auth tag).
        std::fs::remove_dir_all(&home).unwrap();
        assert!(
            restore_encrypted(bk_s, "wrong passphrase").is_err(),
            "wrong passphrase must not restore"
        );

        // Correct passphrase round-trips the marker back.
        restore_encrypted(bk_s, "correct horse battery staple").unwrap();
        let restored = std::fs::read(&marker).unwrap();
        assert_eq!(restored, b"crescendo-secret-state");

        unsafe { std::env::remove_var("MGIMIND_HOME") };
        let _ = std::fs::remove_dir_all(&base);
    }
}
