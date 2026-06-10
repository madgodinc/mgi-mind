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
        .route("/memory/recall", post(memory_recall))
        .route("/memory/add", post(memory_add))
        .route("/memory/ingest", post(memory_ingest))
        .route("/memory/by-agent", post(memory_by_agent))
        .route("/library/create", post(library_create))
        .route("/fact/add", post(fact_add))
        .route("/session/start", post(session_start))
        .route("/session/end", post(session_end))
        .route("/session/last", post(session_last))
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
    eprintln!("  routes: POST /memory/{{search,recall,add,ingest,by-agent}}  POST /fact/add");
    eprintln!("          POST /session/{{start,end,last}}  GET /health");
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

/// Whether the caller wants a structured JSON body for a read route. JSON is the
/// default — an agent calling over HTTP almost always wants to parse fields, not
/// a rendered text block. Opt back into the human render with `format: "text"`.
fn wants_json(args: &Value) -> bool {
    !matches!(
        args.get("format").and_then(|v| v.as_str()),
        Some("text") | Some("render")
    )
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

/// `/memory/search` with `format=json`: the structured SearchResult list. Goes
/// straight to `storage::search` (SearchResult is Serialize) rather than through
/// the text-rendering dispatch path.
async fn search_json(state: &AppState, args: &Value) -> Response {
    let (query, library, limit, tier) = match read_args(args) {
        Ok(t) => t,
        Err(()) => return bad_request("missing required argument 'query'"),
    };
    match crate::storage::search(&state.config, &query, library.as_deref(), limit, tier).await {
        Ok(results) => Json(json!({ "ok": true, "results": results })).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": format!("{e:#}") })),
        )
            .into_response(),
    }
}

/// `/memory/recall` with `format=json`: a `{facts, memories, procedures}` object.
/// Mirrors the silos `mind_recall_all` fuses into text, but keeps them separate
/// so a coordinator can route each. Facts/procedures stay as rendered lines (no
/// stable struct yet); memories are the structured SearchResult list.
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
        "procedures": procedures,
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
    // render. Omitting `format` (or `format: "text"`) keeps the legacy text.
    if wants_json(&args) {
        return search_json(&state, &args).await;
    }
    call(&state, "mind_search", args).await
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
    // Structured recall: a `{facts, memories, procedures}` object so a graph can
    // route each silo on its own. `format: "text"` falls back to the rendered
    // block. Unified recall: facts + memories + procedures in one call.
    if wants_json(&args) {
        return recall_json(&state, &args).await;
    }
    call(&state, "mind_recall_all", args).await
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
    use super::{bind_is_allowed, wants_json};
    use serde_json::json;

    #[test]
    fn json_is_the_default_format() {
        // No `format` field → structured JSON (the agent default).
        assert!(wants_json(&json!({ "query": "x" })));
        assert!(wants_json(&json!({ "query": "x", "format": "json" })));
        // Opt back into the human render.
        assert!(!wants_json(&json!({ "query": "x", "format": "text" })));
        assert!(!wants_json(&json!({ "query": "x", "format": "render" })));
        // Unknown value falls through to JSON, not text.
        assert!(wants_json(&json!({ "query": "x", "format": "yaml" })));
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
