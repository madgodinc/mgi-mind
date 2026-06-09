//! Ephemeral local viewer for the memory store.
//!
//! `mgimind viewer` brings up an HTTP server on 127.0.0.1 on a random free port,
//! prints the URL (with a one-shot bearer token already embedded), and exits
//! when the user hits Ctrl-C. The static frontend is baked into the binary as
//! a string, so there is no extra runtime artifact and no Node/npm — the
//! viewer respects the same single-binary boundary as v0.8.0.
//!
//! Scope is intentionally narrow: this is an audit window, not a notes app.
//! It shows what is in the store, what was changed (audit log), and lets the
//! user delete a memory through a button that goes through the same
//! audited write path as the CLI.

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{
        Html, IntoResponse, Response,
        sse::{Event, Sse},
    },
    routing::{delete, get, post},
};
use serde::Deserialize;
use std::convert::Infallible;
use std::sync::Arc;
use tokio::net::TcpListener;
use uuid::Uuid;

use crate::audit;
use crate::config::MindConfig;
use crate::storage;

/// Static frontend, embedded into the binary at compile time.
const INDEX_HTML: &str = include_str!("viewer_index.html");

#[derive(Clone)]
struct AppState {
    config: Arc<MindConfig>,
    /// One-shot bearer token: client must present this in either
    /// `Authorization: Bearer <token>` or as `?token=<token>` for browser
    /// links. The token is generated per-process and printed once at startup.
    token: Arc<String>,
}

/// Entry point used by `Commands::Viewer`.
pub async fn run(config: MindConfig, open_browser: bool) -> Result<()> {
    let token = Uuid::new_v4().to_string();
    let state = AppState {
        config: Arc::new(config),
        token: Arc::new(token.clone()),
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/api/health", get(health))
        .route("/api/libraries", get(api_libraries))
        .route("/api/memories", get(api_memories))
        .route("/api/audit", get(api_audit))
        .route("/api/memories/:id", delete(api_delete_memory))
        .route("/api/quarantine", get(api_quarantine_list))
        .route("/api/quarantine/:id/promote", post(api_quarantine_promote))
        .route("/api/consolidate", get(api_consolidate_dry_run))
        .route("/api/ingest/recent", get(api_ingest_recent))
        .route("/api/graph", get(api_graph))
        .route("/api/pulse", get(api_pulse))
        .with_state(state);

    // Random free port: ask the OS for 0, then read what it gave us.
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("Failed to bind 127.0.0.1 on a free port")?;
    let addr = listener.local_addr().context("Failed to read bound port")?;
    let url = format!("http://{}/?token={}", addr, token);

    eprintln!();
    eprintln!("  mgimind viewer  •  audit window over the memory store");
    eprintln!("  ───────────────────────────────────────────────────────");
    eprintln!("  open:  {url}");
    eprintln!("  stop:  Ctrl-C");
    eprintln!();

    if open_browser && let Err(e) = open_in_browser(&url) {
        eprintln!("  (could not auto-open browser: {e})");
    }

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("viewer server error")?;
    eprintln!("  viewer stopped.");
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

fn open_in_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "linux")]
    let cmd = "xdg-open";
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(target_os = "windows")]
    let cmd = "explorer";

    std::process::Command::new(cmd)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("spawn {cmd}"))?;
    Ok(())
}

// ----- Routes ----------------------------------------------------------------

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

#[derive(Deserialize)]
struct AuthQuery {
    token: Option<String>,
}

fn check_auth(state: &AppState, headers: &HeaderMap, q: &AuthQuery) -> Result<(), StatusCode> {
    if let Some(t) = q.token.as_deref()
        && t == state.token.as_str()
    {
        return Ok(());
    }
    if let Some(auth) = headers.get("authorization")
        && let Ok(s) = auth.to_str()
        && let Some(t) = s.strip_prefix("Bearer ")
        && t == state.token.as_str()
    {
        return Ok(());
    }
    Err(StatusCode::UNAUTHORIZED)
}

async fn health(
    State(state): State<AppState>,
    Query(q): Query<AuthQuery>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    check_auth(&state, &headers, &q)?;
    Ok(Json(serde_json::json!({"ok": true})))
}

async fn api_libraries(
    State(state): State<AppState>,
    Query(q): Query<AuthQuery>,
    headers: HeaderMap,
) -> Result<Json<Vec<String>>, Response> {
    check_auth(&state, &headers, &q).map_err(|s| s.into_response())?;
    let libs = storage::list_libraries(&state.config)
        .await
        .map_err(internal)?;
    Ok(Json(libs))
}

#[derive(Deserialize)]
struct MemoriesQuery {
    token: Option<String>,
    library: Option<String>,
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    100
}

async fn api_memories(
    State(state): State<AppState>,
    Query(q): Query<MemoriesQuery>,
    headers: HeaderMap,
) -> Result<Json<Vec<MemoryRow>>, Response> {
    let auth = AuthQuery {
        token: q.token.clone(),
    };
    check_auth(&state, &headers, &auth).map_err(|s| s.into_response())?;

    let library = q.library.unwrap_or_default();
    if library.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "library param required").into_response());
    }
    let rows = storage::list_memories(&state.config, &library, q.limit)
        .await
        .map_err(internal)?
        .into_iter()
        .map(|m| MemoryRow {
            id: m.id,
            content: m.content,
            source: m.source,
            r#type: m.r#type,
            created_at: m.created_at,
            updated_at: m.updated_at,
        })
        .collect();
    Ok(Json(rows))
}

async fn api_audit(
    State(state): State<AppState>,
    Query(q): Query<AuthQuery>,
    headers: HeaderMap,
) -> Result<Json<Vec<audit::AuditEvent>>, Response> {
    check_auth(&state, &headers, &q).map_err(|s| s.into_response())?;
    // Return most recent 200 — enough for an audit overview, bounded for the
    // browser. If a user needs more, they can grep audit.log directly.
    let events = audit::recent(200).map_err(internal)?;
    Ok(Json(events))
}

async fn api_delete_memory(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<AuthQuery>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, Response> {
    check_auth(&state, &headers, &q).map_err(|s| s.into_response())?;
    // library arg is kept only for CLI/MCP signature parity; the id is the
    // authoritative key. "viewer" tags the audit actor so the trail shows
    // *where* the delete came from.
    storage::delete_memory(&state.config, "", &id)
        .await
        .map_err(internal)?;
    audit::record(audit::AuditEvent::new(audit::AuditOp::Delete, "", &id).actor("viewer"));
    Ok(Json(serde_json::json!({"ok": true, "id": id})))
}

#[derive(Deserialize)]
struct QuarantineQuery {
    token: Option<String>,
    library: Option<String>,
    #[serde(default = "default_quarantine_limit")]
    limit: usize,
    /// Opaque cursor from a previous response's `next_cursor`. Omit for the
    /// first page.
    cursor: Option<String>,
}

fn default_quarantine_limit() -> usize {
    50
}

#[derive(serde::Serialize)]
struct QuarantineListResponse {
    entries: Vec<QuarantineRow>,
    next_cursor: Option<String>,
}

async fn api_quarantine_list(
    State(state): State<AppState>,
    Query(q): Query<QuarantineQuery>,
    headers: HeaderMap,
) -> Result<Json<QuarantineListResponse>, Response> {
    let auth = AuthQuery {
        token: q.token.clone(),
    };
    check_auth(&state, &headers, &auth).map_err(|s| s.into_response())?;
    let page = storage::quarantine_list_page(
        &state.config,
        q.library.as_deref(),
        q.limit,
        q.cursor.as_deref(),
    )
    .await
    .map_err(internal)?;
    let entries = page
        .entries
        .into_iter()
        .map(|e| QuarantineRow {
            id: e.id,
            library: e.library,
            content: e.content,
            source: e.source,
            reason: e.reason,
            created_at: e.created_at,
        })
        .collect();
    Ok(Json(QuarantineListResponse {
        entries,
        next_cursor: page.next_cursor,
    }))
}

#[derive(Deserialize)]
struct IngestRecentQuery {
    token: Option<String>,
    /// RFC3339 lower bound on created_at (inclusive). Typically the
    /// session-start ISO timestamp. If omitted, returns the most recent
    /// `max_scan` ingest writes regardless of age.
    since: Option<String>,
    /// How many ingest-tagged points to scan (newest first) before applying
    /// the `since` filter on the client. Defaults to 200 — enough for a
    /// typical session, bounded for the browser.
    #[serde(default = "default_ingest_scan")]
    max_scan: usize,
}

fn default_ingest_scan() -> usize {
    200
}

/// "What did auto-ingest write since X?" — the v0.12 viewer's headline page,
/// the auto-ingest-feedback loop the user said they wanted most. Returns
/// MemoryRow rows (same shape as /api/memories) so the UI can render them with
/// the existing memory-card component.
async fn api_ingest_recent(
    State(state): State<AppState>,
    Query(q): Query<IngestRecentQuery>,
    headers: HeaderMap,
) -> Result<Json<Vec<MemoryRow>>, Response> {
    let auth = AuthQuery {
        token: q.token.clone(),
    };
    check_auth(&state, &headers, &auth).map_err(|s| s.into_response())?;
    // Empty `since` means "no lower bound" — pass an ISO sentinel that sorts
    // before any real timestamp. "0000-..." lexicographically precedes any
    // RFC3339 string we'd ever write.
    let since = q.since.as_deref().unwrap_or("0000-01-01T00:00:00Z");
    let rows = storage::recent_by_source_since(&state.config, "ingest", since, q.max_scan)
        .await
        .map_err(internal)?
        .into_iter()
        .map(|m| MemoryRow {
            id: m.id,
            content: m.content,
            source: m.source,
            r#type: m.r#type,
            created_at: m.created_at,
            updated_at: m.updated_at,
        })
        .collect();
    Ok(Json(rows))
}

#[derive(Deserialize)]
struct ConsolidateQuery {
    token: Option<String>,
    library: Option<String>,
}

/// Always dry-run. The viewer surface intentionally does NOT expose `--apply`
/// for consolidate — destructive operations belong on the CLI where the user
/// has to type the flag explicitly. This endpoint is the "what would happen"
/// preview that the v0.12 UI shows before the user runs the CLI command.
async fn api_consolidate_dry_run(
    State(state): State<AppState>,
    Query(q): Query<ConsolidateQuery>,
    headers: HeaderMap,
) -> Result<Json<crate::consolidate::Report>, Response> {
    let auth = AuthQuery {
        token: q.token.clone(),
    };
    check_auth(&state, &headers, &auth).map_err(|s| s.into_response())?;
    let opts = crate::consolidate::Options {
        apply: false,
        library: q.library,
        near_dup_threshold: 0.0, // with_defaults() will fill to 0.97
        decay_days: 0,           // with_defaults() will fill to 180
        prune_cold: false,
    }
    .with_defaults();
    let report = crate::consolidate::run(&state.config, opts)
        .await
        .map_err(internal)?;
    Ok(Json(report))
}

async fn api_quarantine_promote(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<AuthQuery>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, Response> {
    check_auth(&state, &headers, &q).map_err(|s| s.into_response())?;
    let promoted = storage::promote_from_quarantine(&state.config, &id)
        .await
        .map_err(internal)?;
    if !promoted {
        return Ok(Json(serde_json::json!({
            "ok": false,
            "id": id,
            "reason": "not in quarantine"
        })));
    }
    // promote_from_quarantine writes its own audit event (actor=relevance-gate)
    // with note "promoted from quarantine (re-asserted)". We add a second event
    // tagged actor=viewer so the trail shows the manual UI promotion is distinct
    // from the automatic re-assertion path.
    audit::record(
        audit::AuditEvent::new(audit::AuditOp::Update, "", &id)
            .actor("viewer")
            .note("manual promote via viewer UI"),
    );
    Ok(Json(serde_json::json!({"ok": true, "id": id})))
}

/// The brain as a graph: cores (nodes) connected by neurons (edges), for a
/// visual "dig inside the mind" browser.
///
/// Core kinds: `library` (a memory region), `memory` (a stored chunk — the
/// bright cores), `entity` (a fact subject/object concept).
///
/// Neuron kinds: `fact` (entity-predicate-entity, the knowledge-graph wiring),
/// `holds` (library to memory), `mention` (a memory text names a known entity
/// via non-LLM substring match — the associative threads).
///
/// Shape is cytoscape/d3-friendly. `limit` caps memory cores per library so a
/// huge store still renders.
async fn api_graph(
    State(state): State<AppState>,
    Query(q): Query<GraphQuery>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, Response> {
    let auth = AuthQuery {
        token: q.token.clone(),
    };
    check_auth(&state, &headers, &auth).map_err(|s| s.into_response())?;
    let cfg = &state.config;
    let per_lib = q.limit;

    use std::collections::BTreeMap;
    // Dedupe nodes by id; an entity appearing as both subject and object is one
    // core. Map id -> node json so later inserts (e.g. kind upgrade) are cheap.
    let mut nodes: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    let mut edges: Vec<serde_json::Value> = Vec::new();

    // --- entity cores + fact neurons ---
    let facts = crate::knowledge::list_all_facts(cfg).await.map_err(internal)?;
    let mut entities: Vec<String> = Vec::new();
    for f in &facts {
        if !f.valid {
            continue;
        }
        for e in [&f.subject, &f.object] {
            nodes.entry(e.clone()).or_insert_with(|| {
                entities.push(e.clone());
                serde_json::json!({ "id": e, "label": e, "kind": "entity" })
            });
        }
        edges.push(serde_json::json!({
            "source": f.subject, "target": f.object, "label": f.predicate,
            "kind": "fact", "id": f.id,
        }));
    }

    // --- library + memory cores, holds + mention neurons ---
    let libs = crate::storage::list_libraries(cfg).await.map_err(internal)?;
    for lib in &libs {
        if lib.starts_with('_') {
            continue;
        }
        let lib_id = format!("lib:{lib}");
        nodes.entry(lib_id.clone()).or_insert_with(|| {
            serde_json::json!({ "id": lib_id, "label": lib, "kind": "library" })
        });
        let mems = crate::storage::list_memories(cfg, lib, per_lib)
            .await
            .unwrap_or_default();
        for m in &mems {
            let mid = format!("mem:{}", m.id);
            let snippet: String = m.content.chars().take(80).collect();
            nodes.insert(
                mid.clone(),
                serde_json::json!({ "id": mid, "label": snippet, "kind": "memory" }),
            );
            edges.push(serde_json::json!({
                "source": lib_id, "target": mid, "label": "holds", "kind": "holds",
            }));
            // Associative threads: a memory that names a known entity.
            let lc = m.content.to_lowercase();
            for e in &entities {
                if e.len() >= 4 && lc.contains(&e.to_lowercase()) {
                    edges.push(serde_json::json!({
                        "source": mid, "target": e, "label": "mentions", "kind": "mention",
                    }));
                }
            }
        }
    }

    let node_list: Vec<serde_json::Value> = nodes.into_values().collect();
    Ok(Json(serde_json::json!({
        "nodes": node_list,
        "edges": edges,
        "stats": {
            "nodes": node_list.len(), "edges": edges.len(),
            "facts": facts.len(), "libraries": libs.len(),
        },
    })))
}

#[derive(Deserialize)]
struct GraphQuery {
    token: Option<String>,
    #[serde(default = "default_graph_limit")]
    limit: usize,
}

fn default_graph_limit() -> usize {
    200
}

/// Live pulse feed (SSE). Each `data:` line is a `PulseEvent` JSON: an impulse
/// the brain just emitted (write/read/process). The browser animates it as a
/// taxi running a neuron toward `target`, colored by `kind`. Best-effort: a
/// slow client that lags just misses old pulses.
async fn api_pulse(
    State(state): State<AppState>,
    Query(q): Query<AuthQuery>,
    headers: HeaderMap,
) -> Result<Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>>, Response> {
    check_auth(&state, &headers, &q).map_err(|s| s.into_response())?;
    let rx = crate::pulse::subscribe();
    // futures_util::stream::unfold drives the broadcast receiver without adding
    // a new dependency (no async-stream / tokio-stream crate needed).
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    let json = serde_json::to_string(&ev).unwrap_or_default();
                    return Some((Ok(Event::default().data(json)), rx));
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    Ok(Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default()))
}

fn internal<E: std::fmt::Display>(e: E) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
}

// ----- DTOs ------------------------------------------------------------------

#[derive(serde::Serialize)]
struct MemoryRow {
    id: String,
    content: String,
    source: Option<String>,
    r#type: String,
    created_at: String,
    updated_at: String,
}

#[derive(serde::Serialize)]
struct QuarantineRow {
    id: String,
    library: String,
    content: String,
    source: Option<String>,
    reason: String,
    created_at: Option<String>,
}
