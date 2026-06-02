//! In-process MCP server over stdio (phase 0.1). One `mgimind mcp` process is
//! the whole MCP server: it lives for the entire client session, so the ONNX
//! session + tokenizer (cached in `OnceCell`s) load once and stay warm - no
//! daemon, no Unix socket, no Node wrapper.
//!
//! Transport: newline-delimited JSON-RPC 2.0 on stdin/stdout (the MCP stdio
//! spec - one message object per line, no embedded newlines). Requests are
//! handled strictly sequentially in the read loop: a single stdio client means
//! no concurrency, so no `Mutex`/session pool is needed.
//!
//! CRITICAL stdout discipline: stdout carries ONLY JSON-RPC. Any stray
//! `println!`/`print!` on this path corrupts the stream and kills the session.
//! All logging goes to stderr (configured in `main`).
//!
//! Tool-execution failures are returned as a `result` with `isError: true`
//! (NOT a JSON-RPC error) - a JSON-RPC error makes the client think the server
//! itself broke and drop the session.

use anyhow::Result;
use futures_util::FutureExt;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::config::MindConfig;

/// Protocol version we implement. We advertise our own supported version rather
/// than echoing the client's (echoing risks claiming a version we don't honor);
/// the client reconciles any mismatch.
const PROTOCOL_VERSION: &str = "2024-11-05";

/// JSON-RPC 2.0 error codes (subset we use).
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;

/// Run the stdio MCP server until stdin reaches EOF.
pub async fn serve() -> Result<()> {
    // Config load is cheap (reads a small JSON file) and independent of model
    // warmth (the embedder lives in a global OnceCell). Load once; if it fails
    // (e.g. `mgimind init` not run yet) we still answer initialize/tools/list so
    // the client can connect, and surface the problem on the first tool call.
    let config = match crate::config::load_cached() {
        Ok(c) => Some(c),
        Err(e) => {
            eprintln!(
                "mgimind mcp: config not loaded ({e}); tool calls will report this until `mgimind init`"
            );
            None
        }
    };

    // Bring Qdrant up (detached) so a minimal user never runs `serve` by hand.
    // Best-effort: a failure here (e.g. binary missing) must NOT stop us from
    // serving - we still answer initialize/tools/list, and embed-path tools then
    // return an actionable error pointing at `mgimind doctor --fix`.
    if let Err(e) = crate::cli::ensure_qdrant_running().await {
        eprintln!(
            "mgimind mcp: Qdrant not started ({e}); run `mgimind doctor --fix` if tools fail"
        );
    }

    let stdin = tokio::io::stdin();
    let mut lines = BufReader::new(stdin).lines();
    let mut stdout = tokio::io::stdout();

    eprintln!("mgimind mcp: ready (stdio JSON-RPC, protocol {PROTOCOL_VERSION})");

    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }

        let response = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => handle_message(config.as_ref(), msg).await,
            // Parse failure: id is unknown, so per JSON-RPC it must be null.
            Err(e) => Some(error_response(
                Value::Null,
                PARSE_ERROR,
                &format!("parse error: {e}"),
            )),
        };

        // Notifications (and successfully-handled ones with no reply) produce None.
        if let Some(resp) = response {
            let mut out = resp.to_string();
            out.push('\n');
            if stdout.write_all(out.as_bytes()).await.is_err() {
                break;
            }
            let _ = stdout.flush().await;
        }
    }

    Ok(())
}

/// Dispatch one parsed JSON-RPC message. Returns `Some(response)` for requests
/// (messages with an `id`) and `None` for notifications (no `id`).
async fn handle_message(config: Option<&MindConfig>, msg: Value) -> Option<Value> {
    let id = msg.get("id").cloned();
    let is_request = id.is_some();
    let method = msg.get("method").and_then(Value::as_str);

    let Some(method) = method else {
        // No method: invalid for a request, ignorable for a notification.
        return is_request.then(|| {
            error_response(
                id.clone().unwrap_or(Value::Null),
                INVALID_REQUEST,
                "missing method",
            )
        });
    };

    let params = msg.get("params").cloned().unwrap_or(Value::Null);

    match method {
        "initialize" => {
            let id = id?; // notifications can't be "initialize", but be safe
            Some(success_response(id, initialize_result()))
        }
        // Lifecycle notifications: no reply.
        "notifications/initialized" | "notifications/cancelled" => None,
        "ping" => id.map(|id| success_response(id, json!({}))),
        "tools/list" => id.map(|id| success_response(id, json!({ "tools": tool_definitions() }))),
        "tools/call" => {
            let id = id?;
            // Panic isolation (single-process risk): one `mgimind mcp` process
            // serves the whole session, so a panic inside a tool handler - a
            // stray `unwrap`/`expect`/overflow anywhere down the call tree - would
            // unwind through the read loop and kill the server, silently cutting
            // off the user's memory mid-task. Catch it here and return it as a
            // normal `isError` result instead; the session survives.
            let result = std::panic::AssertUnwindSafe(call_tool(config, params))
                .catch_unwind()
                .await
                .unwrap_or_else(|panic| {
                    tool_text(
                        format!(
                            "Error: internal panic in tool handler: {}",
                            panic_message(&panic)
                        ),
                        true,
                    )
                });
            Some(success_response(id, result))
        }
        other => is_request.then(|| {
            error_response(
                id.clone().unwrap_or(Value::Null),
                METHOD_NOT_FOUND,
                &format!("method not found: {other}"),
            )
        }),
    }
}

/// Best-effort retrieval policy. The MCP protocol gives a server no way to
/// gate generation, so this is text the client *may* surface to the model — it
/// is not enforcement. Phrased as triggers, not rules, so the model can apply
/// judgment instead of mechanically calling tools on every turn.
const RETRIEVAL_INSTRUCTIONS: &str = "\
mgi-mind is the user's persistent memory across sessions. Before answering, \
consider calling mind_search when the user: (1) asks about a project, person, \
preference, or decision by name; (2) uses meta-cues like \"did I tell you\", \
\"do you remember\", \"have you forgotten\", \"you should know\"; (3) negates \
something to verify (\"isn't it X?\"); (4) references prior conversations. \
Use mind_context once at session start to see what libraries exist. This is \
best-effort guidance, not a hard requirement — the model decides when retrieval \
is worth the round-trip.";

fn initialize_result() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "mgi-mind", "version": env!("CARGO_PKG_VERSION") },
        "instructions": RETRIEVAL_INSTRUCTIONS
    })
}

fn success_response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// A tool result with text content. `is_error: true` signals a tool-execution
/// failure WITHOUT a protocol error, so the client keeps the session and the
/// model can read/recover from the message.
fn tool_text(text: impl Into<String>, is_error: bool) -> Value {
    json!({
        "content": [{ "type": "text", "text": text.into() }],
        "isError": is_error
    })
}

/// Best-effort human-readable text from a caught panic payload.
fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = panic.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

// ---------------------------------------------------------------------------
// Argument helpers - missing/invalid args are reported as isError results (so
// the model can self-correct), never as protocol errors.
// ---------------------------------------------------------------------------

fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn arg_u64(args: &Value, key: &str, default: u64) -> u64 {
    args.get(key).and_then(Value::as_u64).unwrap_or(default)
}

fn arg_bool(args: &Value, key: &str, default: bool) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(default)
}

/// Dispatch `tools/call`. Always returns a tool-result Value (never a protocol
/// error) - failures become `isError: true` text.
async fn call_tool(config: Option<&MindConfig>, params: Value) -> Value {
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return tool_text("tools/call: missing tool name", true);
    };
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    match dispatch(config, name, &args).await {
        Ok(text) => tool_text(text, false),
        Err(e) => tool_text(format!("Error: {e}"), true),
    }
}

/// Map a tool name + arguments to its rendered text. All 26 tools are wired:
/// - 7 "warm" embed-path tools reuse the existing `render_*`/`build_*` helpers
///   + storage/knowledge functions, using the pre-loaded warm config;
/// - 11 tools call text-returning `crate::cli::run_*` cores (download/doctor
///   progress goes to stderr so stdout stays pure JSON-RPC);
/// - 3 vault tools return static instruction text - secrets never flow over MCP.
async fn dispatch(config: Option<&MindConfig>, name: &str, args: &Value) -> Result<String> {
    // Tools that need storage/knowledge require a loaded config + running Qdrant.
    let warm = |needs_config: bool| -> Result<&MindConfig> {
        let _ = needs_config;
        config.ok_or_else(|| {
            anyhow::anyhow!("MGI-Mind is not initialized. Run `mgimind init` in a terminal first.")
        })
    };

    match name {
        // ---- Warm 7 (embed path; reuse existing helpers) ----
        "mind_search" => {
            let cfg = warm(true)?;
            let query = arg_str(args, "query")
                .ok_or_else(|| anyhow::anyhow!("missing required argument 'query'"))?;
            let library = arg_str(args, "library");
            let limit = arg_u64(args, "limit", 5) as usize;
            let tier = arg_u64(args, "tier", 2) as u8;
            let results = crate::storage::search(cfg, query, library, limit, tier).await?;
            Ok(crate::cli::render_search(&results))
        }
        "mind_add" => {
            let cfg = warm(true)?;
            let library = arg_str(args, "library")
                .ok_or_else(|| anyhow::anyhow!("missing required argument 'library'"))?;
            let content = arg_str(args, "content")
                .ok_or_else(|| anyhow::anyhow!("missing required argument 'content'"))?;
            let source = arg_str(args, "source");
            let n = crate::storage::add_memory(cfg, library, content, source).await?;
            Ok(format!("Added {n} chunk(s) to '{library}'"))
        }
        "mind_provenance_add" => {
            // Strict variant of mind_add: provenance is required and validated.
            // See docs/design/provenance-add.md and src/provenance.rs.
            //
            // Argument presence + validation run BEFORE `warm(...)` so a
            // bad-args call returns an actionable message even on a system
            // that has not been initialized yet (mirrors how the agent learns
            // the surface from `tools/list` + a couple of probe calls).
            let snippet = arg_str(args, "snippet")
                .ok_or_else(|| anyhow::anyhow!("missing required argument 'snippet'"))?;
            let origin_url = arg_str(args, "origin_url")
                .ok_or_else(|| anyhow::anyhow!("missing required argument 'origin_url'"))?;
            let search_tool_used = arg_str(args, "search_tool_used")
                .ok_or_else(|| anyhow::anyhow!("missing required argument 'search_tool_used'"))?;
            let library = arg_str(args, "library").unwrap_or(crate::provenance::DEFAULT_LIBRARY);
            let input = crate::provenance::ProvenanceInput {
                library,
                snippet,
                origin_url,
                repo: arg_str(args, "repo"),
                file: arg_str(args, "file"),
                line_range: arg_str(args, "line_range"),
                lang: arg_str(args, "lang"),
                search_tool_used,
                note: arg_str(args, "note"),
            };
            // Validate BEFORE touching storage. Failures come back as plain
            // tool-error text so the agent can self-correct.
            if let Err(e) = crate::provenance::validate(&input) {
                anyhow::bail!("{e}");
            }
            // Validation passed; now we need a live config to actually persist.
            let cfg = warm(true)?;
            let content = crate::provenance::format_content(&input);
            let source = crate::provenance::source_tag(&input);
            let id = crate::provenance::dedup_id(
                input.library,
                input.snippet,
                input.origin_url,
                input.line_range,
            );
            let n = crate::storage::add_memory(cfg, input.library, &content, Some(&source)).await?;
            // `add_memory` is idempotent (UUIDv5 of library+content collapses
            // repeat writes), so we always report success here — the dedup id
            // surfaces the canonical provenance identifier.
            Ok(format!(
                "Saved {n} chunk(s) to '{library}' (provenance id: {id})"
            ))
        }
        "mind_context" => {
            let cfg = warm(true)?;
            crate::cli::build_context(cfg).await
        }
        "mind_ingest" => {
            let cfg = warm(true)?;
            let raw = arg_str(args, "raw");
            let library = arg_str(args, "library").unwrap_or("projects");
            // Agent-driven (primary): a tagged-JSON candidates array. Backstop:
            // omit candidates and pass `raw` for the heuristic extractor.
            let candidates = match args.get("candidates") {
                Some(v) if !v.is_null() => {
                    serde_json::from_value::<Vec<crate::ingest::Candidate>>(v.clone())
                        .map_err(|e| anyhow::anyhow!("invalid 'candidates': {e}"))?
                }
                _ => Vec::new(),
            };
            if raw.is_none() && candidates.is_empty() {
                anyhow::bail!("mind_ingest needs either 'raw' text or a 'candidates' array");
            }
            let report = crate::ingest::run_ingest(cfg, raw, candidates, library).await?;
            Ok(report.render())
        }
        "mind_history" => {
            let cfg = warm(true)?;
            let limit = arg_u64(args, "limit", 10) as usize;
            let results = crate::storage::history(cfg, limit).await?;
            Ok(crate::cli::render_history(&results))
        }
        "mind_stats" => {
            let cfg = warm(true)?;
            crate::cli::build_stats(cfg).await
        }
        "mind_fact_add" => {
            let cfg = warm(true)?;
            let subject = arg_str(args, "subject")
                .ok_or_else(|| anyhow::anyhow!("missing required argument 'subject'"))?;
            let predicate = arg_str(args, "predicate")
                .ok_or_else(|| anyhow::anyhow!("missing required argument 'predicate'"))?;
            let object = arg_str(args, "object")
                .ok_or_else(|| anyhow::anyhow!("missing required argument 'object'"))?;
            let id = crate::knowledge::add_fact(cfg, subject, predicate, object).await?;
            Ok(format!(
                "Fact added: {subject} -> {predicate} -> {object} [id: {id}]"
            ))
        }
        "mind_fact_query" => {
            let cfg = warm(true)?;
            let subject = arg_str(args, "subject")
                .ok_or_else(|| anyhow::anyhow!("missing required argument 'subject'"))?;
            let facts = crate::knowledge::query_facts(cfg, subject).await?;
            Ok(crate::cli::render_facts(subject, &facts))
        }

        // ---- Procedural memory (Д6): learn / recall / outcome ----
        "mind_learn" => {
            let cfg = warm(true)?;
            let error = arg_str(args, "error")
                .ok_or_else(|| anyhow::anyhow!("missing required argument 'error'"))?;
            let fix = arg_str(args, "fix")
                .ok_or_else(|| anyhow::anyhow!("missing required argument 'fix'"))?;
            let context = arg_str(args, "context").unwrap_or("");
            let provenance = arg_str(args, "provenance");
            // verified defaults false: a manual lesson has no deterministic signal.
            let verified = arg_bool(args, "verified", false);
            crate::procedure::learn(cfg, error, fix, context, provenance, verified).await
        }
        "mind_recall" => {
            let cfg = warm(true)?;
            let error = arg_str(args, "error");
            let context = arg_str(args, "context");
            if error.is_none() && context.is_none() {
                anyhow::bail!("mind_recall needs an 'error' and/or a 'context'");
            }
            let limit = arg_u64(args, "limit", 3) as usize;
            crate::procedure::recall(cfg, error, context, limit).await
        }
        "mind_procedure_outcome" => {
            let cfg = warm(true)?;
            let id = arg_str(args, "id")
                .ok_or_else(|| anyhow::anyhow!("missing required argument 'id'"))?;
            let worked = arg_bool(args, "worked", false);
            crate::procedure::outcome(cfg, id, worked).await
        }

        // ---- Terminal-only 3 (vault; never touch secrets over MCP) ----
        "mind_vault_store" => {
            let key = arg_str(args, "key").unwrap_or("<key>");
            Ok(format!(
                "For security, secret values are never accepted over this channel.\n\
                 Store \"{key}\" yourself in a terminal:\n\n\
                 \x20\x20\x20\x20mgimind vault store {key} <value> --category <password|ssh|api-key|token>\n\n\
                 You'll be prompted for the master password (hidden)."
            ))
        }
        "mind_vault_get" => {
            let key = arg_str(args, "key").unwrap_or("<key>");
            Ok(format!(
                "For security, secrets are never returned over this channel.\n\
                 Retrieve \"{key}\" yourself in a terminal:\n\n\
                 \x20\x20\x20\x20mgimind vault get {key}\n\n\
                 You'll be prompted for the master password (hidden) and a confirmation."
            ))
        }
        "mind_vault_list" => {
            // list_keys() prompts for the master password on a TTY, which MCP
            // has no access to - so this is terminal-only too, not a run_* tool.
            Ok("For security, the vault requires a terminal.\n\
                List your stored keys yourself:\n\n\
                \x20\x20\x20\x20mgimind vault list\n\n\
                You'll be prompted for the master password (hidden)."
                .to_string())
        }

        // ---- run_* tools (load their own config; warmth lives in the embedder
        // OnceCell, not config, so re-loading config per call is cheap) ----
        "mind_create" => {
            let name = arg_str(args, "name")
                .ok_or_else(|| anyhow::anyhow!("missing required argument 'name'"))?;
            crate::cli::run_create(name).await
        }
        "mind_list" => crate::cli::run_list().await,
        "mind_doctor" => crate::cli::run_doctor(arg_bool(args, "fix", false)).await,
        "mind_delete" => {
            let library = arg_str(args, "library")
                .ok_or_else(|| anyhow::anyhow!("missing required argument 'library'"))?;
            let id = arg_str(args, "id")
                .ok_or_else(|| anyhow::anyhow!("missing required argument 'id'"))?;
            crate::cli::run_delete(library, id).await
        }
        "mind_web" => {
            let url = arg_str(args, "url")
                .ok_or_else(|| anyhow::anyhow!("missing required argument 'url'"))?;
            crate::cli::run_web(url, arg_str(args, "save")).await
        }
        "mind_export" => {
            let format = arg_str(args, "format").unwrap_or("json");
            crate::cli::run_export(format, arg_str(args, "output")).await
        }
        "mind_import" => {
            let source = arg_str(args, "source").unwrap_or("obsidian");
            let path = arg_str(args, "path")
                .ok_or_else(|| anyhow::anyhow!("missing required argument 'path'"))?;
            let library = arg_str(args, "library").unwrap_or("imported");
            // MCP defaults to apply=false (dry-run): import via an agent is
            // exactly the case where we want a plan-first guard. The agent can
            // re-call with apply=true after the user OKs the plan.
            let apply = arg_bool(args, "apply", false);
            crate::cli::run_import(source, path, library, apply).await
        }
        "mind_session_start" => {
            let agent = arg_str(args, "agent").unwrap_or("unknown");
            crate::cli::run_session_start(agent).await
        }
        "mind_session_last" => crate::cli::run_session_last(arg_str(args, "agent")).await,
        "mind_session_end" => {
            let agent = arg_str(args, "agent").unwrap_or("unknown");
            let summary = arg_str(args, "summary")
                .ok_or_else(|| anyhow::anyhow!("missing required argument 'summary'"))?;
            crate::cli::run_session_end(agent, summary).await
        }
        "mind_fact_invalidate" => {
            let id = arg_str(args, "id")
                .ok_or_else(|| anyhow::anyhow!("missing required argument 'id'"))?;
            crate::cli::run_fact_invalidate(id).await
        }

        // Test-only handler that panics, so a test can prove the panic-isolation
        // wrapper in `tools/call` turns a handler panic into an `isError` result
        // instead of unwinding through the read loop and killing the session.
        #[cfg(test)]
        "mind_test_panic" => panic!("boom (test panic)"),

        other => Err(anyhow::anyhow!("unknown tool: {other}")),
    }
}

/// The 26 tool definitions advertised by `tools/list`. Schemas are hand-written
/// from the zod schemas in `mcp-server/index.js` (1:1, so signatures don't
/// drift). `inputSchema` is a JSON Schema object per tool.
fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "mind_search",
            "description": "Semantic search across memories",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search query" },
                    "library": { "type": "string", "description": "Filter by library" },
                    "limit": { "type": "number", "default": 5, "description": "Max results" },
                    "tier": { "type": "number", "default": 2, "description": "Retrieval tier: 1=facts, 2=summaries, 3=full" }
                },
                "required": ["query"]
            }
        }),
        json!({
            "name": "mind_add",
            "description": "Add a memory entry",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "library": { "type": "string", "description": "Library name" },
                    "content": { "type": "string", "description": "Content to store" },
                    "source": { "type": "string", "description": "Source tag" }
                },
                "required": ["library", "content"]
            }
        }),
        json!({
            "name": "mind_provenance_add",
            "description": "Persist an externally-sourced snippet (code, doc, RFC quote, commit message, etc.) into mgi-mind with a mandatory provenance citation. The agent supplies the snippet AS PLAIN UTF-8 — no HTML, no markup. Call this ONLY when the snippet was just produced by a code-search or doc-search MCP in the same session; do NOT fill provenance fields from memory.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "library":          { "type": "string", "default": "external-snippets", "description": "Target library. Must exist (create with mind_create)." },
                    "snippet":          { "type": "string", "description": "Raw text to store. Plain UTF-8. Must NOT contain HTML tags; the agent is responsible for stripping markup upstream." },
                    "origin_url":       { "type": "string", "description": "https:// URL the snippet was lifted from. Host must be in the allowlist (github.com, gitlab.com, bitbucket.org, sr.ht, codeberg.org, grep.app, sourcegraph.com)." },
                    "repo":             { "type": "string", "description": "Optional owner/repo when the source is a code host. Regex: ^[\\w.-]+/[\\w.-]+$." },
                    "file":             { "type": "string", "description": "Optional path inside the repo. No leading '/', no '..' segments." },
                    "line_range":       { "type": "string", "description": "Optional line range, e.g. \"42\" or \"42-58\". Regex: ^\\d+(-\\d+)?$." },
                    "lang":             { "type": "string", "description": "Optional language tag (free string)." },
                    "search_tool_used": { "type": "string", "description": "Identifier of the search source the agent used in THIS session, e.g. \"mcp.grep.app\", \"sourcegraph\", \"github code search\", \"local ripgrep\". REQUIRED. Empty rejects with 'provenance source unknown — use mind_add instead'." },
                    "note":             { "type": "string", "description": "Optional one-liner the agent attaches (why this is worth keeping)." }
                },
                "required": ["snippet", "origin_url", "search_tool_used"]
            }
        }),
        json!({
            "name": "mind_fact_add",
            "description": "Add a knowledge graph fact",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "subject": { "type": "string" },
                    "predicate": { "type": "string" },
                    "object": { "type": "string" }
                },
                "required": ["subject", "predicate", "object"]
            }
        }),
        json!({
            "name": "mind_fact_query",
            "description": "Query facts about a subject",
            "inputSchema": {
                "type": "object",
                "properties": { "subject": { "type": "string" } },
                "required": ["subject"]
            }
        }),
        json!({
            "name": "mind_fact_invalidate",
            "description": "Soft-delete a fact by ID (for superseding a changed single-valued fact)",
            "inputSchema": {
                "type": "object",
                "properties": { "id": { "type": "string", "description": "Fact ID from mind_fact_query" } },
                "required": ["id"]
            }
        }),
        json!({
            "name": "mind_session_start",
            "description": "Start a new session",
            "inputSchema": {
                "type": "object",
                "properties": { "agent": { "type": "string", "default": "unknown", "description": "Agent name" } }
            }
        }),
        json!({
            "name": "mind_session_last",
            "description": "Get last session summary",
            "inputSchema": {
                "type": "object",
                "properties": { "agent": { "type": "string", "description": "Only consider this agent's sessions" } }
            }
        }),
        json!({
            "name": "mind_session_end",
            "description": "End the active session for an agent",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "agent": { "type": "string", "default": "unknown", "description": "Same agent name used in mind_session_start" },
                    "summary": { "type": "string", "description": "Session summary" }
                },
                "required": ["summary"]
            }
        }),
        json!({
            "name": "mind_create",
            "description": "Create a new library",
            "inputSchema": {
                "type": "object",
                "properties": { "name": { "type": "string" } },
                "required": ["name"]
            }
        }),
        json!({
            "name": "mind_list",
            "description": "List all libraries",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "mind_doctor",
            "description": "Check system health",
            "inputSchema": {
                "type": "object",
                "properties": { "fix": { "type": "boolean", "default": false } }
            }
        }),
        json!({
            "name": "mind_delete",
            "description": "Delete a specific memory by ID",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "library": { "type": "string", "description": "Library name" },
                    "id": { "type": "string", "description": "Memory UUID (from search results)" }
                },
                "required": ["library", "id"]
            }
        }),
        json!({
            "name": "mind_context",
            "description": "Generate compact context briefing for session start",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "mind_ingest",
            "description": "Auto-ingest memory. PRIMARY: pass a `candidates` array of already-extracted items you judged worth keeping. BACKSTOP: omit candidates and pass `raw` text for marker-based heuristic extraction. Each candidate is secret-scrubbed and near-dup-checked before writing.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "candidates": {
                        "type": "array",
                        "description": "Extracted items. Each is one of: {\"type\":\"memory\",\"content\":\"...\"} | {\"type\":\"fact\",\"subject\":\"...\",\"predicate\":\"...\",\"object\":\"...\"} | {\"type\":\"procedure\",\"trigger_error\":\"...\",\"fix\":\"...\"}",
                        "items": { "type": "object" }
                    },
                    "raw": { "type": "string", "description": "Raw text for heuristic extraction (used only when candidates is omitted)" },
                    "library": { "type": "string", "default": "projects", "description": "Target library for memory candidates" }
                }
            }
        }),
        json!({
            "name": "mind_learn",
            "description": "Record a procedural-memory lesson (error -> fix). Stored unverified by default (a manual lesson has no truth signal); surfaced with low weight until confirmed. Pass verified=true ONLY with a deterministic signal (test green / exit 0).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "error": { "type": "string", "description": "Error message / signature (will be normalized: paths, line numbers, addresses stripped)" },
                    "fix": { "type": "string", "description": "What resolved it" },
                    "context": { "type": "string", "description": "Short task description (drives semantic recall)" },
                    "provenance": { "type": "string", "description": "Project / file where this applied" },
                    "verified": { "type": "boolean", "default": false, "description": "True ONLY if a deterministic check confirmed the fix" }
                },
                "required": ["error", "fix"]
            }
        }),
        json!({
            "name": "mind_recall",
            "description": "Recall error->fix playbooks for an error and/or a task context. Verified procedures rank first; fixes that have failed before are demoted.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "error": { "type": "string", "description": "Error signature to match (lexical)" },
                    "context": { "type": "string", "description": "Task description to match (semantic)" },
                    "limit": { "type": "number", "default": 3, "description": "Max playbooks" }
                }
            }
        }),
        json!({
            "name": "mind_procedure_outcome",
            "description": "Record whether a recalled procedure worked when reused. worked=false raises its fail count and demotes it, so the store self-corrects instead of ossifying on a bad fix.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Procedure id from mind_recall" },
                    "worked": { "type": "boolean", "default": false, "description": "Did the fix work this time" }
                },
                "required": ["id", "worked"]
            }
        }),
        json!({
            "name": "mind_history",
            "description": "Show recent additions chronologically",
            "inputSchema": {
                "type": "object",
                "properties": { "limit": { "type": "number", "default": 10 } }
            }
        }),
        json!({
            "name": "mind_web",
            "description": "Read a webpage as Markdown, optionally save to library",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "URL to read" },
                    "save": { "type": "string", "description": "Library to save into (omit to just read)" }
                },
                "required": ["url"]
            }
        }),
        json!({
            "name": "mind_export",
            "description": "Export all data to JSON or Markdown files",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "format": { "type": "string", "default": "json", "description": "json or md" },
                    "output": { "type": "string", "default": "./mgimind-export", "description": "Output directory" }
                }
            }
        }),
        json!({
            "name": "mind_import",
            "description": "Import markdown files from a directory (Obsidian, etc.)",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "source": { "type": "string", "default": "obsidian", "description": "Source type: obsidian, markdown" },
                    "path": { "type": "string", "description": "Path to vault/directory" },
                    "library": { "type": "string", "default": "imported", "description": "Target library name" }
                },
                "required": ["path"]
            }
        }),
        json!({
            "name": "mind_vault_store",
            "description": "Explain how to store a secret (never accepts the secret over MCP)",
            "inputSchema": {
                "type": "object",
                "properties": { "key": { "type": "string", "description": "Key name the user wants to store" } },
                "required": ["key"]
            }
        }),
        json!({
            "name": "mind_vault_get",
            "description": "Explain how to retrieve a secret (never returns plaintext over MCP)",
            "inputSchema": {
                "type": "object",
                "properties": { "key": { "type": "string", "description": "Key name" } },
                "required": ["key"]
            }
        }),
        json!({
            "name": "mind_vault_list",
            "description": "Explain how to list stored secret keys (terminal-only; needs the master password)",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "mind_stats",
            "description": "Show memory statistics",
            "inputSchema": { "type": "object", "properties": {} }
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_all_26_tools() {
        let tools = tool_definitions();
        assert_eq!(
            tools.len(),
            26,
            "tools/list must advertise exactly 26 tools"
        );
    }

    #[test]
    fn provenance_add_is_listed() {
        let tools = tool_definitions();
        let found = tools
            .iter()
            .any(|t| t.get("name").and_then(Value::as_str) == Some("mind_provenance_add"));
        assert!(found, "mind_provenance_add must appear in tools/list");
    }

    #[test]
    fn every_tool_has_name_and_schema() {
        for t in tool_definitions() {
            assert!(
                t.get("name").and_then(Value::as_str).is_some(),
                "tool missing name: {t}"
            );
            assert_eq!(
                t.get("inputSchema")
                    .and_then(|s| s.get("type"))
                    .and_then(Value::as_str),
                Some("object"),
                "tool {t} inputSchema must be an object schema"
            );
        }
    }

    #[tokio::test]
    async fn initialize_returns_our_protocol_version() {
        let msg = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} });
        let resp = handle_message(None, msg)
            .await
            .expect("initialize is a request");
        assert_eq!(resp["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(resp["result"]["serverInfo"]["name"], "mgi-mind");
    }

    #[tokio::test]
    async fn initialize_carries_retrieval_instructions() {
        // The MCP `instructions` field is the only programmatic channel for
        // best-effort "search before answer" policy. If it disappears, clients
        // lose the only signal we have — guard it.
        let msg = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} });
        let resp = handle_message(None, msg).await.unwrap();
        let instr = resp["result"]["instructions"]
            .as_str()
            .expect("initialize result must include `instructions`");
        assert!(
            instr.contains("mind_search"),
            "instructions must mention mind_search"
        );
        assert!(
            instr.contains("best-effort"),
            "instructions must call this best-effort (no MCP enforcement)"
        );
    }

    #[tokio::test]
    async fn initialized_notification_has_no_reply() {
        let msg = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        assert!(handle_message(None, msg).await.is_none());
    }

    #[tokio::test]
    async fn tools_list_returns_26() {
        let msg = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" });
        let resp = handle_message(None, msg).await.unwrap();
        assert_eq!(resp["result"]["tools"].as_array().unwrap().len(), 26);
    }

    #[tokio::test]
    async fn unknown_method_is_method_not_found() {
        let msg = json!({ "jsonrpc": "2.0", "id": 3, "method": "nope/nope" });
        let resp = handle_message(None, msg).await.unwrap();
        assert_eq!(resp["error"]["code"], METHOD_NOT_FOUND);
    }

    #[tokio::test]
    async fn vault_tools_never_touch_secrets() {
        for tool in ["mind_vault_get", "mind_vault_store", "mind_vault_list"] {
            let params = json!({ "name": tool, "arguments": { "key": "k" } });
            let res = call_tool(None, params).await;
            let text = res["content"][0]["text"].as_str().unwrap();
            assert!(
                text.contains("terminal"),
                "{tool} must redirect to a terminal"
            );
            assert_eq!(
                res["isError"], false,
                "{tool} returns instruction, not an error"
            );
        }
    }

    #[tokio::test]
    async fn warm_tool_without_config_reports_init_needed() {
        let params = json!({ "name": "mind_search", "arguments": { "query": "x" } });
        let res = call_tool(None, params).await;
        assert_eq!(res["isError"], true);
        assert!(
            res["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("mgimind init")
        );
    }

    // -----------------------------------------------------------------------
    // mind_provenance_add: argument + validation rejections must surface as
    // `isError: true` tool results (so the agent can self-correct), never as
    // protocol errors and never as silent successes. These run without a real
    // config because the tool checks arguments + validation BEFORE warm-up.
    // -----------------------------------------------------------------------

    async fn provenance_call(args: Value) -> (bool, String) {
        let params = json!({ "name": "mind_provenance_add", "arguments": args });
        let res = call_tool(None, params).await;
        let is_err = res["isError"].as_bool().unwrap_or(false);
        let text = res["content"][0]["text"].as_str().unwrap_or("").to_string();
        (is_err, text)
    }

    #[tokio::test]
    async fn mind_provenance_add_rejects_missing_snippet() {
        let (is_err, text) = provenance_call(json!({
            "origin_url": "https://github.com/a/b",
            "search_tool_used": "ripgrep",
        }))
        .await;
        assert!(is_err);
        assert!(text.contains("snippet"), "{text}");
    }

    #[tokio::test]
    async fn mind_provenance_add_rejects_missing_origin_url() {
        let (is_err, text) = provenance_call(json!({
            "snippet": "fn x() {}",
            "search_tool_used": "ripgrep",
        }))
        .await;
        assert!(is_err);
        assert!(text.contains("origin_url"), "{text}");
    }

    #[tokio::test]
    async fn mind_provenance_add_rejects_missing_search_tool_used() {
        let (is_err, text) = provenance_call(json!({
            "snippet": "fn x() {}",
            "origin_url": "https://github.com/a/b",
        }))
        .await;
        assert!(is_err);
        assert!(text.contains("search_tool_used"), "{text}");
    }

    #[tokio::test]
    async fn mind_provenance_add_rejects_empty_search_tool_used() {
        // An empty (but present) field exercises the validator, which yields
        // the specific "use mind_add instead" guidance string.
        let (is_err, text) = provenance_call(json!({
            "snippet": "fn x() {}",
            "origin_url": "https://github.com/a/b",
            "search_tool_used": "",
        }))
        .await;
        assert!(is_err);
        assert!(
            text.contains("provenance source unknown") && text.contains("mind_add"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn mind_provenance_add_rejects_http_url() {
        let (is_err, text) = provenance_call(json!({
            "snippet": "fn x() {}",
            "origin_url": "http://github.com/a/b",
            "search_tool_used": "ripgrep",
        }))
        .await;
        assert!(is_err);
        assert!(text.contains("https"), "{text}");
    }

    #[tokio::test]
    async fn mind_provenance_add_rejects_off_allowlist_host() {
        let (is_err, text) = provenance_call(json!({
            "snippet": "fn x() {}",
            "origin_url": "https://evil.example.com/a/b",
            "search_tool_used": "ripgrep",
        }))
        .await;
        assert!(is_err);
        assert!(text.contains("allowlist"), "{text}");
    }

    #[tokio::test]
    async fn mind_provenance_add_rejects_path_traversal_in_file() {
        let (is_err, text) = provenance_call(json!({
            "snippet": "fn x() {}",
            "origin_url": "https://github.com/a/b",
            "search_tool_used": "ripgrep",
            "file": "../../etc/passwd",
        }))
        .await;
        assert!(is_err);
        assert!(text.contains(".."), "{text}");
    }

    #[tokio::test]
    async fn mind_provenance_add_rejects_mark_tags_in_snippet() {
        let (is_err, text) = provenance_call(json!({
            "snippet": "fn foo<mark>bar</mark>()",
            "origin_url": "https://github.com/a/b",
            "search_tool_used": "ripgrep",
        }))
        .await;
        assert!(is_err);
        assert!(
            text.contains("plain UTF-8") || text.contains("<mark>"),
            "{text}"
        );
    }

    /// A panic inside a tool handler must NOT escape the read loop (single-process
    /// risk): `tools/call` catches it and returns a normal JSON-RPC success
    /// envelope carrying an `isError` tool result, so the session survives.
    #[tokio::test]
    async fn tool_handler_panic_is_isolated_as_iserror() {
        // Silence the default panic hook so the caught panic doesn't print a scary
        // backtrace during this (intentional) test.
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));

        let msg = json!({
            "jsonrpc": "2.0",
            "id": 99,
            "method": "tools/call",
            "params": { "name": "mind_test_panic", "arguments": {} }
        });
        let resp = handle_message(None, msg)
            .await
            .expect("a request always gets a response");

        std::panic::set_hook(prev);

        // It is a `result` (not a protocol `error`), and the result is an isError
        // tool message that surfaces the panic - exactly what keeps the client
        // from concluding the server itself broke.
        assert!(resp.get("error").is_none(), "must not be a protocol error");
        let result = &resp["result"];
        assert_eq!(result["isError"], json!(true));
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("panic"), "should surface the panic: {text}");
        assert!(
            text.contains("boom (test panic)"),
            "should include the panic message: {text}"
        );
    }
}
