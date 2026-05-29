//! Long-lived daemon (audit #16). The MCP server otherwise spawns a fresh
//! `mgimind` process per call, so the ONNX session and tokenizer (cached in
//! `OnceCell`s) are reloaded every time - ~2-5s of model-load latency on every
//! search/add/context. This daemon loads the model once, keeps it warm, and
//! serves requests over a Unix socket; the MCP client talks to it and only
//! falls back to spawning the CLI when the socket isn't there.
//!
//! Protocol: newline-delimited JSON. One request object per line
//! (`{"cmd":"search","query":"..."}`), one response per line
//! (`{"ok":true,"data":{...}}` or `{"ok":false,"error":"..."}`).

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

use crate::config::MindConfig;
use crate::{knowledge, storage};

/// Unix socket the daemon listens on (and the MCP client connects to).
pub fn socket_path() -> std::path::PathBuf {
    crate::config::mind_home().join("daemon.sock")
}

fn default_limit() -> usize {
    5
}
fn default_tier() -> u8 {
    2
}

#[derive(Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
enum Request {
    /// Liveness probe - lets a client check the daemon without loading anything.
    Ping,
    Search {
        query: String,
        #[serde(default)]
        library: Option<String>,
        #[serde(default = "default_limit")]
        limit: usize,
        #[serde(default = "default_tier")]
        tier: u8,
    },
    Add {
        library: String,
        content: String,
        #[serde(default)]
        source: Option<String>,
    },
    Context,
    History {
        #[serde(default = "default_limit")]
        limit: usize,
    },
    FactAdd {
        subject: String,
        predicate: String,
        object: String,
    },
    FactQuery {
        subject: String,
    },
    Stats,
}

/// Each request returns `{ "text": <rendered output> }` - the same text the
/// equivalent CLI command prints (rendered via the shared `cli::render_*`
/// helpers), so the MCP client can print it verbatim and output matches the
/// CLI-spawn fallback path exactly.
async fn handle(config: &MindConfig, req: Request) -> Result<Value> {
    let text = match req {
        Request::Ping => "pong".to_string(),
        Request::Search {
            query,
            library,
            limit,
            tier,
        } => {
            let results = storage::search(config, &query, library.as_deref(), limit, tier).await?;
            crate::cli::render_search(&results)
        }
        Request::Add {
            library,
            content,
            source,
        } => {
            let id = storage::add_memory(config, &library, &content, source.as_deref()).await?;
            format!("Added to '{library}' [id: {id}]")
        }
        Request::Context => crate::cli::build_context(config).await?,
        Request::History { limit } => {
            let results = storage::history(config, limit).await?;
            crate::cli::render_history(&results)
        }
        Request::FactAdd {
            subject,
            predicate,
            object,
        } => {
            let id = knowledge::add_fact(config, &subject, &predicate, &object).await?;
            format!("Fact added: {subject} -> {predicate} -> {object} [id: {id}]")
        }
        Request::FactQuery { subject } => {
            let facts = knowledge::query_facts(config, &subject).await?;
            crate::cli::render_facts(&subject, &facts)
        }
        Request::Stats => crate::cli::build_stats(config).await?,
    };
    Ok(json!({ "text": text }))
}

/// Run the daemon: bind the socket, warm the embedder, then serve one
/// JSON request per line until the socket is closed.
pub async fn run(config: MindConfig) -> Result<()> {
    let path = socket_path();
    // Clear any stale socket from a previous (crashed) run before binding.
    let _ = std::fs::remove_file(&path);
    let listener =
        UnixListener::bind(&path).with_context(|| format!("Failed to bind {}", path.display()))?;

    // Warm the model once up front so the first real request is already fast.
    // A search loads the ONNX session + tokenizer; failures here are non-fatal
    // (e.g. no collections yet) - the point is just to trigger the load.
    eprintln!("mgimind daemon: warming embedding model...");
    let _ = storage::search(&config, "warmup", None, 1, 1).await;
    eprintln!("mgimind daemon: ready, listening on {}", path.display());

    loop {
        let (stream, _) = listener.accept().await?;
        let cfg = config.clone();
        tokio::spawn(async move {
            let (read_half, mut write_half) = stream.into_split();
            let mut lines = BufReader::new(read_half).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                let response = match serde_json::from_str::<Request>(&line) {
                    Ok(req) => match handle(&cfg, req).await {
                        Ok(data) => json!({ "ok": true, "data": data }),
                        Err(e) => json!({ "ok": false, "error": e.to_string() }),
                    },
                    Err(e) => json!({ "ok": false, "error": format!("bad request: {e}") }),
                };
                let mut out = response.to_string();
                out.push('\n');
                if write_half.write_all(out.as_bytes()).await.is_err() {
                    break;
                }
            }
        });
    }
}
