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
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
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
            let n = storage::add_memory(config, &library, &content, source.as_deref()).await?;
            format!("Added {n} chunk(s) to '{library}'")
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

    // Lock the socket to the owner (0600) so another local user can't read or
    // write the whole memory through it.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }

    // Warm the model once up front so the first real request is already fast.
    // A search loads the ONNX session + tokenizer; failures here are non-fatal
    // (e.g. no collections yet) - the point is just to trigger the load.
    eprintln!("mgimind daemon: warming embedding model...");
    let _ = storage::search(&config, "warmup", None, 1, 1).await;
    eprintln!("mgimind daemon: ready, listening on {}", path.display());

    loop {
        // A transient accept error must not kill the daemon.
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
            Err(e) => {
                eprintln!("mgimind daemon: accept error: {e}");
                continue;
            }
        };
        let cfg = config.clone();
        tokio::spawn(async move {
            let (read_half, mut write_half) = stream.into_split();
            // Cap bytes per connection so a malformed/huge request can't OOM the
            // daemon. The MCP client opens one connection per request, so 1 MiB is
            // ample for a real request.
            let mut lines = BufReader::new(read_half.take(1 << 20)).lines();
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

#[cfg(test)]
mod tests {
    use super::Request;

    #[test]
    fn parses_search_with_defaults() {
        let r: Request = serde_json::from_str(r#"{"cmd":"search","query":"hi"}"#).unwrap();
        match r {
            Request::Search {
                query, limit, tier, ..
            } => {
                assert_eq!(query, "hi");
                assert_eq!(limit, 5);
                assert_eq!(tier, 2);
            }
            _ => panic!("expected Search"),
        }
    }

    #[test]
    fn parses_add_and_ping_and_factadd() {
        assert!(matches!(
            serde_json::from_str::<Request>(r#"{"cmd":"add","library":"l","content":"c"}"#)
                .unwrap(),
            Request::Add { .. }
        ));
        assert!(matches!(
            serde_json::from_str::<Request>(r#"{"cmd":"ping"}"#).unwrap(),
            Request::Ping
        ));
        assert!(matches!(
            serde_json::from_str::<Request>(
                r#"{"cmd":"fact_add","subject":"s","predicate":"p","object":"o"}"#
            )
            .unwrap(),
            Request::FactAdd { .. }
        ));
    }

    #[test]
    fn rejects_unknown_and_malformed() {
        assert!(serde_json::from_str::<Request>(r#"{"cmd":"nope"}"#).is_err());
        assert!(serde_json::from_str::<Request>(r#"{"query":"no cmd"}"#).is_err());
        assert!(serde_json::from_str::<Request>(r#"not json"#).is_err());
    }
}
