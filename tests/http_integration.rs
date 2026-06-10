//! Black-box integration test for the multi-agent HTTP surface (`serve-http`).
//! This is the exact path an external multi-agent system (e.g. a Band of agents)
//! hits: bearer auth → token-derived identity → dispatch → the SAME write gates
//! as the CLI/MCP path (Steps 1-7). The surface was correct-by-reading but had
//! zero executed tests; these lock its contract in.
//!
//! Gated on `MGIMIND_IT_QDRANT` + `MGIMIND_IT_MODELS` + `ORT_DYLIB_PATH` (a plain
//! `cargo test` without a Qdrant/model just skips). Drives the server over `curl`
//! so no HTTP client dev-dependency is needed.

use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_mgimind")
}

/// Kills the spawned server on EVERY exit path — including a panic in setup
/// (e.g. the library-create assert) before any assertion runs. A leaked server
/// would hold the fixed port and poison the next run. Drop is unconditional, so
/// it's stronger than a catch_unwind that only wraps the assertion block.
struct ServerGuard(Child);
impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// `curl -s -w "\n%{http_code}"` → returns (status_code, body). Body is whatever
/// preceded the trailing status line. `args` are extra curl args (method, headers,
/// data, url).
fn curl(args: &[&str]) -> (u16, String) {
    let out = Command::new("curl")
        .args(["-s", "-w", "\n%{http_code}"])
        .args(args)
        .output()
        .expect("spawn curl");
    let s = String::from_utf8_lossy(&out.stdout);
    let (body, code) = s.rsplit_once('\n').unwrap_or(("", "0"));
    (code.trim().parse().unwrap_or(0), body.to_string())
}

#[test]
fn http_surface_full_contract() {
    let (Ok(port), Ok(models), Ok(ort)) = (
        std::env::var("MGIMIND_IT_QDRANT"),
        std::env::var("MGIMIND_IT_MODELS"),
        std::env::var("ORT_DYLIB_PATH"),
    ) else {
        eprintln!("SKIP: set MGIMIND_IT_QDRANT, MGIMIND_IT_MODELS and ORT_DYLIB_PATH to run");
        return;
    };
    let model_src = Path::new(&models).join("multilingual-e5-base");
    if !model_src.join("model.onnx").exists() {
        eprintln!("SKIP: no multilingual-e5-base model under MGIMIND_IT_MODELS");
        return;
    }

    // Isolated MGIMIND_HOME with an e5 config + a copy of the model.
    let home = tempfile::tempdir().expect("tempdir");
    let mind = home.path().join("mgimind");
    std::fs::create_dir_all(mind.join("models")).unwrap();
    copy_dir(&model_src, &mind.join("models/multilingual-e5-base")).expect("copy model");
    let cfg = serde_json::json!({
        "version": "it",
        "data_dir": mind,
        "model_name": "multilingual-e5-base",
        "qdrant_port": port.parse::<u16>().expect("MGIMIND_IT_QDRANT must be a port"),
        "vector_size": 768,
        "pooling": "mean",
        "uses_token_type_ids": false,
        "query_prefix": "query: ",
        "passage_prefix": "passage: ",
        "rerank_enabled": false,
    });
    std::fs::write(
        mind.join("config.json"),
        serde_json::to_string(&cfg).unwrap(),
    )
    .unwrap();

    // A fixed, almost-certainly-free high port + a known per-agent token, so the
    // test never has to scrape the bound port from server output. Identity is
    // DERIVED from the token (X-Agent ignored), so "alice" is trustworthy.
    let http_port = 47193u16;
    let token = "TESTTOKEN_alice_42";
    // Guard so the child is killed on ANY exit path from here on — including a
    // panic in the setup asserts below, not just the request asserts.
    let mut server = ServerGuard(
        Command::new(bin())
            .args([
                "serve-http",
                "--port",
                &http_port.to_string(),
                "--agent-token",
            ])
            .arg(format!("alice:{token}"))
            .env("MGIMIND_HOME", &mind)
            .env("ORT_DYLIB_PATH", &ort)
            .spawn()
            .expect("spawn serve-http"),
    );

    let base = format!("http://127.0.0.1:{http_port}");
    let bearer = format!("Authorization: Bearer {token}");
    let lib = format!("ithttp_{}", std::process::id());

    // 1) Wait for /health to come up. Detect a bind failure (port busy / leaked
    //    server from a prior run) FAST via try_wait instead of polling 30s.
    let health = format!("{base}/health");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(Some(status)) = server.0.try_wait() {
            panic!(
                "serve-http exited before /health came up ({status}) — port {http_port} likely busy"
            );
        }
        let (code, _) = curl(&["-H", &bearer, &health]);
        if code == 200 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "server /health never returned 200"
        );
        std::thread::sleep(Duration::from_millis(200));
    }

    // 2) Bad bearer on an AUTHENTICATED route → 401. (/health is an
    //    unauthenticated liveness probe by design, so test auth on /memory/*.)
    let ingest = format!("{base}/memory/ingest");
    let (code, _) = curl(&[
        "-X",
        "POST",
        "-H",
        "Authorization: Bearer WRONG",
        "-H",
        "Content-Type: application/json",
        "-d",
        "{}",
        &ingest,
    ]);
    assert_eq!(
        code, 401,
        "a bad bearer token must be rejected with 401 on a protected route"
    );

    // 2b) Create the working library over HTTP — the Band makes its own library,
    //     no operator pre-step. The only structure-mutating route, non-destructive.
    let create = format!("{base}/library/create");
    let cbody = serde_json::json!({"name": lib}).to_string();
    let (code, out) = curl(&[
        "-X",
        "POST",
        "-H",
        &bearer,
        "-H",
        "Content-Type: application/json",
        "-d",
        &cbody,
        &create,
    ]);
    assert_eq!(
        code, 200,
        "library create over HTTP should succeed, got {code}: {out}"
    );

    // 2c) A reserved library name must be refused with a clean 4xx, not created —
    //     the namespace guard reaches the untrusted HTTP caller.
    let rbody = serde_json::json!({"name": "_procedures"}).to_string();
    let (code, out) = curl(&[
        "-X",
        "POST",
        "-H",
        &bearer,
        "-H",
        "Content-Type: application/json",
        "-d",
        &rbody,
        &create,
    ]);
    assert!(
        (400..500).contains(&code),
        "a reserved library name must be refused with 4xx, not {code}: {out}"
    );

    // 3) Ingest a memory as alice → 200, stored.
    let content = "The launch retrospective is scheduled for the second Tuesday of every month.";
    let body = serde_json::json!({
        "library": lib,
        "candidates": [{"type": "memory", "content": content}]
    })
    .to_string();
    let (code, out) = curl(&[
        "-X",
        "POST",
        "-H",
        &bearer,
        "-H",
        "Content-Type: application/json",
        "-d",
        &body,
        &ingest,
    ]);
    assert_eq!(code, 200, "first ingest should succeed, got {code}: {out}");
    assert!(
        out.contains("1 memory"),
        "first ingest should store 1 memory, got: {out}"
    );

    // 4) Re-ingest the SAME content → Step-7 fix proven over HTTP: it must be a
    //    re-assertion (kept live, no new write), NOT quarantined/demoted. The
    //    render string is fragile, so back it with a STRUCTURAL check: it must NOT
    //    report a quarantine, and (below) the content must still be retrievable.
    let (code, out) = curl(&[
        "-X",
        "POST",
        "-H",
        &bearer,
        "-H",
        "Content-Type: application/json",
        "-d",
        &body,
        &ingest,
    ]);
    assert_eq!(code, 200, "re-ingest should still 200, got {code}: {out}");
    let low = out.to_lowercase();
    assert!(
        low.contains("re-asserted") && !low.contains("quarantin"),
        "re-ingesting identical content must be a re-assertion, not a quarantine (Step 7), got: {out}"
    );

    // 4b) STRUCTURAL proof it stayed live: a search must still find it. If the
    //     Step-7 fix regressed and it got quarantined, search (which excludes
    //     quarantine) would return nothing — this assertion fails where the
    //     substring check might not.
    let search = format!("{base}/memory/search");
    let sbody = serde_json::json!({
        "query": "when is the launch retrospective", "library": lib, "tier": 3
    })
    .to_string();
    let (code, out) = curl(&[
        "-X",
        "POST",
        "-H",
        &bearer,
        "-H",
        "Content-Type: application/json",
        "-d",
        &sbody,
        &search,
    ]);
    assert_eq!(code, 200, "search should 200, got {code}: {out}");
    assert!(
        out.contains("launch retrospective"),
        "after re-ingest the memory must still be live and searchable, got: {out}"
    );

    // 5) /memory/by-agent (alice's own token) → her write comes back, proving the
    //    author index + token-derived identity. Assert on the CONTENT only — the
    //    agent name "alice" appears in error/empty responses too, so checking for
    //    it would make this near-unfalsifiable.
    let by_agent = format!("{base}/memory/by-agent");
    let (code, out) = curl(&[
        "-X",
        "POST",
        "-H",
        &bearer,
        "-H",
        "Content-Type: application/json",
        "-d",
        "{}",
        &by_agent,
    ]);
    assert_eq!(code, 200, "by-agent should 200, got {code}: {out}");
    assert!(
        out.contains("launch retrospective"),
        "by-agent should surface alice's actual write, got: {out}"
    );

    // 6) Bad/missing args → clean 4xx, never a 500/panic. mind_search with no
    //    query is a dispatch-level error → 400.
    let (code, out) = curl(&[
        "-X",
        "POST",
        "-H",
        &bearer,
        "-H",
        "Content-Type: application/json",
        "-d",
        "{}",
        &search,
    ]);
    assert!(
        (400..500).contains(&code),
        "a bad request must be a clean 4xx, not {code}: {out}"
    );

    // 7) HTTP/MCP parity: the read + non-destructive routes added so an
    //    HTTP-only agent can do what the MCP agent can. Each must 200 with a
    //    valid token (a thin wrapper over the same dispatch). POST with an empty
    //    or minimal body; we assert reachability + auth, not tool internals.
    for (path, body) in [
        ("/library/list", "{}"),
        ("/fact/query", "{\"subject\":\"user\"}"),
        ("/session/context", "{}"),
        ("/consolidate", "{}"),
        ("/quarantine/list", "{}"),
    ] {
        let url = format!("{base}{path}");
        let (code, out) = curl(&[
            "-X",
            "POST",
            "-H",
            &bearer,
            "-H",
            "Content-Type: application/json",
            "-d",
            body,
            &url,
        ]);
        assert_eq!(
            code, 200,
            "{path} should 200 with a token, got {code}: {out}"
        );
        // And it must require auth like every other route.
        let (code, _) = curl(&[
            "-X",
            "POST",
            "-H",
            "Authorization: Bearer WRONG",
            "-H",
            "Content-Type: application/json",
            "-d",
            body,
            &url,
        ]);
        assert_eq!(code, 401, "{path} must reject a bad token with 401");
    }

    // 8) procedure learn -> recall roundtrip over HTTP, and the `query`->context
    //    convenience mapping (an agent uses one `query` field everywhere).
    let learn = format!("{base}/procedure/learn");
    let (code, out) = curl(&[
        "-X",
        "POST",
        "-H",
        &bearer,
        "-H",
        "Content-Type: application/json",
        "-d",
        "{\"error\":\"DiskFullErr\",\"fix\":\"clear the cache\"}",
        &learn,
    ]);
    assert_eq!(code, 200, "procedure/learn should 200, got {code}: {out}");
    let recall = format!("{base}/procedure/recall");
    let (code, out) = curl(&[
        "-X",
        "POST",
        "-H",
        &bearer,
        "-H",
        "Content-Type: application/json",
        "-d",
        "{\"query\":\"DiskFullErr\"}",
        &recall,
    ]);
    assert_eq!(
        code, 200,
        "procedure/recall(query=) should 200, got {code}: {out}"
    );
    assert!(
        out.to_lowercase().contains("clear the cache"),
        "recall should surface the learned fix, got: {out}"
    );

    // Best-effort cleanup so repeated runs don't accumulate collections in the
    // shared Qdrant. The guard kills the server regardless.
    let _ = Command::new(bin())
        .args(["drop", &lib])
        .env("MGIMIND_HOME", &mind)
        .env("ORT_DYLIB_PATH", &ort)
        .output();
}
