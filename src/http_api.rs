//! Loopback HTTP tool-surface for external multi-agent systems.
//!
//! `mgimind serve-http` brings up an HTTP server on 127.0.0.1 on a chosen (or
//! random) free port and exposes a SMALL, EXPLICIT allowlist of memory tools so
//! an external coordinator (e.g. a Python multi-agent runtime) can recall/save
//! against the brain as a callable tool.
//!
//! Design boundaries (deliberate, see docs/design / critic review):
//!   * NOT a blanket `/tool/:name` passthrough. Each route maps to exactly one
//!     dispatch tool. Destructive/bulk tools (delete, consolidate, import,
//!     export, vault) are NOT reachable here.
//!   * Bearer token required on every route (reuses the viewer's posture).
//!     Loopback bind only.
//!   * `X-Agent: <id>` is a self-asserted AUDIT HINT and an author tag — never
//!     authentication. It labels who-wrote-what; it does not grant access.
//!   * Calls go through `crate::mcp::dispatch`, the same dispatcher the MCP
//!     stdio loop uses, wrapped in panic isolation so one bad tool call cannot
//!     take down the server.

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::FutureExt;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::net::TcpListener;
use uuid::Uuid;

use crate::config::MindConfig;

#[derive(Clone)]
struct AppState {
    config: Arc<MindConfig>,
    /// token → agent identity. The authn seam for v2.0 multi-tenant (Д7):
    /// when an agent is named for a token, that identity is DERIVED from the
    /// presented token, not asserted by the `X-Agent` header. With per-agent
    /// tokens the author tag becomes trustworthy. The default single-token mode
    /// maps one generated token to `None` (anonymous) — backward compatible.
    tokens: Arc<std::collections::HashMap<String, Option<String>>>,
    /// In-memory read counter per agent (reads are too frequent for the
    /// append-only audit log). Gives the multi-agent graph a "who read how
    /// much" signal without disk churn; resets when the server restarts.
    reads: Arc<std::sync::Mutex<std::collections::HashMap<String, u64>>>,
}

/// Entry point used by `Commands::ServeHttp`. `agent_tokens` is a list of
/// `name:token` pairs; when empty, one anonymous token is generated. `host` is
/// the bind interface (defaults to `127.0.0.1`); binding a non-loopback host
/// exposes the brain beyond this machine, so it requires explicit `--agent-token`
/// auth — the loopback-only default was a deliberate security posture, and the
/// only safe way to relax it is to demand a real token.
pub async fn run(
    config: MindConfig,
    host: &str,
    port: Option<u16>,
    agent_tokens: Vec<String>,
) -> Result<()> {
    if !bind_is_allowed(host, &agent_tokens) {
        anyhow::bail!(
            "refusing to bind {host} (non-loopback) with an anonymous token — that \
             would expose an open brain. Pass --agent-token NAME:TOKEN to bind a \
             reachable interface."
        );
    }

    let mut tokens: std::collections::HashMap<String, Option<String>> =
        std::collections::HashMap::new();
    let mut generated: Option<String> = None;

    if agent_tokens.is_empty() {
        // Default: one anonymous bearer token (prior behavior).
        let token = Uuid::new_v4().to_string();
        tokens.insert(token.clone(), None);
        generated = Some(token);
    } else {
        // Per-agent tokens: identity is derived from the token, X-Agent ignored.
        for pair in &agent_tokens {
            let (name, tok) = pair
                .split_once(':')
                .ok_or_else(|| anyhow::anyhow!("--agent-token must be NAME:TOKEN, got '{pair}'"))?;
            if name.is_empty() || tok.is_empty() {
                anyhow::bail!("--agent-token NAME and TOKEN must both be non-empty: '{pair}'");
            }
            tokens.insert(tok.to_string(), Some(name.to_string()));
        }
    }

    // Sorted agent names for the startup banner (computed before `state` moves
    // into the router).
    let agent_names = {
        let mut n: Vec<String> = tokens.values().filter_map(|v| v.clone()).collect();
        n.sort_unstable();
        n.join(", ")
    };

    let state = AppState {
        config: Arc::new(config),
        tokens: Arc::new(tokens),
        reads: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
    };

    let app = Router::new()
        .route("/health", get(health))
        .route("/memory/search", post(memory_search))
        .route("/memory/browse", post(memory_browse))
        .route("/memory/recall", post(memory_recall))
        .route("/memory/add", post(memory_add))
        .route("/memory/ingest", post(memory_ingest))
        .route("/memory/by-agent", post(memory_by_agent))
        .route("/library/create", post(library_create))
        .route("/library/list", post(library_list))
        .route("/fact/add", post(fact_add))
        .route("/fact/query", post(fact_query))
        .route("/fact/invalidate", post(fact_invalidate))
        .route("/procedure/learn", post(procedure_learn))
        .route("/procedure/recall", post(procedure_recall))
        .route("/consolidate", post(consolidate_preview))
        .route("/quarantine/list", post(quarantine_list))
        .route("/quarantine/promote", post(quarantine_promote))
        .route("/session/start", post(session_start))
        .route("/session/end", post(session_end))
        .route("/session/last", post(session_last))
        .route("/session/context", post(session_context))
        .route("/stats/activity", post(stats_activity))
        .route("/should-search", post(should_search))
        .with_state(state);

    let bind = format!("{host}:{}", port.unwrap_or(0));
    let listener = TcpListener::bind(&bind)
        .await
        .with_context(|| format!("Failed to bind {bind}"))?;
    let addr = listener.local_addr().context("Failed to read bound port")?;

    let scope = if host == "127.0.0.1" || host == "::1" || host == "localhost" {
        "loopback"
    } else {
        "network-exposed"
    };
    eprintln!();
    eprintln!("  mgimind serve-http  •  {scope} tool-surface for multi-agent access");
    eprintln!("  ─────────────────────────────────────────────────────────────────");
    eprintln!("  url:    http://{addr}");
    if let Some(token) = &generated {
        eprintln!("  token:  {token}");
        eprintln!("  auth:   Authorization: Bearer {token}");
        eprintln!("  agent:  X-Agent: <id>   (self-asserted author tag, not auth)");
    } else {
        eprintln!("  auth:   per-agent tokens — identity DERIVED from the bearer token");
        eprintln!("  agents: {agent_names}");
    }
    eprintln!("  routes: POST /memory/{{search,browse,recall,add,ingest,by-agent}}");
    eprintln!("          POST /fact/{{add,query,invalidate}}  /procedure/{{learn,recall}}");
    eprintln!("          POST /library/{{create,list}}  /quarantine/{{list,promote}}");
    eprintln!("          POST /consolidate  /should-search");
    eprintln!("          POST /session/{{start,end,last,context}}  GET /health");
    eprintln!("  stop:   Ctrl-C");
    eprintln!();

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("http-api server error")?;
    eprintln!("  http-api stopped.");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    // Also catch SIGTERM: `docker stop` (and most process managers) send it, and
    // as PID 1 in a container an unhandled SIGTERM is ignored — without this every
    // `docker stop` would wait the full grace period and then SIGKILL.
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(_) => {
                let _ = ctrl_c.await;
                return;
            }
        };
        tokio::select! {
            _ = ctrl_c => {},
            _ = term.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = ctrl_c.await;
    }
}

// ----- auth + helpers --------------------------------------------------------

/// Bearer-token check. Loopback is not a trust boundary on its own, so every
/// route requires a valid token. Returns the agent identity DERIVED from the
/// token (Some when per-agent tokens are configured, None for the anonymous
/// single-token mode). A derived identity is trustworthy; the `X-Agent` header
/// is not.
fn check_auth(state: &AppState, headers: &HeaderMap) -> Result<Option<String>, StatusCode> {
    if let Some(auth) = headers.get("authorization")
        && let Ok(s) = auth.to_str()
        && let Some(t) = s.strip_prefix("Bearer ")
        && let Some(agent) = state.tokens.get(t)
    {
        return Ok(agent.clone());
    }
    Err(StatusCode::UNAUTHORIZED)
}

/// Count one read for the caller. Cheap in-memory tally; the agent is the
/// token-derived identity, falling back to the X-Agent header, then "anonymous".
fn note_read(state: &AppState, derived: &Option<String>, headers: &HeaderMap) {
    let who = derived
        .clone()
        .or_else(|| agent_of(headers))
        .unwrap_or_else(|| "anonymous".to_string());
    if let Ok(mut map) = state.reads.lock() {
        *map.entry(who).or_insert(0) += 1;
    }
}

/// The self-asserted caller id from `X-Agent`. Audit hint / author tag only.
fn agent_of(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-agent")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Call one dispatch tool with panic isolation; map the result to JSON.
/// A panicking tool becomes a 500, never a crashed worker. Mirrors the MCP
/// loop's `AssertUnwindSafe(...).catch_unwind().await` posture (mcp.rs:151).
async fn call(state: &AppState, tool: &str, args: Value) -> Response {
    let fut = crate::mcp::dispatch(Some(&state.config), tool, &args);
    match std::panic::AssertUnwindSafe(fut).catch_unwind().await {
        Ok(Ok(text)) => Json(json!({ "ok": true, "result": text })).into_response(),
        Ok(Err(e)) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": format!("{e:#}") })),
        )
            .into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "ok": false, "error": "tool panicked" })),
        )
            .into_response(),
    }
}

/// The requested response shape for a read route. `format` is a closed set, so
/// it is validated with an allow-list: an unknown value (`"yaml"`, a typo like
/// `"tex"`) is an input error, not a silent fall-through to the default — a
/// caller asking for text must not get JSON because it misspelled the word.
#[derive(Debug)]
enum Format {
    Json,
    Text,
}

/// Resolve the `format` arg. None defaults to JSON (an agent over HTTP wants
/// fields); `text`/`render` opt into the human block; anything else is rejected.
/// Returns `Err(unknown_value)` so the caller can 400 with the offending string.
fn resolve_format(args: &Value) -> Result<Format, String> {
    match args.get("format").and_then(|v| v.as_str()) {
        None | Some("json") => Ok(Format::Json),
        Some("text") | Some("render") => Ok(Format::Text),
        Some(other) => Err(other.to_string()),
    }
}

/// A 400 with the standard `{ok:false, error}` body.
fn bad_request(msg: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "ok": false, "error": msg })),
    )
        .into_response()
}

/// Common read args: `query` (required), `library`, `limit`, `tier`. `Err(())`
/// means the required `query` was missing — the caller turns it into a 400.
fn read_args(args: &Value) -> Result<(String, Option<String>, usize, u8), ()> {
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or(())?;
    let library = args
        .get("library")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
    let tier = args.get("tier").and_then(|v| v.as_u64()).unwrap_or(2) as u8;
    Ok((query, library, limit, tier))
}

/// `/memory/search` with `format=json`: the structured SearchResult list, with
/// the same optional metadata filters (author, source, date window, multi-library
/// OR) the MCP `mind_search` accepts. Goes straight to `storage::search_filtered`
/// (SearchResult is Serialize) rather than through the text-rendering dispatch.
async fn search_json(state: &AppState, args: &Value) -> Response {
    let (query, _library, limit, tier) = match read_args(args) {
        Ok(t) => t,
        Err(()) => return bad_request("missing required argument 'query'"),
    };
    let mfilter = crate::mcp::memory_filter_from_args(args);
    match crate::storage::search_filtered(&state.config, &query, &mfilter, limit, tier).await {
        Ok(results) => Json(json!({ "ok": true, "results": results })).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": format!("{e:#}") })),
        )
            .into_response(),
    }
}

/// `/memory/recall` with `format=json`: a `{facts, memories, procedures_text}`
/// object. Mirrors the silos `mind_recall_all` fuses into text, but keeps them
/// separate so a coordinator can route each. `memories` is the structured
/// SearchResult list; `facts` is a list of `{subject,predicate,object}`;
/// `procedures` has no stable struct yet, so it ships as a rendered string under
/// the deliberately-named `procedures_text` — the name says it is text, not a
/// structured field, so a parser is never misled about its shape.
async fn recall_json(state: &AppState, args: &Value) -> Response {
    let (query, _library, limit, _tier) = match read_args(args) {
        Ok(t) => t,
        Err(()) => return bad_request("missing required argument 'query'"),
    };
    let cfg = &state.config;

    let facts: Vec<Value> = match crate::knowledge::query_facts(cfg, &query).await {
        Ok(fs) => fs
            .iter()
            .filter(|f| f.valid)
            .take(limit)
            .map(|f| json!({ "subject": f.subject, "predicate": f.predicate, "object": f.object }))
            .collect(),
        Err(_) => Vec::new(),
    };

    let memories = crate::storage::search(cfg, &query, None, limit, 2)
        .await
        .unwrap_or_default();

    let procedures = match crate::procedure::recall(cfg, None, Some(&query), limit).await {
        Ok(p) => {
            let t = p.trim();
            if t.is_empty() || t.to_lowercase().starts_with("no ") {
                String::new()
            } else {
                t.to_string()
            }
        }
        Err(_) => String::new(),
    };

    Json(json!({
        "ok": true,
        "facts": facts,
        "memories": memories,
        "procedures_text": procedures,
    }))
    .into_response()
}

/// Resolve the author tag and merge it into the args under `agent`. A
/// token-derived identity (`derived`) is authoritative and OVERRIDES any
/// `X-Agent` header or body `agent` — you cannot impersonate another agent when
/// your token names you. In the anonymous single-token mode `derived` is None
/// and the self-asserted `X-Agent` header is used as before (audit hint only).
fn with_agent(mut args: Value, headers: &HeaderMap, derived: Option<String>) -> Value {
    let author = derived.or_else(|| agent_of(headers));
    if let Some(agent) = author
        && let Value::Object(map) = &mut args
    {
        map.insert("agent".to_string(), Value::String(agent));
    }
    args
}

// ----- routes ----------------------------------------------------------------

async fn health() -> Json<Value> {
    Json(json!({ "ok": true, "service": "mgimind-http", "version": env!("CARGO_PKG_VERSION") }))
}

async fn memory_search(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(args): Json<Value>,
) -> Response {
    let derived = match check_auth(&state, &headers) {
        Ok(d) => d,
        Err(c) => return c.into_response(),
    };
    note_read(&state, &derived, &headers);
    // `format: "json"` (the default for agents) returns the structured
    // SearchResult list — id, score, author, library, created_at — instead of
    // the human-readable text block, so a caller never has to regex-parse the
    // render. `format: "text"` keeps the legacy text; an unknown format is a 400.
    match resolve_format(&args) {
        Ok(Format::Json) => search_json(&state, &args).await,
        Ok(Format::Text) => call(&state, "mind_search", args).await,
        Err(other) => bad_request(&format!("unknown format '{other}' (use 'json' or 'text')")),
    }
}

/// Browse/list memories by metadata with no search query (the inventory path).
/// JSON by default returns the structured MemoryRecord list; `format: "text"`
/// returns the rendered block via `mind_browse`. Same metadata filters as search.
async fn memory_browse(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(args): Json<Value>,
) -> Response {
    let derived = match check_auth(&state, &headers) {
        Ok(d) => d,
        Err(c) => return c.into_response(),
    };
    note_read(&state, &derived, &headers);
    match resolve_format(&args) {
        Ok(Format::Json) => browse_json(&state, &args).await,
        Ok(Format::Text) => call(&state, "mind_browse", args).await,
        Err(other) => bad_request(&format!("unknown format '{other}' (use 'json' or 'text')")),
    }
}

/// `/memory/browse` with `format=json`: the structured MemoryRecord list.
async fn browse_json(state: &AppState, args: &Value) -> Response {
    let mfilter = crate::mcp::memory_filter_from_args(args);
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
    match crate::storage::list_filtered(&state.config, &mfilter, limit).await {
        // `truncated` tells a caller the result is a newest-window, not the whole
        // matching set — so it can page with created_before or narrow the filter.
        Ok((records, truncated)) => {
            Json(json!({ "ok": true, "results": records, "truncated": truncated })).into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": format!("{e:#}") })),
        )
            .into_response(),
    }
}

async fn memory_recall(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(args): Json<Value>,
) -> Response {
    let derived = match check_auth(&state, &headers) {
        Ok(d) => d,
        Err(c) => return c.into_response(),
    };
    note_read(&state, &derived, &headers);
    // Structured recall: a `{facts, memories, procedures_text}` object so a graph
    // can route each silo on its own. `format: "text"` falls back to the rendered
    // block; an unknown format is a 400.
    match resolve_format(&args) {
        Ok(Format::Json) => recall_json(&state, &args).await,
        Ok(Format::Text) => call(&state, "mind_recall_all", args).await,
        Err(other) => bad_request(&format!("unknown format '{other}' (use 'json' or 'text')")),
    }
}

async fn memory_add(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(args): Json<Value>,
) -> Response {
    let derived = match check_auth(&state, &headers) {
        Ok(d) => d,
        Err(c) => return c.into_response(),
    };
    call(&state, "mind_add", with_agent(args, &headers, derived)).await
}

async fn memory_ingest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(args): Json<Value>,
) -> Response {
    let derived = match check_auth(&state, &headers) {
        Ok(d) => d,
        Err(c) => return c.into_response(),
    };
    call(&state, "mind_ingest", with_agent(args, &headers, derived)).await
}

async fn fact_add(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(args): Json<Value>,
) -> Response {
    let derived = match check_auth(&state, &headers) {
        Ok(d) => d,
        Err(c) => return c.into_response(),
    };
    call(&state, "mind_fact_add", with_agent(args, &headers, derived)).await
}

// ----- HTTP/MCP parity (read + non-destructive) ------------------------------
// These routes expose MCP tools that were already implemented and locality-safe
// but reachable only over stdio, so an HTTP-only runtime (OpenAI Agents SDK,
// Node, CI) could not query facts, persist a procedure, preview consolidation,
// or list the quarantine. Each is a thin wrapper over the SAME dispatch the MCP
// loop uses. Destructive/bulk tools (delete, export, import-apply, vault,
// consolidate --apply) stay off this surface by design — `mind_consolidate`
// dispatch is dry-run-only, `mind_fact_invalidate` flips the validity flag (it
// does not delete), and `mind_quarantine` here is list/promote only.

/// `mind_fact_query`: structured facts for a subject (read).
async fn fact_query(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(args): Json<Value>,
) -> Response {
    if let Err(c) = check_auth(&state, &headers) {
        return c.into_response();
    }
    call(&state, "mind_fact_query", args).await
}

/// `mind_fact_invalidate`: mark a fact invalid by id. Non-destructive — the row
/// and its history stay; this flips the validity flag the duel model reads.
async fn fact_invalidate(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(args): Json<Value>,
) -> Response {
    if let Err(c) = check_auth(&state, &headers) {
        return c.into_response();
    }
    call(&state, "mind_fact_invalidate", args).await
}

/// `mind_learn`: persist an error->fix procedure (write, like add). Carries the
/// token-derived author so a multi-agent graph records who learned the lesson.
async fn procedure_learn(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(args): Json<Value>,
) -> Response {
    let derived = match check_auth(&state, &headers) {
        Ok(d) => d,
        Err(c) => return c.into_response(),
    };
    call(&state, "mind_learn", with_agent(args, &headers, derived)).await
}

/// `mind_recall`: recall matching error->fix procedures (read). Accepts the
/// `query` field used by the other read routes and maps it to the tool's
/// `context` arg, so an agent can use one convention across search/browse/recall.
async fn procedure_recall(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(mut args): Json<Value>,
) -> Response {
    if let Err(c) = check_auth(&state, &headers) {
        return c.into_response();
    }
    // Map `query` → `context` when `context`/`error` aren't already given.
    if let Value::Object(m) = &mut args
        && !m.contains_key("context")
        && !m.contains_key("error")
        && let Some(q) = m.get("query").cloned()
    {
        m.insert("context".into(), q);
    }
    call(&state, "mind_recall", args).await
}

/// `mind_consolidate`: DRY-RUN preview of dedup/decay/cold-prune. The dispatch
/// arm hardcodes apply=false; acting on it stays a CLI command. Preview only.
async fn consolidate_preview(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(args): Json<Value>,
) -> Response {
    if let Err(c) = check_auth(&state, &headers) {
        return c.into_response();
    }
    call(&state, "mind_consolidate", args).await
}

/// `mind_quarantine action=list`: list relevance-gate-filtered entries (read).
/// The route forces `action=list` so the write actions of the consolidated tool
/// (promote/etc) aren't reachable here; that requires the body be a JSON object.
async fn quarantine_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(mut args): Json<Value>,
) -> Response {
    if let Err(c) = check_auth(&state, &headers) {
        return c.into_response();
    }
    match &mut args {
        Value::Object(m) => {
            m.insert("action".into(), Value::String("list".into()));
        }
        _ => return bad_request("body must be a JSON object"),
    }
    call(&state, "mind_quarantine", args).await
}

/// `mind_quarantine_promote`: promote one quarantined entry to ordinary memory
/// by id. Non-destructive (it surfaces an already-stored entry).
async fn quarantine_promote(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(args): Json<Value>,
) -> Response {
    if let Err(c) = check_auth(&state, &headers) {
        return c.into_response();
    }
    call(&state, "mind_quarantine_promote", args).await
}

/// `mind_library action=list`: list libraries with counts (read). Forces
/// `action=list` so the consolidated tool's create/delete actions aren't
/// reachable here; requires the body be a JSON object.
async fn library_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(mut args): Json<Value>,
) -> Response {
    if let Err(c) = check_auth(&state, &headers) {
        return c.into_response();
    }
    match &mut args {
        Value::Object(m) => {
            m.insert("action".into(), Value::String("list".into()));
        }
        _ => return bad_request("body must be a JSON object"),
    }
    call(&state, "mind_library", args).await
}

/// `mind_context`: the session-start context digest (recent facts + libraries).
async fn session_context(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(args): Json<Value>,
) -> Response {
    if let Err(c) = check_auth(&state, &headers) {
        return c.into_response();
    }
    call(&state, "mind_context", args).await
}

// NOTE: `mind_web` is deliberately NOT exposed over HTTP. It performs an
// arbitrary OUTBOUND fetch, which on the MCP channel comes from the trusted
// frontier model but on the loopback HTTP channel comes from any process that
// reached the port with a token — that makes the server a confused deputy / SSRF
// vector (an attacker can drive it to internal URLs like the cloud metadata
// endpoint or the bundled Qdrant). An HTTP agent that needs to fetch has its own
// network; it does not need the brain to fetch on its behalf. `mind_web` stays
// on the CLI/MCP surface only.

/// Create a library by name. The only mutating-structure verb on the surface,
/// and a non-destructive one: `mind_create` -> `run_create` only adds a library
/// to the registry, never drops or deletes (drop/delete stay CLI-only). A Band
/// of agents needs this to make its own working library instead of relying on an
/// operator to pre-create one.
async fn library_create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(args): Json<Value>,
) -> Response {
    let derived = match check_auth(&state, &headers) {
        Ok(d) => d,
        Err(c) => return c.into_response(),
    };
    call(&state, "mind_create", with_agent(args, &headers, derived)).await
}

/// "What did agent X write." With per-agent tokens, omitting `agent` in the
/// body means "what did I write" (the token-derived identity); naming another
/// agent in the body queries that agent. In anonymous mode the body `agent` or
/// `X-Agent` header selects the target.
async fn memory_by_agent(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(args): Json<Value>,
) -> Response {
    let derived = match check_auth(&state, &headers) {
        Ok(d) => d,
        Err(c) => return c.into_response(),
    };
    note_read(&state, &derived, &headers);
    // For by-agent, the body `agent` (explicit target) wins; fall back to the
    // caller's own derived/header identity ("what did I write").
    let has_explicit = args.get("agent").and_then(|v| v.as_str()).is_some();
    let merged = if has_explicit {
        args
    } else {
        with_agent(args, &headers, derived)
    };
    call(&state, "mind_by_agent", merged).await
}

// ----- continuity (session) --------------------------------------------------
// The multi-agent payoff: an agent can resume from where a prior run (its own,
// or a teammate's) left off. The session is keyed by agent — with per-agent
// tokens the agent is derived from the token, so "resume my session" needs no
// body field; a coordinator can also resume a named agent by passing `agent`.

/// Inject `action` and resolve the agent (body > token/header) into the args,
/// then dispatch `mind_session`.
fn session_call_args(
    mut args: Value,
    action: &str,
    headers: &HeaderMap,
    derived: Option<String>,
) -> Value {
    if let Value::Object(map) = &mut args {
        map.insert("action".to_string(), Value::String(action.to_string()));
        // Explicit body `agent` wins (coordinator resuming a named agent);
        // otherwise use the token-derived / header identity.
        let has_explicit = map.get("agent").and_then(|v| v.as_str()).is_some();
        if !has_explicit && let Some(agent) = derived.or_else(|| agent_of(headers)) {
            map.insert("agent".to_string(), Value::String(agent));
        }
    }
    args
}

async fn session_start(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(args): Json<Value>,
) -> Response {
    let derived = match check_auth(&state, &headers) {
        Ok(d) => d,
        Err(c) => return c.into_response(),
    };
    call(
        &state,
        "mind_session",
        session_call_args(args, "start", &headers, derived),
    )
    .await
}

async fn session_end(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(args): Json<Value>,
) -> Response {
    let derived = match check_auth(&state, &headers) {
        Ok(d) => d,
        Err(c) => return c.into_response(),
    };
    call(
        &state,
        "mind_session",
        session_call_args(args, "end", &headers, derived),
    )
    .await
}

async fn session_last(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(args): Json<Value>,
) -> Response {
    let derived = match check_auth(&state, &headers) {
        Ok(d) => d,
        Err(c) => return c.into_response(),
    };
    call(
        &state,
        "mind_session",
        session_call_args(args, "last", &headers, derived),
    )
    .await
}

/// Per-agent read activity since the server started. The write side is already
/// queryable per-agent via /memory/by-agent (author index); this adds the read
/// side the audit log deliberately does not carry, completing the "who read and
/// wrote what" picture for a multi-agent graph. In-memory, resets on restart.
async fn stats_activity(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(c) = check_auth(&state, &headers) {
        return c.into_response();
    }
    let reads: std::collections::HashMap<String, u64> =
        state.reads.lock().map(|m| m.clone()).unwrap_or_default();
    Json(json!({ "ok": true, "reads_by_agent": reads })).into_response()
}

/// Query-aware "should I search memory before answering?" advice. Lets a client
/// gate its own answer on the search-before-answer policy.
async fn should_search(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(args): Json<Value>,
) -> Response {
    if let Err(c) = check_auth(&state, &headers) {
        return c.into_response();
    }
    call(&state, "mind_should_search", args).await
}

/// Whether the server may bind `host`. Loopback is always allowed (the safe
/// default). A non-loopback host (e.g. 0.0.0.0 for a Docker `-p` mapping) exposes
/// the brain beyond this machine, so it is allowed only when explicit per-agent
/// tokens are set — never with an anonymous token. Pure, so it is unit-tested.
fn bind_is_allowed(host: &str, agent_tokens: &[String]) -> bool {
    let is_loopback = matches!(host, "127.0.0.1" | "::1" | "localhost");
    is_loopback || !agent_tokens.is_empty()
}

#[cfg(test)]
mod tests {
    use super::{Format, bind_is_allowed, resolve_format};
    use serde_json::json;

    #[test]
    fn format_defaults_to_json_and_rejects_unknown() {
        // No `format` field, or "json" → structured JSON (the agent default).
        assert!(matches!(
            resolve_format(&json!({ "query": "x" })),
            Ok(Format::Json)
        ));
        assert!(matches!(
            resolve_format(&json!({ "format": "json" })),
            Ok(Format::Json)
        ));
        // Opt back into the human render.
        assert!(matches!(
            resolve_format(&json!({ "format": "text" })),
            Ok(Format::Text)
        ));
        assert!(matches!(
            resolve_format(&json!({ "format": "render" })),
            Ok(Format::Text)
        ));
        // Unknown value is an input error, NOT a silent fall-through — a typo
        // like "tex" must not quietly hand back JSON.
        assert_eq!(
            resolve_format(&json!({ "format": "yaml" })).unwrap_err(),
            "yaml"
        );
        assert_eq!(
            resolve_format(&json!({ "format": "tex" })).unwrap_err(),
            "tex"
        );
    }

    #[test]
    fn loopback_allowed_with_or_without_token() {
        for host in ["127.0.0.1", "::1", "localhost"] {
            assert!(bind_is_allowed(host, &[]));
            assert!(bind_is_allowed(host, &["u:tok".to_string()]));
        }
    }

    #[test]
    fn non_loopback_requires_a_token() {
        // 0.0.0.0 (the Docker case) must refuse an anonymous token...
        assert!(!bind_is_allowed("0.0.0.0", &[]));
        assert!(!bind_is_allowed("192.168.1.5", &[]));
        // ...but is allowed once a real token is set.
        assert!(bind_is_allowed("0.0.0.0", &["u:tok".to_string()]));
    }
}
