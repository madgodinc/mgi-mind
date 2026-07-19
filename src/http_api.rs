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
    /// Serializes writes to the file-backed KV store (`/kv/set`, `/kv/get`).
    /// The KV surface is deliberately NOT a memory tool: it stores raw,
    /// caller-opaque blobs (e.g. an orchestrator's run checkpoint) with no
    /// chunking and no embedding, so the value round-trips byte-for-byte —
    /// searchable memory mangles a large JSON blob into chunks. One mutex +
    /// one JSON file is enough for the loopback, single-writer use case.
    kv_lock: Arc<tokio::sync::Mutex<()>>,
    /// v2.0 flood control: per-author write timestamps over a rolling 60s window.
    /// A runaway agent writing through /memory/add skips the ingest relevance
    /// gate, so serve-http caps writes per author (config.write_quota_per_min; 0
    /// disables). In-memory, resets on restart — a rate limit, not an audit.
    writes: Arc<
        std::sync::Mutex<
            std::collections::HashMap<String, std::collections::VecDeque<std::time::Instant>>,
        >,
    >,
    /// v2.0 library ACL: agent name -> the libraries a scoped token may touch.
    /// Populated only for `--agent-token NAME:TOKEN:lib1,lib2`. A name absent here
    /// is unscoped (full access), preserving the pre-v2.0 default.
    scopes: Arc<std::collections::HashMap<String, Vec<String>>>,
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
    let mut scopes: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    let mut generated: Option<String> = None;

    if agent_tokens.is_empty() {
        // Default: one anonymous bearer token (prior behavior).
        let token = Uuid::new_v4().to_string();
        tokens.insert(token.clone(), None);
        generated = Some(token);
    } else {
        // Per-agent tokens: parse NAME:TOKEN[:lib1,lib2]. Extracted into a pure
        // function so the fail-closed parsing (empty segments, duplicate names,
        // present-but-empty scope) is unit-testable without binding a socket.
        let (t, s) = parse_agent_tokens(&agent_tokens)?;
        tokens = t;
        scopes = s;
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
        kv_lock: Arc::new(tokio::sync::Mutex::new(())),
        writes: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        scopes: Arc::new(scopes),
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
        .route("/fact/contested", post(fact_contested))
        .route("/procedure/learn", post(procedure_learn))
        .route("/procedure/recall", post(procedure_recall))
        .route("/consolidate", post(consolidate_preview))
        .route("/quarantine/list", post(quarantine_list))
        .route("/quarantine/promote", post(quarantine_promote))
        .route("/memory/restore", post(memory_restore))
        .route("/session/start", post(session_start))
        .route("/session/end", post(session_end))
        .route("/session/last", post(session_last))
        .route("/session/context", post(session_context))
        .route("/stats/activity", post(stats_activity))
        .route("/should-search", post(should_search))
        .route("/audit", post(audit_recent))
        .route("/kv/set", post(kv_set))
        .route("/kv/get", post(kv_get))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            scope_gate,
        ))
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
    eprintln!(
        "          POST /fact/{{add,query,invalidate,contested}}  /procedure/{{learn,recall}}"
    );
    eprintln!(
        "          POST /library/{{create,list}}  /quarantine/{{list,promote}}  /memory/restore"
    );
    eprintln!("          POST /consolidate  /should-search  /audit");
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

/// Parse `--agent-token NAME:TOKEN[:lib1,lib2]` entries into the (token→name)
/// and (name→allowlist) maps. Fail-closed: an empty NAME or TOKEN, a duplicate
/// NAME, or a present-but-empty library scope (`NAME:TOKEN:` / `NAME:TOKEN:,`)
/// is an error rather than a silent fall-through to full access. TOKEN must be
/// colon-free (generated tokens are UUIDs): `splitn(3, ':')` folds everything
/// after the second ':' into the allowlist segment, so a ':' inside a token
/// would mis-split — documented, not defended, because tokens are UUIDs.
#[allow(clippy::type_complexity)]
fn parse_agent_tokens(
    agent_tokens: &[String],
) -> Result<(
    std::collections::HashMap<String, Option<String>>,
    std::collections::HashMap<String, Vec<String>>,
)> {
    let mut tokens: std::collections::HashMap<String, Option<String>> =
        std::collections::HashMap::new();
    let mut scopes: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    let mut seen_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    for pair in agent_tokens {
        let mut parts = pair.splitn(3, ':');
        let name = parts.next().unwrap_or("");
        let tok = parts.next().unwrap_or("");
        let libs = parts.next();
        if name.is_empty() || tok.is_empty() {
            anyhow::bail!("--agent-token must be NAME:TOKEN[:lib1,lib2], got '{pair}'");
        }
        if !seen_names.insert(name.to_string()) {
            // Two tokens sharing a name would merge scopes ambiguously (scopes
            // is keyed by name); reject rather than silently pick one.
            anyhow::bail!("--agent-token NAME '{name}' is used more than once");
        }
        tokens.insert(tok.to_string(), Some(name.to_string()));
        if let Some(libs) = libs {
            let list: Vec<String> = libs
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if list.is_empty() {
                // A present-but-empty scope (`NAME:TOKEN:` or `NAME:TOKEN:,`)
                // must not silently fall through to full access.
                anyhow::bail!(
                    "--agent-token '{name}' has an empty library scope; drop the \
                     trailing ':' for full access or name at least one library"
                );
            }
            scopes.insert(name.to_string(), list);
        }
    }
    Ok((tokens, scopes))
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

/// Count one read for the caller. Cheap in-memory tally, keyed on the
/// token-derived identity (else one shared "anonymous" bucket). Deliberately NOT
/// the self-asserted X-Agent header: keying on it would let an anonymous-mode
/// client grow this map without bound by rotating the header (the same DoS the
/// write quota avoids). The key set stays bounded by the configured agents.
fn note_read(state: &AppState, derived: &Option<String>) {
    let who = quota_key(derived);
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
    let rerank = crate::mcp::rerank_from_args(args);
    match crate::storage::search_filtered(&state.config, &query, &mfilter, limit, tier, rerank)
        .await
    {
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

    // Honour the same metadata filters as /memory/search (author, source,
    // libraries), so a library-scoped token's recall stays confined to its
    // allowlist — apply_scope injects `libraries` into args before we get here.
    let mfilter = crate::mcp::memory_filter_from_args(args);
    let memories =
        crate::storage::search_filtered(cfg, &query, &mfilter, limit, 2, Default::default())
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

/// The flood-control bucket key: the token-derived identity, else one shared
/// "anonymous" bucket. Deliberately NOT the self-asserted X-Agent header — in the
/// anonymous single-token mode a client could otherwise rotate X-Agent to both
/// evade the quota and grow the map without bound. The key set stays bounded by
/// the configured agent names plus "anonymous".
fn quota_key(derived: &Option<String>) -> String {
    derived.clone().unwrap_or_else(|| "anonymous".to_string())
}

/// Enforce the per-author write quota (config.write_quota_per_min over a rolling
/// 60s window). Err(429) once the caller has spent its budget; a quota of 0
/// disables the gate. It exists because /memory/add writes skip the ingest
/// relevance filter, so a looping agent can flood the shared pool directly.
fn check_write_quota(state: &AppState, who: &str) -> Option<Response> {
    let quota = state.config.write_quota_per_min;
    if quota == 0 {
        return None;
    }
    let now = std::time::Instant::now();
    let window = std::time::Duration::from_secs(60);
    // A poisoned lock must not wedge writes; fail open (availability beats a
    // best-effort rate limit).
    let mut map = match state.writes.lock() {
        Ok(m) => m,
        Err(_) => return None,
    };
    let hits = map.entry(who.to_string()).or_default();
    while let Some(&front) = hits.front() {
        if now.duration_since(front) >= window {
            hits.pop_front();
        } else {
            break;
        }
    }
    if hits.len() as u32 >= quota {
        return Some(
            (
                StatusCode::TOO_MANY_REQUESTS,
                Json(json!({
                    "ok": false,
                    "error": format!("write quota exceeded ({quota}/min for '{who}')"),
                })),
            )
                .into_response(),
        );
    }
    hits.push_back(now);
    None
}

/// Is this caller a library-scoped token (configured as NAME:TOKEN:lib1,lib2)?
fn is_scoped(state: &AppState, derived: &Option<String>) -> bool {
    derived
        .as_ref()
        .is_some_and(|name| state.scopes.contains_key(name))
}

/// A 403 for a library the token is not scoped to.
fn scope_denied(name: &str, lib: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({
            "ok": false,
            "error": format!("token '{name}' is not scoped to library '{lib}'"),
        })),
    )
        .into_response()
}

/// Enforce a scoped token's library allowlist (v2.0 ACL) at the library grain.
/// The route-level `scope_gate` middleware already fail-closes a scoped token out
/// of every route that can not honour an allowlist; this narrows the surviving
/// memory routes to the token's libraries.
///
/// A write commits to exactly one `library` and the write path ignores a
/// `libraries` filter, so a scoped write MUST name an allowlisted library
/// explicitly — injecting a filter would let an unlibraried write fall through to
/// the tool's default library (`mind_ingest` defaults to "projects"), outside the
/// allowlist. A read that names no library has the allowlist injected as its
/// `libraries` filter; a read naming a disallowed library is a 403. Unscoped and
/// anonymous tokens are unaffected.
fn apply_scope(
    state: &AppState,
    derived: &Option<String>,
    args: &mut Value,
    is_write: bool,
) -> Option<Response> {
    let name = derived.as_ref()?;
    let allowed = state.scopes.get(name)?;
    match scope_libs(allowed, args, is_write) {
        Ok(()) => None,
        Err(ScopeReject::MustNameLibrary) => Some(
            (
                StatusCode::FORBIDDEN,
                Json(json!({
                    "ok": false,
                    "error": format!(
                        "library-scoped token '{name}' must name a 'library' it is scoped to"
                    ),
                })),
            )
                .into_response(),
        ),
        Err(ScopeReject::NotAllowed(lib)) => Some(scope_denied(name, &lib)),
    }
}

/// Why a scoped request was rejected. Kept transport-free so the decision core
/// (`scope_libs`) is unit-testable without constructing an `AppState` or an axum
/// `Response`; `apply_scope` maps each variant to the exact same JSON body the
/// v2.0 contract locked.
enum ScopeReject {
    /// A write that did not name a `library` it is scoped to.
    MustNameLibrary,
    /// A read or write that named a library outside the allowlist (carried).
    NotAllowed(String),
}

/// Pure ACL decision for a library-scoped token: given the token's `allowed`
/// libraries, validate `args` and canonicalize its library filter in place, or
/// reject. No `AppState`, no `Response` — this is the entire enforcement logic,
/// separated so it can be exhaustively tested against adversarial bodies
/// (`{}`, `[]`, non-string entries, unicode, singular-vs-plural precedence).
///
/// A write commits to exactly one `library` and the write path ignores a
/// `libraries` filter, so a scoped write MUST name an allowlisted library
/// explicitly. A read that names no library has the allowlist injected as its
/// `libraries` filter; a read naming a disallowed library is rejected. The read
/// filter is always canonicalized to exactly the validated set with the singular
/// `library` dropped — otherwise a degenerate `libraries` (`[]`, `[123]`) left
/// present in the body would be read downstream (where `libraries` takes
/// precedence) as "all libraries", escaping the allowlist.
fn scope_libs(allowed: &[String], args: &mut Value, is_write: bool) -> Result<(), ScopeReject> {
    let Value::Object(obj) = args else {
        // A scoped token with a non-object body must fail closed, NOT pass through:
        // `/memory/browse` accepts a bare/degenerate body (no `query` required) and
        // would otherwise list every library. A write can not name its library, so
        // reject; a read is coerced to the allowlist filter.
        if is_write {
            return Err(ScopeReject::MustNameLibrary);
        }
        *args = json!({ "libraries": allowed });
        return Ok(());
    };

    if is_write {
        return match obj.get("library").and_then(|v| v.as_str()) {
            Some(l) if allowed.iter().any(|a| a == l) => Ok(()),
            Some(l) => Err(ScopeReject::NotAllowed(l.to_string())),
            None => Err(ScopeReject::MustNameLibrary),
        };
    }

    let mut requested: Vec<String> = Vec::new();
    if let Some(l) = obj.get("library").and_then(|v| v.as_str()) {
        requested.push(l.to_string());
    }
    if let Some(arr) = obj.get("libraries").and_then(|v| v.as_array()) {
        for v in arr {
            if let Some(s) = v.as_str() {
                requested.push(s.to_string());
            }
        }
    }
    if requested.is_empty() {
        // Confine an unlibraried read to the allowlist so it can not span other
        // agents' libraries.
        obj.remove("library");
        obj.insert(
            "libraries".to_string(),
            Value::Array(allowed.iter().map(|s| Value::String(s.clone())).collect()),
        );
        return Ok(());
    }
    for lib in &requested {
        if !allowed.contains(lib) {
            return Err(ScopeReject::NotAllowed(lib.clone()));
        }
    }
    obj.remove("library");
    obj.insert(
        "libraries".to_string(),
        Value::Array(requested.iter().map(|s| Value::String(s.clone())).collect()),
    );
    Ok(())
}

/// Route-level fail-closed gate for library-scoped tokens (v2.0). A scoped token
/// reaches only the memory routes that honour its allowlist (search/browse/recall
/// /add/ingest) plus health/should-search; every other route is 403 for it. Those
/// either span all libraries (by-agent, facts, quarantine, consolidate, audit),
/// share a global namespace (kv, sessions), or administer libraries — none of
/// which the library ACL can confine yet, so they are denied rather than leaked.
/// Unscoped and anonymous tokens pass through; auth still runs in each handler.
/// The only routes a library-scoped token may reach (v2.0). Pure so the
/// fail-closed allowlist is unit-testable without a live request: every route
/// NOT in this set must 403 for a scoped token, because it either spans all
/// libraries (by-agent, facts, quarantine, consolidate, audit), shares a global
/// namespace (kv, sessions), or administers libraries.
fn scoped_route_allowed(path: &str) -> bool {
    const ALLOWED_FOR_SCOPED: &[&str] = &[
        "/health",
        "/should-search",
        "/memory/search",
        "/memory/browse",
        "/memory/recall",
        "/memory/add",
        "/memory/ingest",
        // v2.4: by-agent is now confined (library allowlist injected) rather than
        // blanket-denied — the gate asks for confinement, not lockout.
        "/memory/by-agent",
    ];
    ALLOWED_FOR_SCOPED.contains(&path)
}

async fn scope_gate(
    State(state): State<AppState>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let scoped = req
        .headers()
        .get("authorization")
        .and_then(|a| a.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .and_then(|t| state.tokens.get(t))
        .and_then(|name| name.as_ref())
        .is_some_and(|name| state.scopes.contains_key(name));
    if scoped && !scoped_route_allowed(req.uri().path()) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "ok": false,
                "error": "route not available to a library-scoped token (2.0 confines \
                          scoped tokens to /memory/{search,browse,recall,add,ingest})",
            })),
        )
            .into_response();
    }
    next.run(req).await
}

// ----- routes ----------------------------------------------------------------

async fn health() -> Json<Value> {
    Json(json!({ "ok": true, "service": "mgimind-http", "version": env!("CARGO_PKG_VERSION") }))
}

async fn memory_search(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(mut args): Json<Value>,
) -> Response {
    let derived = match check_auth(&state, &headers) {
        Ok(d) => d,
        Err(c) => return c.into_response(),
    };
    note_read(&state, &derived);
    if let Some(resp) = apply_scope(&state, &derived, &mut args, false) {
        return resp;
    }
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
    Json(mut args): Json<Value>,
) -> Response {
    let derived = match check_auth(&state, &headers) {
        Ok(d) => d,
        Err(c) => return c.into_response(),
    };
    note_read(&state, &derived);
    if let Some(resp) = apply_scope(&state, &derived, &mut args, false) {
        return resp;
    }
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
    Json(mut args): Json<Value>,
) -> Response {
    let derived = match check_auth(&state, &headers) {
        Ok(d) => d,
        Err(c) => return c.into_response(),
    };
    note_read(&state, &derived);
    if let Some(resp) = apply_scope(&state, &derived, &mut args, false) {
        return resp;
    }
    // Structured recall: a `{facts, memories, procedures_text}` object so a graph
    // can route each silo on its own. `format: "text"` falls back to the rendered
    // block; an unknown format is a 400.
    match resolve_format(&args) {
        Ok(Format::Json) => recall_json(&state, &args).await,
        // The text render (mind_recall_all) fuses memories from ALL libraries and
        // ignores the injected `libraries` filter, so it can not be confined. A
        // scoped token must use the JSON path (search_filtered honours the scope).
        Ok(Format::Text) if is_scoped(&state, &derived) => (
            StatusCode::FORBIDDEN,
            Json(json!({
                "ok": false,
                "error": "library-scoped tokens must use format=json on /memory/recall",
            })),
        )
            .into_response(),
        Ok(Format::Text) => call(&state, "mind_recall_all", args).await,
        Err(other) => bad_request(&format!("unknown format '{other}' (use 'json' or 'text')")),
    }
}

async fn memory_add(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(mut args): Json<Value>,
) -> Response {
    let derived = match check_auth(&state, &headers) {
        Ok(d) => d,
        Err(c) => return c.into_response(),
    };
    // Scope before quota so a 403 does not spend the caller's write budget.
    if let Some(resp) = apply_scope(&state, &derived, &mut args, true) {
        return resp;
    }
    let who = quota_key(&derived);
    if let Some(resp) = check_write_quota(&state, &who) {
        return resp;
    }
    call(&state, "mind_add", with_agent(args, &headers, derived)).await
}

async fn memory_ingest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(mut args): Json<Value>,
) -> Response {
    let derived = match check_auth(&state, &headers) {
        Ok(d) => d,
        Err(c) => return c.into_response(),
    };
    // Scope before quota so a 403 does not spend the caller's write budget.
    if let Some(resp) = apply_scope(&state, &derived, &mut args, true) {
        return resp;
    }
    // A library-scoped token's ingest must confine fact/procedure candidates (they
    // would land in the GLOBAL stores, escaping the library allowlist). Set the
    // flag server-side — overwrite any client value so it can't be bypassed from
    // the request body.
    if let Value::Object(ref mut obj) = args {
        obj.insert(
            "_confine_extraction".to_string(),
            Value::Bool(is_scoped(&state, &derived)),
        );
    }
    let who = quota_key(&derived);
    if let Some(resp) = check_write_quota(&state, &who) {
        return resp;
    }
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
    // Structured path (like search_json): call the knowledge layer directly so the
    // duel verdict comes back. The text dispatch returns only an id, which reads as
    // "stored as truth" even when the duel contested or quarantined the write.
    let subject = args.get("subject").and_then(|v| v.as_str());
    let predicate = args.get("predicate").and_then(|v| v.as_str());
    let object = args.get("object").and_then(|v| v.as_str());
    let (subject, predicate, object) = match (subject, predicate, object) {
        (Some(s), Some(p), Some(o)) if !s.is_empty() && !p.is_empty() && !o.is_empty() => (s, p, o),
        _ => return bad_request("fact requires non-empty 'subject', 'predicate', 'object'"),
    };
    // Quota after validation so a malformed request does not spend the budget.
    let who = quota_key(&derived);
    if let Some(resp) = check_write_quota(&state, &who) {
        return resp;
    }
    // Facts carry no library, so they are outside the library ACL — attribute to
    // the token-derived author, else the X-Agent hint, else a body `agent` field
    // (the last is honoured only in anonymous mode, matching the old dispatch).
    let author = derived.or_else(|| agent_of(&headers)).or_else(|| {
        args.get("agent")
            .and_then(|v| v.as_str())
            .map(str::to_string)
    });
    match crate::knowledge::add_fact_authored_verdict(
        &state.config,
        subject,
        predicate,
        object,
        author.as_deref(),
    )
    .await
    {
        Ok((id, verdict)) => {
            Json(json!({ "ok": true, "id": id, "verdict": verdict })).into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": format!("{e:#}") })),
        )
            .into_response(),
    }
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
/// and its history stay; this flips the validity flag the duel model reads. The
/// caller's token-derived identity is attached as the audit actor (who hid it).
async fn fact_invalidate(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(args): Json<Value>,
) -> Response {
    let derived = match check_auth(&state, &headers) {
        Ok(d) => d,
        Err(c) => return c.into_response(),
    };
    // Attribute to the token-derived identity, else the X-Agent hint, else "http"
    // — an anonymous HTTP invalidate must read as "http", never fall through to
    // the MCP/cli default. So we resolve the actor here and inject it explicitly.
    let mut args = with_agent(args, &headers, derived);
    if let Value::Object(m) = &mut args
        && !m.contains_key("agent")
    {
        m.insert("agent".into(), Value::String("http".into()));
    }
    call(&state, "mind_fact_invalidate", args).await
}

/// v2.0: list facts the duel left unresolved — `contested` (both values live) and
/// `quarantine_candidate` (a weak newcomer held back). The read half of
/// cross-agent conflict resolution: a coordinator sees where its agents disagreed
/// instead of the disagreement staying invisible. Read-only; returns the
/// structured Fact list (Fact is Serialize). `limit` caps the scan (default 50).
async fn fact_contested(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(args): Json<Value>,
) -> Response {
    let derived = match check_auth(&state, &headers) {
        Ok(d) => d,
        Err(c) => return c.into_response(),
    };
    note_read(&state, &derived);
    // Clamp so a caller can not scroll the whole facts collection into RAM.
    let limit = (args.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize).min(2000);
    match crate::knowledge::list_contested(&state.config, limit).await {
        Ok(facts) => Json(json!({ "ok": true, "results": facts })).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": format!("{e:#}") })),
        )
            .into_response(),
    }
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
    let who = quota_key(&derived);
    if let Some(resp) = check_write_quota(&state, &who) {
        return resp;
    }
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

/// `mind_restore`: un-archive a soft-forgotten memory by id (non-destructive).
async fn memory_restore(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(args): Json<Value>,
) -> Response {
    if let Err(c) = check_auth(&state, &headers) {
        return c.into_response();
    }
    call(&state, "mind_restore", args).await
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
    note_read(&state, &derived);
    // v2.4 confinement: a library-scoped token's by-agent read is restricted to
    // its own allowlist (computed before `derived` is moved into with_agent).
    // Server-set so it can't be widened from the request body.
    let scope_libs: Option<Vec<String>> =
        derived.as_ref().and_then(|n| state.scopes.get(n)).cloned();
    // For by-agent, the body `agent` (explicit target) wins; fall back to the
    // caller's own derived/header identity ("what did I write").
    let has_explicit = args.get("agent").and_then(|v| v.as_str()).is_some();
    let mut merged = if has_explicit {
        args
    } else {
        with_agent(args, &headers, derived)
    };
    if let (Some(libs), Value::Object(obj)) = (&scope_libs, &mut merged) {
        obj.insert("_scope_libs".to_string(), json!(libs));
    }
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

/// The append-only audit trail, most-recent-N. This is the Track-3 / Crescendo
/// "prove every decision" substrate over HTTP: a multi-agent run reads it to
/// render an attributable decision ledger (each event carries op / actor /
/// target / before / after / note / timestamp). `limit` caps the count
/// (default 200, hard max 2000 so a caller can't pull an unbounded log into a
/// browser). `since_ts` is an optional RFC3339 lower bound (inclusive) for
/// incremental fetch — a dashboard polls with the last timestamp it saw.
/// Read-only; the log itself is never mutated here.
async fn audit_recent(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(args): Json<Value>,
) -> Response {
    if let Err(c) = check_auth(&state, &headers) {
        return c.into_response();
    }
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(200)
        .min(2000) as usize;
    let mut events = match crate::audit::recent(limit) {
        Ok(e) => e,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "ok": false, "error": format!("{e:#}") })),
            )
                .into_response();
        }
    };
    // Optional inclusive lower bound on timestamp for incremental polling.
    if let Some(since) = args.get("since_ts").and_then(|v| v.as_str()) {
        events.retain(|e| e.ts.as_str() >= since);
    }
    Json(json!({ "ok": true, "events": events })).into_response()
}

/// Path to the single file-backed KV store under the data dir.
fn kv_path(state: &AppState) -> std::path::PathBuf {
    state.config.data_dir.join("kv_store.json")
}

/// Load the whole KV map (missing/corrupt file → empty map, so a torn write
/// degrades to "key not found" rather than a hard error).
fn kv_load(state: &AppState) -> serde_json::Map<String, Value> {
    match std::fs::read_to_string(kv_path(state)) {
        Ok(s) => serde_json::from_str::<serde_json::Map<String, Value>>(&s).unwrap_or_default(),
        Err(_) => serde_json::Map::new(),
    }
}

/// `/kv/set` — store a raw `value` (any JSON) under `key`. Opaque to the brain:
/// no chunking, no embedding, byte-for-byte round-trip. Built for an external
/// orchestrator to persist run state (crash-proof resume). Writes are serialized
/// by `kv_lock` and committed atomically (temp file + rename) so a crash
/// mid-write cannot corrupt other keys.
async fn kv_set(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(args): Json<Value>,
) -> Response {
    if check_auth(&state, &headers).is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let key = match args.get("key").and_then(|v| v.as_str()) {
        Some(k) if !k.is_empty() => k.to_string(),
        _ => return bad_request("kv/set requires a non-empty string `key`"),
    };
    if args.get("value").is_none() {
        return bad_request("kv/set requires a `value`");
    }
    let value = args.get("value").cloned().unwrap();

    let _guard = state.kv_lock.lock().await;
    let mut map = kv_load(&state);
    map.insert(key, value);
    let path = kv_path(&state);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("json.tmp");
    let body = match serde_json::to_string(&map) {
        Ok(b) => b,
        Err(e) => return bad_request(&format!("serialize failed: {e}")),
    };
    if let Err(e) = std::fs::write(&tmp, &body).and_then(|_| std::fs::rename(&tmp, &path)) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "ok": false, "error": format!("kv write failed: {e}") })),
        )
            .into_response();
    }
    Json(json!({ "ok": true, "result": "stored" })).into_response()
}

/// `/kv/get` — fetch the raw value previously stored under `key`. Returns
/// `{ok:true, found:false}` when absent (not an error — the caller decides).
async fn kv_get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(args): Json<Value>,
) -> Response {
    if check_auth(&state, &headers).is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let key = match args.get("key").and_then(|v| v.as_str()) {
        Some(k) if !k.is_empty() => k.to_string(),
        _ => return bad_request("kv/get requires a non-empty string `key`"),
    };
    let _guard = state.kv_lock.lock().await;
    let map = kv_load(&state);
    match map.get(&key) {
        Some(v) => Json(json!({ "ok": true, "found": true, "value": v })).into_response(),
        None => Json(json!({ "ok": true, "found": false })).into_response(),
    }
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
    use super::{
        Format, ScopeReject, bind_is_allowed, parse_agent_tokens, resolve_format, scope_libs,
        scoped_route_allowed,
    };
    use serde_json::json;

    fn libs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

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

    // ---- scoped_route_allowed (fail-closed route gate) ----

    #[test]
    fn scoped_gate_allows_only_the_eight_memory_routes() {
        for ok in [
            "/health",
            "/should-search",
            "/memory/search",
            "/memory/browse",
            "/memory/recall",
            "/memory/add",
            "/memory/ingest",
            // v2.4: by-agent is confined (allowlist injected), no longer denied.
            "/memory/by-agent",
        ] {
            assert!(scoped_route_allowed(ok), "{ok} should be reachable");
        }
        // Everything that spans libraries, shares a global namespace, or
        // administers libraries must be denied to a scoped token.
        for denied in [
            "/fact/add",
            "/fact/query",
            "/fact/contested",
            "/procedure/learn",
            "/consolidate",
            "/quarantine/list",
            "/quarantine/promote",
            "/memory/restore",
            "/session/start",
            "/session/context",
            "/stats/activity",
            "/audit",
            "/kv/set",
            "/kv/get",
            "/library/create",
            "/library/list",
            "/",
            "/memory",
            "/memory/search/", // trailing slash is a different path — deny
            "/MEMORY/SEARCH",  // case-sensitive — deny
            "",
        ] {
            assert!(!scoped_route_allowed(denied), "{denied} must be denied");
        }
    }

    // ---- scope_libs (per-library ACL decision) ----

    #[test]
    fn scope_read_with_no_library_injects_the_allowlist() {
        let allow = libs(&["a", "b"]);
        let mut args = json!({ "query": "x" });
        assert!(scope_libs(&allow, &mut args, false).is_ok());
        assert_eq!(args["libraries"], json!(["a", "b"]));
        assert!(args.get("library").is_none());
    }

    #[test]
    fn scope_read_canonicalizes_a_valid_singular_library() {
        let allow = libs(&["a", "b"]);
        let mut args = json!({ "library": "a", "query": "x" });
        assert!(scope_libs(&allow, &mut args, false).is_ok());
        // singular dropped, filter is exactly the validated set
        assert!(args.get("library").is_none());
        assert_eq!(args["libraries"], json!(["a"]));
    }

    #[test]
    fn scope_read_rejects_a_disallowed_library() {
        let allow = libs(&["a"]);
        let mut args = json!({ "library": "secret" });
        assert!(matches!(
            scope_libs(&allow, &mut args, false),
            Err(ScopeReject::NotAllowed(l)) if l == "secret"
        ));
    }

    #[test]
    fn scope_read_rejects_if_any_requested_library_is_disallowed() {
        let allow = libs(&["a", "b"]);
        let mut args = json!({ "libraries": ["a", "secret"] });
        assert!(matches!(
            scope_libs(&allow, &mut args, false),
            Err(ScopeReject::NotAllowed(l)) if l == "secret"
        ));
    }

    #[test]
    fn scope_read_checks_both_singular_and_plural_together() {
        // A valid `library` must not launder a disallowed `libraries` entry.
        let allow = libs(&["a"]);
        let mut args = json!({ "library": "a", "libraries": ["secret"] });
        assert!(matches!(
            scope_libs(&allow, &mut args, false),
            Err(ScopeReject::NotAllowed(l)) if l == "secret"
        ));
    }

    #[test]
    fn scope_read_empty_plural_array_is_not_all_libraries() {
        // `[]` reduces to no requested libs → must fall back to the allowlist,
        // never to an unfiltered (all-libraries) read.
        let allow = libs(&["a", "b"]);
        let mut args = json!({ "libraries": [] });
        assert!(scope_libs(&allow, &mut args, false).is_ok());
        assert_eq!(args["libraries"], json!(["a", "b"]));
    }

    #[test]
    fn scope_read_ignores_non_string_entries() {
        let allow = libs(&["a", "b"]);
        // Only "a" is a real request; 123/null/object are ignored.
        let mut args = json!({ "libraries": [123, null, {"x":1}, "a"] });
        assert!(scope_libs(&allow, &mut args, false).is_ok());
        assert_eq!(args["libraries"], json!(["a"]));
        // A plural of ONLY non-strings collapses to empty → allowlist fallback,
        // not "all libraries".
        let mut only_junk = json!({ "libraries": [123, null] });
        assert!(scope_libs(&allow, &mut only_junk, false).is_ok());
        assert_eq!(only_junk["libraries"], json!(["a", "b"]));
    }

    #[test]
    fn scope_read_non_object_body_is_coerced_to_the_allowlist() {
        let allow = libs(&["a"]);
        for mut body in [json!(null), json!([1, 2, 3]), json!("string"), json!(7)] {
            assert!(scope_libs(&allow, &mut body, false).is_ok());
            assert_eq!(body, json!({ "libraries": ["a"] }));
        }
    }

    #[test]
    fn scope_read_matches_unicode_exactly() {
        let allow = libs(&["проекты"]);
        let mut ok = json!({ "library": "проекты" });
        assert!(scope_libs(&allow, &mut ok, false).is_ok());
        assert_eq!(ok["libraries"], json!(["проекты"]));
        let mut no = json!({ "library": "Проекты" }); // different first codepoint
        assert!(matches!(
            scope_libs(&allow, &mut no, false),
            Err(ScopeReject::NotAllowed(_))
        ));
    }

    #[test]
    fn scope_write_must_name_an_allowlisted_library() {
        let allow = libs(&["a", "b"]);
        // named + allowed → ok
        assert!(scope_libs(&allow, &mut json!({ "library": "a", "content": "x" }), true).is_ok());
        // named + disallowed → reject with the offending name
        assert!(matches!(
            scope_libs(&allow, &mut json!({ "library": "z" }), true),
            Err(ScopeReject::NotAllowed(l)) if l == "z"
        ));
        // unnamed → must-name
        assert!(matches!(
            scope_libs(&allow, &mut json!({ "content": "x" }), true),
            Err(ScopeReject::MustNameLibrary)
        ));
        // a `libraries` filter does NOT satisfy a write — it commits to one lib
        assert!(matches!(
            scope_libs(&allow, &mut json!({ "libraries": ["a"] }), true),
            Err(ScopeReject::MustNameLibrary)
        ));
        // non-object write → must-name
        assert!(matches!(
            scope_libs(&allow, &mut json!(null), true),
            Err(ScopeReject::MustNameLibrary)
        ));
    }

    // ---- parse_agent_tokens (fail-closed token parser) ----

    #[test]
    fn parse_tokens_two_part_is_unscoped() {
        let (tokens, scopes) = parse_agent_tokens(&["alice:tok123".to_string()]).unwrap();
        assert_eq!(tokens.get("tok123"), Some(&Some("alice".to_string())));
        assert!(
            scopes.is_empty(),
            "a two-part token has full (unscoped) access"
        );
    }

    #[test]
    fn parse_tokens_three_part_scopes_and_trims() {
        let (_t, scopes) =
            parse_agent_tokens(&["bob:tok: projects , avtokvartal ".to_string()]).unwrap();
        assert_eq!(
            scopes.get("bob").unwrap(),
            &libs(&["projects", "avtokvartal"])
        );
    }

    #[test]
    fn parse_tokens_rejects_empty_name_or_token() {
        assert!(parse_agent_tokens(&[":tok".to_string()]).is_err());
        assert!(parse_agent_tokens(&["name:".to_string()]).is_err());
        assert!(parse_agent_tokens(&["".to_string()]).is_err());
    }

    #[test]
    fn parse_tokens_rejects_duplicate_names() {
        let err = parse_agent_tokens(&["a:t1".to_string(), "a:t2".to_string()]).unwrap_err();
        assert!(err.to_string().contains("more than once"));
    }

    #[test]
    fn parse_tokens_rejects_present_but_empty_scope() {
        // Trailing ':' or an all-empty list must NOT fall through to full access.
        assert!(parse_agent_tokens(&["a:t:".to_string()]).is_err());
        assert!(parse_agent_tokens(&["a:t:,".to_string()]).is_err());
        assert!(parse_agent_tokens(&["a:t: , ".to_string()]).is_err());
    }

    #[test]
    fn parse_tokens_colon_in_token_mis_splits_by_design() {
        // Documented limitation: splitn(3) folds everything after the 2nd ':'
        // into the scope segment. Tokens are UUIDs (colon-free), so this is
        // locked as known behavior, not defended against.
        let (_t, scopes) = parse_agent_tokens(&["a:t:x:y".to_string()]).unwrap();
        assert_eq!(scopes.get("a").unwrap(), &libs(&["x:y"]));
    }
}
