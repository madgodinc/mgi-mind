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

    // v1.4 Phase 3 step 3: spawn the background doubt-window re-test
    // loop. Three hard guarantees: yields when an MCP call is in
    // flight, caps per-tick scan, adaptive cadence by edit rate.
    // The loop runs for the lifetime of the warm process; dropping
    // the JoinHandle at process exit is fine.
    if let Some(cfg) = config.clone() {
        let _handle = crate::doubt::spawn_background_retest_loop(cfg);
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

    // v1.4 Phase 5 post-critic fix: shut down the llama-server subprocess
    // explicitly. The static OnceCell holding the handle is never Dropped
    // on normal exit, so the child llama-server would leak (~2 GB RAM and
    // a port). Calling shutdown_server here covers the EOF / clean-stdin
    // case; SIGTERM/SIGKILL paths are covered by PR_SET_PDEATHSIG set on
    // the child at spawn time.
    #[cfg(feature = "extractor")]
    crate::extractor::shutdown_server();
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

/// Operating policy injected into the client at MCP `initialize`. The protocol
/// cannot force a tool call, so this is the strongest lever available: a
/// default-ON posture (verify before acting, capture before moving on) instead
/// of the old opt-in "consider searching" wording, which gave the model an out
/// on every turn and left the store unused.
const RETRIEVAL_INSTRUCTIONS: &str = "\
mgi-mind is the user's persistent memory and source of truth across sessions. \
Treat your own recollection as a draft to verify against it.\n\
\n\
BEFORE acting or making any factual claim about the user's projects, people, \
environment, preferences, or past decisions: call mind_search (or \
mind_recall_all) first. Assume your context is stale. Skip the lookup for \
general knowledge, arithmetic, and turns that work only on content the user \
supplied in this conversation (formatting, summarizing their pasted text, \
explaining a snippet they showed you).\n\
\n\
ALWAYS search first on: a named project/person/tool; meta-cues (\"did I tell \
you\", \"do you remember\", \"you should know\"); a negation to verify (\"isn't \
it X?\"); a reference to prior work (\"like last time\", \"the file we were \
editing\"); anything the user states as a fixed preference or decision.\n\
\n\
AFTER resolving something worth keeping (a decision, a fact, an error->fix): \
capture it with mind_add or mind_learn before moving on — uncaptured context is \
lost next session.\n\
\n\
Call mind_context once at session start for recent state and the library list. \
Unsure whether a turn needs a lookup? call mind_should_search first.";

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

/// Build a `MemoryFilter` from the shared query-arg shape used by `mind_search`
/// (MCP) and `/memory/search` + `/memory/recall` (HTTP). Accepts a single
/// `library` or a `libraries` array (OR), plus optional `author`, `source`,
/// `created_since`, `created_before`. Every field is optional; an all-absent
/// args object yields the empty filter (same as the old library-only path).
pub(crate) fn memory_filter_from_args(args: &Value) -> crate::storage::MemoryFilter {
    let libraries = match args.get("libraries").and_then(Value::as_array) {
        Some(arr) => arr
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        None => arg_str(args, "library")
            .map(|l| vec![l.to_string()])
            .unwrap_or_default(),
    };
    crate::storage::MemoryFilter {
        libraries,
        author: arg_str(args, "author").map(str::to_string),
        source: arg_str(args, "source").map(str::to_string),
        created_since: arg_str(args, "created_since").map(str::to_string),
        created_before: arg_str(args, "created_before").map(str::to_string),
    }
}

/// Dispatch `tools/call`. Always returns a tool-result Value (never a protocol
/// error) - failures become `isError: true` text.
async fn call_tool(config: Option<&MindConfig>, params: Value) -> Value {
    // v1.4 Phase 3 step 3 guarantee (a): the background re-test loop
    // sees this flag and yields rather than contend with a live tool
    // call. Dropped at end of scope (including via panic), so a
    // misbehaving tool cannot leave the flag stuck.
    let _busy_guard = crate::doubt::BusyGuard::new();

    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return tool_text("tools/call: missing tool name", true);
    };
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    let result = match dispatch(config, name, &args).await {
        Ok(text) => tool_text(text, false),
        Err(e) => tool_text(format!("Error: {e}"), true),
    };

    // v0.13 liveness: stamp a heartbeat for every active session so an
    // interrupted MCP client (Ctrl-C / kill / crash) can be detected on the
    // next mind_session_start. Best-effort — heartbeat failures must never
    // sabotage a successful tool call.
    crate::session::touch_all_active();

    result
}

/// Map a tool name + arguments to its rendered text. All 30 tools are wired:
/// - 7 "warm" embed-path tools reuse the existing `render_*`/`build_*` helpers
///   + storage/knowledge functions, using the pre-loaded warm config;
/// - 11 tools call text-returning `crate::cli::run_*` cores (download/doctor
///   progress goes to stderr so stdout stays pure JSON-RPC);
/// - 3 vault tools return static instruction text - secrets never flow over MCP.
///
/// Shared by the MCP stdio loop and the loopback HTTP API in http_api.rs; it is
/// stdio-independent (config is a parameter, the result is a value), so an axum
/// handler can call it directly.
pub async fn dispatch(config: Option<&MindConfig>, name: &str, args: &Value) -> Result<String> {
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
            let mfilter = memory_filter_from_args(args);
            let limit = arg_u64(args, "limit", 5) as usize;
            let tier = arg_u64(args, "tier", 2) as u8;
            let results =
                crate::storage::search_filtered(cfg, query, &mfilter, limit, tier).await?;
            Ok(crate::cli::render_search(&results))
        }
        "mind_recall_all" => {
            // Unified recall across all three silos in one call: facts (current
            // first), memories, and procedures. A lean fusion over the existing
            // per-silo queries — no cross-silo coordinator/link-layer needed.
            // This is what /memory/recall over HTTP wants instead of aliasing
            // plain search.
            let cfg = warm(true)?;
            let query = arg_str(args, "query")
                .ok_or_else(|| anyhow::anyhow!("missing required argument 'query'"))?;
            let limit = arg_u64(args, "limit", 5) as usize;
            let mut out = String::new();

            // Facts (current ones first; invalid ones are excluded by the query).
            if let Ok(facts) = crate::knowledge::query_facts(cfg, query).await {
                let current: Vec<_> = facts.iter().filter(|f| f.valid).take(limit).collect();
                if !current.is_empty() {
                    out.push_str("## Facts (current)\n");
                    for f in current {
                        out.push_str(&format!("- {} {} {}\n", f.subject, f.predicate, f.object));
                    }
                    out.push('\n');
                }
            }

            // Memories (semantic).
            if let Ok(mems) = crate::storage::search(cfg, query, None, limit, 2).await
                && !mems.is_empty()
            {
                out.push_str("## Memories\n");
                out.push_str(&crate::cli::render_search(&mems));
                out.push('\n');
            }

            // Procedures (error->fix lessons matching by context).
            if let Ok(procs) = crate::procedure::recall(cfg, None, Some(query), limit).await {
                let trimmed = procs.trim();
                if !trimmed.is_empty() && !trimmed.to_lowercase().starts_with("no ") {
                    out.push_str("## Procedures\n");
                    out.push_str(trimmed);
                    out.push('\n');
                }
            }

            if out.is_empty() {
                out.push_str("(nothing found across memories, facts, or procedures)");
            }
            Ok(out)
        }
        "mind_should_search" => {
            // Live, query-aware search-before-answer advice (the runtime half of
            // the policy that AI_INSTRUCTIONS documents and bench_policy scores
            // offline). Advisory only — MCP cannot force a search.
            let cfg = warm(true)?;
            let query = arg_str(args, "query")
                .ok_or_else(|| anyhow::anyhow!("missing required argument 'query'"))?;
            // Known project/library names are the strongest P1 signal; pull them
            // from the registered libraries (no separate registry — audit #18).
            let libs: Vec<String> = crate::storage::list_libraries(cfg)
                .await
                .unwrap_or_default()
                .into_iter()
                .filter(|l| !l.starts_with('_'))
                .collect();
            let advice = crate::retrieval_policy::classify(query, &libs);
            Ok(crate::retrieval_policy::render(&advice))
        }
        "mind_add" => {
            let cfg = warm(true)?;
            let library = arg_str(args, "library")
                .ok_or_else(|| anyhow::anyhow!("missing required argument 'library'"))?;
            let content = arg_str(args, "content")
                .ok_or_else(|| anyhow::anyhow!("missing required argument 'content'"))?;
            let source = arg_str(args, "source");
            // `agent` is the optional author tag (set by the multi-agent HTTP
            // surface). Absent on the MCP stdio path → unattributed write.
            let author = arg_str(args, "agent");
            let n =
                crate::storage::add_memory_authored(cfg, library, content, source, author).await?;
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
            let author = arg_str(args, "agent");
            let report =
                crate::ingest::run_ingest_authored(cfg, raw, candidates, library, author).await?;
            Ok(report.render())
        }
        "mind_history" => {
            let cfg = warm(true)?;
            let limit = arg_u64(args, "limit", 10) as usize;
            let results = crate::storage::history(cfg, limit).await?;
            Ok(crate::cli::render_history(&results))
        }
        "mind_by_agent" => {
            // What did one agent contribute (multi-agent visibility). Reads the
            // indexed `author` keyword field — no semantic query needed.
            let cfg = warm(true)?;
            let agent = arg_str(args, "agent")
                .ok_or_else(|| anyhow::anyhow!("missing required argument 'agent'"))?;
            let limit = arg_u64(args, "limit", 20) as usize;
            let results = crate::storage::by_author(cfg, agent, limit).await?;
            Ok(crate::cli::render_search(&results))
        }
        "mind_quarantine_list" => {
            let cfg = warm(true)?;
            let library = arg_str(args, "library");
            let limit = arg_u64(args, "limit", 20) as usize;
            let entries = crate::storage::quarantine_list(cfg, library, limit).await?;
            Ok(crate::cli::render_quarantine_list(&entries))
        }
        "mind_quarantine_show" => {
            let cfg = warm(true)?;
            let id = arg_str(args, "id")
                .ok_or_else(|| anyhow::anyhow!("missing required argument 'id'"))?;
            match crate::storage::quarantine_get(cfg, id).await? {
                Some(e) => Ok(crate::cli::render_quarantine_entry(&e)),
                None => Ok(format!(
                    "No quarantined entry with id '{id}' (may be a regular memory or unknown id)."
                )),
            }
        }
        "mind_consolidate" => {
            // MCP surface is dry-run only. Destructive consolidate (apply=true)
            // stays on the CLI where the user has to type the flag explicitly.
            // The same posture as /api/consolidate in the viewer — preview
            // surface, not action surface.
            let cfg = warm(true)?;
            let library = arg_str(args, "library").map(|s| s.to_string());
            let opts = crate::consolidate::Options {
                apply: false,
                library,
                near_dup_threshold: 0.0, // with_defaults() fills to 0.97
                decay_days: 0,           // with_defaults() fills to 180
                prune_cold: false,
            }
            .with_defaults();
            let r = crate::consolidate::run(cfg, opts).await?;
            Ok(format!(
                "Consolidate dry-run:\n  scanned:           {}\n  exact duplicates:  {}\n  near duplicates:   {}\n  cold candidates:   {}\n(run `mgimind consolidate --apply` in a terminal to act on this)",
                r.scanned, r.exact_dups_removed, r.near_dups_removed, r.cold_candidates
            ))
        }
        "mind_quarantine_promote" => {
            let cfg = warm(true)?;
            let id = arg_str(args, "id")
                .ok_or_else(|| anyhow::anyhow!("missing required argument 'id'"))?;
            if crate::storage::promote_from_quarantine(cfg, id).await? {
                Ok(format!(
                    "Promoted '{id}' from quarantine to ordinary memory."
                ))
            } else {
                Ok(format!("Nothing to promote — '{id}' is not in quarantine."))
            }
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
            let author = arg_str(args, "agent");
            let id = crate::knowledge::add_fact_authored(cfg, subject, predicate, object, author)
                .await?;
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
            // Manual outcome — no deterministic signal, so don't verify.
            crate::procedure::outcome(cfg, id, worked, false).await
        }
        "mind_outcome" => {
            // v1.5 Phase 7 step 7.1: generalised external-signal API.
            // Differs from mind_procedure_outcome in three ways: works on
            // any memory_id (not only procedures); accepts a typed signal
            // (test_passed / code_compiled / user_confirmed / cited_by);
            // is idempotent on (memory_id, signal_type, source).
            let cfg = warm(true)?;
            let memory_id = arg_str(args, "memory_id")
                .ok_or_else(|| anyhow::anyhow!("missing required argument 'memory_id'"))?;
            let signal_type_str = arg_str(args, "signal_type")
                .ok_or_else(|| anyhow::anyhow!("missing required argument 'signal_type'"))?;
            let signal_type =
                crate::outcome::OutcomeSignal::parse(signal_type_str).ok_or_else(|| {
                    anyhow::anyhow!(
                        "unknown signal_type '{signal_type_str}' — expected one of: \
                         test_passed, code_compiled, user_confirmed, cited_by"
                    )
                })?;
            let success = arg_bool(args, "success", true);
            // Source defaults to a literal "unspecified" so the
            // (signal_type, source) dedup key is well-defined even when
            // callers omit it. Callers SHOULD pass a meaningful source.
            let source = arg_str(args, "source").unwrap_or("unspecified").to_string();
            let signal = crate::outcome::ExternalSignal {
                signal_type,
                success,
                source,
                ts: chrono::Utc::now().to_rfc3339(),
            };
            let (mut out, signal_was_new) =
                crate::outcome::record_with_novelty(cfg, memory_id, signal).await?;
            // Bridge: a test_passed/code_compiled signal on a PROCEDURE is the
            // "the fix actually worked" signal the procedural layer needs — so
            // also bump its success/fail counters (→ `verified` on a real
            // success). Without this bridge, mind_outcome only logged an external
            // signal and procedures stayed unverified. Explicit-signal path (no
            // session-window correlation, the caller names the procedure id).
            // Only bump on a NEW signal — re-posting the same (type, source)
            // dedups the log, so the counter must not inflate on redelivery.
            let is_proc_signal = matches!(
                signal_type,
                crate::outcome::OutcomeSignal::TestPassed
                    | crate::outcome::OutcomeSignal::CodeCompiled
            );
            if signal_was_new
                && is_proc_signal
                && crate::storage::is_procedure(cfg, memory_id).await?
            {
                // Deterministic signal → a success both counts and verifies.
                let note = crate::procedure::outcome(cfg, memory_id, success, success).await?;
                out.push('\n');
                out.push_str(&note);
            }
            Ok(out)
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
        "mind_visualize" => {
            // Open the 3D memory visualization (spawns the viewer detached and
            // returns the URL). Use when the user asks to see/show their memory
            // or "the brain".
            crate::cli::run_visualize(true).await
        }
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

        // ---- v1.1.0 consolidated tools (alias-phase, deprecated siblings
        // removed in v2.0). Each tool dispatches by `action` to the existing
        // handlers so behavior is bit-for-bit identical to the old surface. ----
        "mind_quarantine" => {
            let action = arg_str(args, "action")
                .ok_or_else(|| anyhow::anyhow!("missing required 'action' (list|show|promote)"))?;
            match action {
                "list" => {
                    let cfg = warm(true)?;
                    let library = arg_str(args, "library");
                    let limit = arg_u64(args, "limit", 20) as usize;
                    let entries = crate::storage::quarantine_list(cfg, library, limit).await?;
                    Ok(crate::cli::render_quarantine_list(&entries))
                }
                "show" => {
                    let cfg = warm(true)?;
                    let id = arg_str(args, "id")
                        .ok_or_else(|| anyhow::anyhow!("action=show requires 'id'"))?;
                    match crate::storage::quarantine_get(cfg, id).await? {
                        Some(e) => Ok(crate::cli::render_quarantine_entry(&e)),
                        None => Ok(format!(
                            "No quarantined entry with id '{id}' (may be a regular memory or unknown id)."
                        )),
                    }
                }
                "promote" => {
                    let cfg = warm(true)?;
                    let id = arg_str(args, "id")
                        .ok_or_else(|| anyhow::anyhow!("action=promote requires 'id'"))?;
                    if crate::storage::promote_from_quarantine(cfg, id).await? {
                        Ok(format!(
                            "Promoted '{id}' from quarantine to ordinary memory."
                        ))
                    } else {
                        Ok(format!("Nothing to promote — '{id}' is not in quarantine."))
                    }
                }
                "expire" => {
                    let cfg = warm(true)?;
                    let id = arg_str(args, "id")
                        .ok_or_else(|| anyhow::anyhow!("action=expire requires 'id'"))?;
                    if crate::storage::expire_from_quarantine(cfg, id).await? {
                        Ok(format!(
                            "Expired '{id}' — confirmed the gate was right to reject it. \
                             Removed from quarantine (content + reason recorded in the audit \
                             log first, when audit is enabled)."
                        ))
                    } else {
                        Ok(format!(
                            "Nothing to expire — '{id}' is not in quarantine (live memory is \
                             never touched by this action; use mind_delete for that)."
                        ))
                    }
                }
                other => anyhow::bail!(
                    "mind_quarantine: unknown action '{other}' (expected list|show|promote|expire)"
                ),
            }
        }
        "mind_vault" => {
            let action = arg_str(args, "action")
                .ok_or_else(|| anyhow::anyhow!("missing required 'action' (get|store|list)"))?;
            match action {
                "store" => {
                    let key = arg_str(args, "key").unwrap_or("<key>");
                    Ok(format!(
                        "For security, secret values are never accepted over this channel.\n\
                         Store \"{key}\" yourself in a terminal:\n\n\
                         \x20\x20\x20\x20mgimind vault store {key} <value> --category <password|ssh|api-key|token>\n\n\
                         You'll be prompted for the master password (hidden)."
                    ))
                }
                "get" => {
                    let key = arg_str(args, "key").unwrap_or("<key>");
                    Ok(format!(
                        "For security, secrets are never returned over this channel.\n\
                         Retrieve \"{key}\" yourself in a terminal:\n\n\
                         \x20\x20\x20\x20mgimind vault get {key}\n\n\
                         You'll be prompted for the master password (hidden) and a confirmation."
                    ))
                }
                "list" => Ok("For security, the vault requires a terminal.\n\
                    List your stored keys yourself:\n\n\
                    \x20\x20\x20\x20mgimind vault list\n\n\
                    You'll be prompted for the master password (hidden)."
                    .to_string()),
                other => {
                    anyhow::bail!("mind_vault: unknown action '{other}' (expected get|store|list)")
                }
            }
        }
        "mind_session" => {
            let action = arg_str(args, "action")
                .ok_or_else(|| anyhow::anyhow!("missing required 'action' (start|end|last)"))?;
            match action {
                "start" => {
                    let agent = arg_str(args, "agent").unwrap_or("unknown");
                    crate::cli::run_session_start(agent).await
                }
                "last" => crate::cli::run_session_last(arg_str(args, "agent")).await,
                "end" => {
                    let agent = arg_str(args, "agent").unwrap_or("unknown");
                    let summary = arg_str(args, "summary")
                        .ok_or_else(|| anyhow::anyhow!("action=end requires 'summary'"))?;
                    crate::cli::run_session_end(agent, summary).await
                }
                other => anyhow::bail!(
                    "mind_session: unknown action '{other}' (expected start|end|last)"
                ),
            }
        }
        "mind_fact" => {
            let action = arg_str(args, "action").ok_or_else(|| {
                anyhow::anyhow!("missing required 'action' (add|query|invalidate)")
            })?;
            match action {
                "add" => {
                    let cfg = warm(true)?;
                    let subject = arg_str(args, "subject")
                        .ok_or_else(|| anyhow::anyhow!("action=add requires 'subject'"))?;
                    let predicate = arg_str(args, "predicate")
                        .ok_or_else(|| anyhow::anyhow!("action=add requires 'predicate'"))?;
                    let object = arg_str(args, "object")
                        .ok_or_else(|| anyhow::anyhow!("action=add requires 'object'"))?;
                    let id = crate::knowledge::add_fact(cfg, subject, predicate, object).await?;
                    Ok(format!(
                        "Fact added: {subject} -> {predicate} -> {object} [id: {id}]"
                    ))
                }
                "query" => {
                    let cfg = warm(true)?;
                    let subject = arg_str(args, "subject")
                        .ok_or_else(|| anyhow::anyhow!("action=query requires 'subject'"))?;
                    let facts = crate::knowledge::query_facts(cfg, subject).await?;
                    Ok(crate::cli::render_facts(subject, &facts))
                }
                "invalidate" => {
                    let id = arg_str(args, "id")
                        .ok_or_else(|| anyhow::anyhow!("action=invalidate requires 'id'"))?;
                    crate::cli::run_fact_invalidate(id).await
                }
                other => anyhow::bail!(
                    "mind_fact: unknown action '{other}' (expected add|query|invalidate)"
                ),
            }
        }
        "mind_library" => {
            let action = arg_str(args, "action")
                .ok_or_else(|| anyhow::anyhow!("missing required 'action' (create|delete|list)"))?;
            match action {
                "create" => {
                    let name = arg_str(args, "name")
                        .ok_or_else(|| anyhow::anyhow!("action=create requires 'name'"))?;
                    crate::cli::run_create(name).await
                }
                "list" => crate::cli::run_list().await,
                "delete" => {
                    let library = arg_str(args, "library")
                        .ok_or_else(|| anyhow::anyhow!("action=delete requires 'library'"))?;
                    let id = arg_str(args, "id")
                        .ok_or_else(|| anyhow::anyhow!("action=delete requires 'id'"))?;
                    crate::cli::run_delete(library, id).await
                }
                other => anyhow::bail!(
                    "mind_library: unknown action '{other}' (expected create|delete|list)"
                ),
            }
        }

        // v1.4 Phase 0 step 3: predicate cardinality registry.
        // Configures whether two distinct objects for the same
        // (subject, predicate) pair count as a conflict (Single,
        // TemporalSingle) or coexist (Multi). Default for unregistered
        // predicates is Multi.
        "mind_predicate" => {
            let action = arg_str(args, "action")
                .ok_or_else(|| anyhow::anyhow!("missing required 'action' (register|list|get)"))?;
            match action {
                "register" => {
                    let cfg = warm(true)?;
                    let predicate = arg_str(args, "predicate")
                        .ok_or_else(|| anyhow::anyhow!("action=register requires 'predicate'"))?;
                    let cardinality_str = arg_str(args, "cardinality").ok_or_else(|| {
                        anyhow::anyhow!(
                            "action=register requires 'cardinality' (single|temporal-single|multi)"
                        )
                    })?;
                    let cardinality = crate::knowledge::Cardinality::parse(cardinality_str)
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "unknown cardinality '{cardinality_str}' (expected single|temporal-single|multi)"
                            )
                        })?;
                    crate::knowledge::register_cardinality(cfg, predicate, cardinality).await?;
                    Ok(format!("Registered '{predicate}' as {cardinality:?}."))
                }
                "get" => {
                    let cfg = warm(true)?;
                    let predicate = arg_str(args, "predicate")
                        .ok_or_else(|| anyhow::anyhow!("action=get requires 'predicate'"))?;
                    let c = crate::knowledge::get_cardinality(cfg, predicate).await?;
                    Ok(format!("{predicate} -> {c:?}"))
                }
                "list" => {
                    let cfg = warm(true)?;
                    let entries = crate::knowledge::list_cardinalities(cfg).await?;
                    if entries.is_empty() {
                        Ok("No predicates registered. Default for any unregistered predicate is Multi.".to_string())
                    } else {
                        let mut out = format!("{} registered predicate(s):\n", entries.len());
                        for (p, c) in entries {
                            out.push_str(&format!("  {p} -> {c:?}\n"));
                        }
                        Ok(out)
                    }
                }
                other => anyhow::bail!(
                    "mind_predicate: unknown action '{other}' (expected register|list|get)"
                ),
            }
        }

        // Test-only handler that panics, so a test can prove the panic-isolation
        // wrapper in `tools/call` turns a handler panic into an `isError` result
        // instead of unwinding through the read loop and killing the session.
        #[cfg(test)]
        "mind_test_panic" => panic!("boom (test panic)"),

        other => Err(anyhow::anyhow!("unknown tool: {other}")),
    }
}

/// The 30 tool definitions advertised by `tools/list`. Schemas are hand-written
/// from the zod schemas in `mcp-server/index.js` (1:1, so signatures don't
/// drift). `inputSchema` is a JSON Schema object per tool.
fn tool_definitions() -> Vec<Value> {
    // List order matters: well-behaved MCP clients render tools in the order
    // returned by tools/list, and many model prompts surface the first dozen
    // first. So:
    //   (1) the consolidated v1.1 tools come right after the everyday verbs
    //       (search/add/provenance/context),
    //   (2) the deprecated singletons keep working but live at the end of the
    //       list, with `"deprecated": true` so well-behaved clients can hide
    //       them, and their description prefixed with the v2.0 death-date.
    // The dispatch table above still handles every old name, so any client
    // that already wires the old tools by name keeps working unchanged through
    // the v1.x line.
    let tools = vec![
        json!({
            "name": "mind_search",
            "description": "Semantic search across memories, with optional metadata filters (author, source, date window, multiple libraries).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search query" },
                    "library": { "type": "string", "description": "Filter to one library" },
                    "libraries": { "type": "array", "items": { "type": "string" }, "description": "Filter to ANY of these libraries (OR). Use instead of 'library' for cross-library search." },
                    "author": { "type": "string", "description": "Filter to memories written by this agent" },
                    "source": { "type": "string", "description": "Filter by ingest source tag (e.g. a session id or URL)" },
                    "created_since": { "type": "string", "description": "Only memories created at/after this instant, INCLUSIVE (RFC3339 timestamp or YYYY-MM-DD date)" },
                    "created_before": { "type": "string", "description": "Only memories created before this instant, EXCLUSIVE (RFC3339 timestamp or YYYY-MM-DD date)" },
                    "limit": { "type": "number", "default": 5, "description": "Max results" },
                    "tier": { "type": "number", "default": 2, "description": "Retrieval tier: 1=facts, 2=summaries, 3=full" }
                },
                "required": ["query"]
            }
        }),
        json!({
            "name": "mind_recall_all",
            "description": "Unified recall across all memory types at once (facts + memories + procedures) for one query. Current facts first, then semantic memories, then matching error->fix procedures. One call instead of three separate searches.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "What to recall" },
                    "limit": { "type": "number", "default": 5, "description": "Max per silo" }
                },
                "required": ["query"]
            }
        }),
        json!({
            "name": "mind_should_search",
            "description": "Decide whether to search memory BEFORE answering a user query. Returns priority (must-search / should-search / answer-directly), the reason, and which libraries to search first. Call this on a turn when unsure; it implements the search-before-answer trigger policy (named project, meta-cue like 'did I tell you', negation to verify, cross-session reference). Advisory — it cannot force a search.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "The user's query to classify" }
                },
                "required": ["query"]
            }
        }),
        json!({
            "name": "mind_visualize",
            "description": "Open the 3D memory visualization in the user's browser — the brain as glowing cores (memories, facts, regions) wired by neurons, with live pulses. Call this when the user asks to SEE or SHOW their memory / 'the brain' / how memory looks. Spawns a local viewer and returns the URL.",
            "inputSchema": { "type": "object", "properties": {} }
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
            "description": "Recall error->fix playbooks for an error and/or a task context. Ranking blends relevance with trust: a verified or proven fix gets a boost, a repeatedly-failing one is demoted, but a strongly-matching unverified fix still surfaces.",
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
            "name": "mind_outcome",
            "description": "v1.5: Record a typed external-signal outcome on any memory (not only procedures). Use this when a test passed/failed, code compiled, a user explicitly confirmed/denied a fact, or a citing memory referenced this one. Idempotent on (memory_id, signal_type, source) — re-posting the same triple updates the existing entry rather than appending a duplicate. Signals contribute to the fact's external_signal_score, which feeds the duel rule and the §3 Mechanism 2 doubt-window guardrail. When memory_id is a PROCEDURE and signal_type is test_passed or code_compiled, this also bumps the procedure's success/fail counters — so a green test after a mind_learn fix marks that playbook verified without a separate mind_procedure_outcome call. Pass the procedure id returned by mind_learn.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "memory_id": { "type": "string", "description": "Target memory id (from mind_search or any tool returning ids)" },
                    "signal_type": {
                        "type": "string",
                        "enum": ["test_passed", "code_compiled", "user_confirmed", "cited_by"],
                        "description": "What kind of signal this is. test_passed is the strongest (weight 1.0); cited_by the weakest (0.2)."
                    },
                    "success": { "type": "boolean", "default": true, "description": "Did the signal positively confirm the fact (true) or negate it (false)? A failed test_passed (success=false) pulls the score negative." },
                    "source": { "type": "string", "description": "Stable identifier of where the signal came from (e.g. 'ci.github.com/run/12345', 'user-mad'). Used for idempotency." }
                },
                "required": ["memory_id", "signal_type"]
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
            "name": "mind_consolidate",
            "description": "Preview what `mgimind consolidate` would do — count of exact duplicates, near-duplicates, and cold (old + unused) entries. Always dry-run on the MCP surface; destructive consolidation stays on the CLI where the user types --apply explicitly. Use this when the user asks 'how much duplicate memory do I have?' or before suggesting they run the CLI command.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "library": { "type": "string", "description": "Scope to one library (omit for all)" }
                }
            }
        }),
        json!({
            "name": "mind_quarantine_list",
            "description": "List entries the v0.11 relevance gate filtered into the quarantine layer. These are not surfaced by mind_search by design — use this tool when you suspect a fact was filtered (e.g., the user keeps repeating something the gate would reject as low-signal). Newest first.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "library": { "type": "string", "description": "Scope to one library (omit for all)" },
                    "limit": { "type": "number", "default": 20 }
                }
            }
        }),
        json!({
            "name": "mind_quarantine_show",
            "description": "Show a single quarantined entry with its full content and the gate reason. Returns 'not in quarantine' for regular memory ids — the surface is honest about what it can see.",
            "inputSchema": {
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"]
            }
        }),
        json!({
            "name": "mind_quarantine_promote",
            "description": "Explicitly promote a quarantined entry to ordinary memory by id. The automatic promotion path is re-asserting the same content via mind_ingest; use this when you know the entry should be live without re-ingesting.",
            "inputSchema": {
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"]
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
    ];

    // ---- v1.1 consolidated tools (alias-phase). 5 new verbs cover what
    // used to be 13 singletons. Old singletons continue to work for the whole
    // v1.x line and are removed in v2.0.
    let consolidated = vec![
        json!({
            "name": "mind_quarantine",
            "description": "Inspect, promote, or expire entries that the relevance gate filtered into the quarantine layer. Single tool with `action`: list (newest first, optional library filter), show (full content + gate reason by id), promote (the gate was too strict — move to ordinary memory by id), expire (the gate was right — delete by id; only ever touches quarantined points, never live memory, and stays recoverable from the audit log). Replaces mind_quarantine_list / _show / _promote.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["list", "show", "promote", "expire"], "description": "What to do" },
                    "id": { "type": "string", "description": "Required for action=show, promote, or expire" },
                    "library": { "type": "string", "description": "Scope for action=list (optional)" },
                    "limit": { "type": "number", "default": 20, "description": "Max entries for action=list" }
                },
                "required": ["action"]
            }
        }),
        json!({
            "name": "mind_vault",
            "description": "Vault is terminal-only by design. This tool explains how the user runs the equivalent `mgimind vault` command — secret values never cross the MCP channel. Single tool with `action`: store (instructions for storing a secret), get (instructions for retrieving), list (instructions for listing keys). Replaces mind_vault_store / _get / _list.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["store", "get", "list"], "description": "What instructions to render" },
                    "key": { "type": "string", "description": "Optional key name to interpolate into the instructions" }
                },
                "required": ["action"]
            }
        }),
        json!({
            "name": "mind_session",
            "description": "Manage the agent's session record across runs. Single tool with `action`: start (open a new session for an agent), last (read the previous session's summary), end (close the active session with a hand-written summary). Replaces mind_session_start / _last / _end.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["start", "last", "end"], "description": "Lifecycle step" },
                    "agent": { "type": "string", "default": "unknown", "description": "Agent name; same across start/end" },
                    "summary": { "type": "string", "description": "Required for action=end. Free-form, <200 words, what happened this session" }
                },
                "required": ["action"]
            }
        }),
        json!({
            "name": "mind_fact",
            "description": "Knowledge-graph facts (subject -> predicate -> object). Single tool with `action`: add (insert a new fact), query (list facts about a subject), invalidate (soft-delete a fact by id when a single-valued fact has been superseded). Replaces mind_fact_add / _query / _invalidate.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["add", "query", "invalidate"], "description": "Operation" },
                    "subject": { "type": "string", "description": "Required for add/query" },
                    "predicate": { "type": "string", "description": "Required for add" },
                    "object": { "type": "string", "description": "Required for add" },
                    "id": { "type": "string", "description": "Required for invalidate (from action=query)" }
                },
                "required": ["action"]
            }
        }),
        json!({
            "name": "mind_library",
            "description": "Library namespaces. Single tool with `action`: create (new library by name), list (all libraries with counts), delete (remove a specific memory by id within a library — destructive). Replaces mind_create / mind_list / mind_delete.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["create", "list", "delete"], "description": "Operation" },
                    "name": { "type": "string", "description": "Required for action=create" },
                    "library": { "type": "string", "description": "Required for action=delete" },
                    "id": { "type": "string", "description": "Required for action=delete; memory uuid from search results" }
                },
                "required": ["action"]
            }
        }),
        json!({
            "name": "mind_predicate",
            "description": "v1.4: register and inspect predicate cardinality (single | temporal-single | multi). Cardinality controls how `mind_fact(action=\"add\")` detects conflicts: Multi predicates never conflict; Single/TemporalSingle flag a `conflict_pending` event when a second distinct object is asserted. Default for unregistered predicates is Multi.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["register", "get", "list"], "description": "Operation" },
                    "predicate": { "type": "string", "description": "Required for action=register and action=get" },
                    "cardinality": { "type": "string", "enum": ["single", "temporal-single", "multi"], "description": "Required for action=register" }
                },
                "required": ["action"]
            }
        }),
    ];

    // Mark the 13 singletons as deprecated and rewrite their description with
    // the v2.0 death-date and the replacement signature. Well-behaved MCP
    // clients hide tools where `deprecated: true`, but the surface still works
    // for clients that don't honor the field.
    const DEPRECATIONS: &[(&str, &str)] = &[
        ("mind_quarantine_list", "mind_quarantine(action=\"list\")"),
        ("mind_quarantine_show", "mind_quarantine(action=\"show\")"),
        (
            "mind_quarantine_promote",
            "mind_quarantine(action=\"promote\")",
        ),
        ("mind_vault_store", "mind_vault(action=\"store\")"),
        ("mind_vault_get", "mind_vault(action=\"get\")"),
        ("mind_vault_list", "mind_vault(action=\"list\")"),
        ("mind_session_start", "mind_session(action=\"start\")"),
        ("mind_session_last", "mind_session(action=\"last\")"),
        ("mind_session_end", "mind_session(action=\"end\")"),
        ("mind_fact_add", "mind_fact(action=\"add\")"),
        ("mind_fact_query", "mind_fact(action=\"query\")"),
        ("mind_fact_invalidate", "mind_fact(action=\"invalidate\")"),
        ("mind_create", "mind_library(action=\"create\")"),
        ("mind_list", "mind_library(action=\"list\")"),
        ("mind_delete", "mind_library(action=\"delete\")"),
    ];

    // Splits the list into "kept" (everyday + the new consolidated) and
    // "deprecated" (singletons), preserving their original order so a
    // backwards-compat smoke test still sees the same handler set.
    let dep_names: std::collections::HashMap<&str, &str> = DEPRECATIONS.iter().copied().collect();
    let mut kept: Vec<Value> = Vec::new();
    let mut deprecated: Vec<Value> = Vec::new();
    for tool in tools.into_iter() {
        let name = tool.get("name").and_then(|n| n.as_str()).unwrap_or("");
        if let Some(replacement) = dep_names.get(name) {
            let mut tool = tool;
            if let Some(obj) = tool.as_object_mut() {
                obj.insert("deprecated".to_string(), Value::Bool(true));
                let old_desc = obj
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string();
                let new_desc =
                    format!("DEPRECATED — use {replacement}. Removed in v2.0. ({old_desc})");
                obj.insert("description".to_string(), Value::String(new_desc));
            }
            deprecated.push(tool);
        } else {
            kept.push(tool);
        }
    }

    // Final order: everyday verbs + consolidated v1.1 tools first, deprecated
    // singletons last so a client showing only the first N has a clean view.
    kept.extend(consolidated);
    kept.extend(deprecated);
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_consolidated_and_legacy_tools() {
        // v1.1 alias phase: 30 v1.0 singletons stay live + 5 new consolidated
        // verbs (mind_quarantine/_vault/_session/_fact/_library). The 15
        // deprecated singletons are still in the list, but flagged
        // `deprecated: true`. Total v1.1 = 35. v1.4 adds mind_predicate → 36.
        // v1.5 Phase 7 adds mind_outcome → 37.
        let tools = tool_definitions();
        assert_eq!(
            tools.len(),
            40,
            "tools/list = 30 legacy + 5 v1.1 consolidated + 1 v1.4 (mind_predicate) + 1 v1.5 (mind_outcome) + 1 (mind_recall_all) + 1 (mind_should_search) + 1 (mind_visualize) = 40"
        );
        let deprecated = tools
            .iter()
            .filter(|t| t.get("deprecated").and_then(Value::as_bool) == Some(true))
            .count();
        assert_eq!(
            deprecated, 15,
            "15 singletons must be flagged deprecated for v2.0 removal"
        );
        let live_surface = tools.len() - deprecated;
        assert_eq!(
            live_surface, 25,
            "non-deprecated surface is 25 tools (20 v1.1 + 1 v1.4 mind_predicate + 1 v1.5 mind_outcome + 1 mind_recall_all + 1 mind_should_search + 1 mind_visualize)"
        );
    }

    #[test]
    fn consolidated_tools_are_present() {
        let tools = tool_definitions();
        for needed in [
            "mind_quarantine",
            "mind_vault",
            "mind_session",
            "mind_fact",
            "mind_library",
            "mind_predicate", // v1.4
        ] {
            let found = tools
                .iter()
                .any(|t| t.get("name").and_then(Value::as_str) == Some(needed));
            assert!(found, "consolidated tool {needed} missing from tools/list");
        }
    }

    #[test]
    fn deprecated_tools_point_at_replacement() {
        let tools = tool_definitions();
        // Every deprecated singleton must (a) be flagged, (b) name its
        // replacement in the description so the agent self-corrects.
        for t in &tools {
            if t.get("deprecated").and_then(Value::as_bool) != Some(true) {
                continue;
            }
            let desc = t.get("description").and_then(Value::as_str).unwrap_or("");
            assert!(
                desc.starts_with("DEPRECATED"),
                "deprecated tool {} should lead with the death notice, got: {desc}",
                t.get("name").and_then(Value::as_str).unwrap_or("?")
            );
            assert!(
                desc.contains("v2.0"),
                "deprecated tool {} must name the v2.0 removal target",
                t.get("name").and_then(Value::as_str).unwrap_or("?")
            );
        }
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
        // The MCP `instructions` field is the only programmatic channel for the
        // "verify before acting" policy. If it disappears, clients lose the only
        // signal we have — guard it, and guard the opt-out posture (search
        // before acting, not "consider searching").
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
            instr.contains("source of truth") && instr.contains("BEFORE"),
            "instructions must carry the default-on 'verify before acting' posture"
        );
    }

    #[tokio::test]
    async fn initialized_notification_has_no_reply() {
        let msg = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        assert!(handle_message(None, msg).await.is_none());
    }

    #[tokio::test]
    async fn tools_list_returns_v1_5_surface() {
        // 30 legacy v1.0 singletons (15 deprecated, alias phase) + 5
        // consolidated v1.1 verbs + 1 v1.4 (mind_predicate) +
        // 1 v1.5 (mind_outcome) + 1 (mind_recall_all) + 1 (mind_should_search)
        // + 1 (mind_visualize) = 40 total.
        // Removal of the 15 deprecated singletons is scheduled for v2.0.
        let msg = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" });
        let resp = handle_message(None, msg).await.unwrap();
        assert_eq!(resp["result"]["tools"].as_array().unwrap().len(), 40);
    }

    #[tokio::test]
    async fn consolidated_vault_dispatches_to_terminal_instructions() {
        // The vault consolidated verb must route action=store|get|list to the
        // same terminal-only instructions the singletons returned. This is the
        // "alias phase" promise — old and new tool names are bit-for-bit
        // equivalent in behavior.
        for action in ["store", "get", "list"] {
            let params = json!({
                "name": "mind_vault",
                "arguments": { "action": action, "key": "test-k" }
            });
            let res = call_tool(None, params).await;
            let text = res["content"][0]["text"].as_str().unwrap();
            assert!(
                text.contains("terminal"),
                "mind_vault action={action} must redirect to a terminal"
            );
            assert_eq!(res["isError"], false);
        }
    }

    #[tokio::test]
    async fn consolidated_tool_rejects_unknown_action() {
        // Wrong action should fail fast with a clear message naming the
        // allowed values, not silently no-op or panic.
        let params = json!({
            "name": "mind_vault",
            "arguments": { "action": "destroy" }
        });
        let res = call_tool(None, params).await;
        assert_eq!(res["isError"], true);
        let text = res["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("unknown action"));
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
