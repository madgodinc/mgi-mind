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
/// `name:token` pairs; when empty, one anonymous token is generated.
pub async fn run(
    config: MindConfig,
    port: Option<u16>,
    agent_tokens: Vec<String>,
) -> Result<()> {
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
        .route("/fact/add", post(fact_add))
        .route("/session/start", post(session_start))
        .route("/session/end", post(session_end))
        .route("/session/last", post(session_last))
        .route("/stats/activity", post(stats_activity))
        .route("/should-search", post(should_search))
        .with_state(state);

    let bind = format!("127.0.0.1:{}", port.unwrap_or(0));
    let listener = TcpListener::bind(&bind)
        .await
        .with_context(|| format!("Failed to bind {bind}"))?;
    let addr = listener.local_addr().context("Failed to read bound port")?;

    eprintln!();
    eprintln!("  mgimind serve-http  •  loopback tool-surface for multi-agent access");
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
    eprintln!(
        "  routes: POST /memory/{{search,recall,add,ingest,by-agent}}  POST /fact/add"
    );
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
    let _ = tokio::signal::ctrl_c().await;
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
    // Unified recall: facts + memories + procedures in one call.
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
fn session_call_args(mut args: Value, action: &str, headers: &HeaderMap, derived: Option<String>) -> Value {
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
    call(&state, "mind_session", session_call_args(args, "start", &headers, derived)).await
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
    call(&state, "mind_session", session_call_args(args, "end", &headers, derived)).await
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
    call(&state, "mind_session", session_call_args(args, "last", &headers, derived)).await
}

/// Per-agent read activity since the server started. The write side is already
/// queryable per-agent via /memory/by-agent (author index); this adds the read
/// side the audit log deliberately does not carry, completing the "who read and
/// wrote what" picture for a multi-agent graph. In-memory, resets on restart.
async fn stats_activity(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(c) = check_auth(&state, &headers) {
        return c.into_response();
    }
    let reads: std::collections::HashMap<String, u64> = state
        .reads
        .lock()
        .map(|m| m.clone())
        .unwrap_or_default();
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
