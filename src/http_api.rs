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
    /// One-shot bearer token, generated per-process, printed once at startup.
    /// Required in `Authorization: Bearer <token>` on every route.
    token: Arc<String>,
}

/// Entry point used by `Commands::ServeHttp`.
pub async fn run(config: MindConfig, port: Option<u16>) -> Result<()> {
    let token = Uuid::new_v4().to_string();
    let state = AppState {
        config: Arc::new(config),
        token: Arc::new(token.clone()),
    };

    let app = Router::new()
        .route("/health", get(health))
        .route("/memory/search", post(memory_search))
        .route("/memory/recall", post(memory_recall))
        .route("/memory/add", post(memory_add))
        .route("/memory/ingest", post(memory_ingest))
        .route("/memory/by-agent", post(memory_by_agent))
        .route("/fact/add", post(fact_add))
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
    eprintln!("  token:  {token}");
    eprintln!("  auth:   Authorization: Bearer {token}");
    eprintln!("  agent:  X-Agent: <id>   (audit hint + author tag, not auth)");
    eprintln!(
        "  routes: POST /memory/{{search,recall,add,ingest,by-agent}}  POST /fact/add  GET /health"
    );
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
/// route requires the per-process token.
fn check_auth(state: &AppState, headers: &HeaderMap) -> Result<(), StatusCode> {
    if let Some(auth) = headers.get("authorization")
        && let Ok(s) = auth.to_str()
        && let Some(t) = s.strip_prefix("Bearer ")
        && t == state.token.as_str()
    {
        return Ok(());
    }
    Err(StatusCode::UNAUTHORIZED)
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

/// Merge the X-Agent header into the args object under `agent` (author tag),
/// without clobbering an explicit `agent` already in the body.
fn with_agent(mut args: Value, headers: &HeaderMap) -> Value {
    if let Some(agent) = agent_of(headers) {
        if let Value::Object(map) = &mut args {
            map.entry("agent").or_insert(Value::String(agent));
        }
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
    if let Err(c) = check_auth(&state, &headers) {
        return c.into_response();
    }
    call(&state, "mind_search", args).await
}

async fn memory_recall(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(args): Json<Value>,
) -> Response {
    if let Err(c) = check_auth(&state, &headers) {
        return c.into_response();
    }
    // mind_recall_all is not on this base; mind_search is the recall surface.
    call(&state, "mind_search", args).await
}

async fn memory_add(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(args): Json<Value>,
) -> Response {
    if let Err(c) = check_auth(&state, &headers) {
        return c.into_response();
    }
    call(&state, "mind_add", with_agent(args, &headers)).await
}

async fn memory_ingest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(args): Json<Value>,
) -> Response {
    if let Err(c) = check_auth(&state, &headers) {
        return c.into_response();
    }
    call(&state, "mind_ingest", with_agent(args, &headers)).await
}

async fn fact_add(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(args): Json<Value>,
) -> Response {
    if let Err(c) = check_auth(&state, &headers) {
        return c.into_response();
    }
    call(&state, "mind_fact_add", with_agent(args, &headers)).await
}

/// "What did agent X write." The agent is taken from the body `agent` field or
/// the `X-Agent` header (so a coordinator can ask about itself with just the
/// header, or about another agent by naming it in the body).
async fn memory_by_agent(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(args): Json<Value>,
) -> Response {
    if let Err(c) = check_auth(&state, &headers) {
        return c.into_response();
    }
    call(&state, "mind_by_agent", with_agent(args, &headers)).await
}
