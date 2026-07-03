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

    // 9) A fact-invalidate over HTTP must be attributed in the audit log to the
    //    TOKEN-DERIVED identity (this server runs as `alice:<token>`), NOT to the
    //    terminal/MCP default. (PR-1, circle 1: the actor is resolved per surface
    //    from the bearer token; a network call can't masquerade as "cli", and a
    //    named token is even stronger than the anonymous "http" fallback.)
    let add_fact = Command::new(bin())
        .args(["fact", "add", "bob", "works_at", "Acme"])
        .env("MGIMIND_HOME", &mind)
        .env("ORT_DYLIB_PATH", &ort)
        .output()
        .expect("fact add");
    let add_stdout = String::from_utf8_lossy(&add_fact.stdout);
    let fid = add_stdout
        .split("id: ")
        .nth(1)
        .and_then(|s| s.split(']').next())
        .map(str::trim)
        .expect("fact add prints an id")
        .to_string();
    let invalidate = format!("{base}/fact/invalidate");
    let body = format!("{{\"id\":\"{fid}\"}}");
    let (code, out) = curl(&[
        "-X",
        "POST",
        "-H",
        &bearer, // valid named token (alice), no X-Agent → identity from token
        "-H",
        "Content-Type: application/json",
        "-d",
        &body,
        &invalidate,
    ]);
    assert_eq!(code, 200, "fact/invalidate should 200, got {code}: {out}");
    let log = std::fs::read_to_string(mind.join("audit.log")).expect("audit.log exists");
    // The op serializes as the snake_case wire form `fact_invalidate`.
    let row = log
        .lines()
        .rfind(|l| l.contains("fact_invalidate") && l.contains(&fid))
        .unwrap_or_else(|| panic!("no fact_invalidate row for {fid}; log:\n{log}"));
    assert!(
        row.contains("\"actor\":\"alice\""),
        "HTTP invalidate must be attributed to the token identity 'alice', not cli/mcp; got: {row}"
    );
    assert!(
        row.contains("works_at") || row.contains("bob") || row.contains("Acme"),
        "invalidate audit must record the hidden triple in `before`; got: {row}"
    );

    // 9) /audit route (Track-3 / Crescendo decision-ledger substrate): the
    //    append-only trail is readable over HTTP, and a memory ADD by a named
    //    token is now self-attributed in the audit log (actor=alice), so the
    //    "prove every decision" trail needs no join against the payload index.
    let audit = format!("{base}/audit");
    let (code, out) = curl(&[
        "-X",
        "POST",
        "-H",
        &bearer,
        "-H",
        "Content-Type: application/json",
        "-d",
        "{\"limit\":500}",
        &audit,
    ]);
    assert_eq!(code, 200, "/audit should 200, got {code}: {out}");
    assert!(
        out.contains("\"events\""),
        "/audit must return an events array, got: {out}"
    );
    assert!(
        out.contains("\"op\":\"add\"") && out.contains("\"actor\":\"alice\""),
        "a memory add over a named token must be attributed to alice in the audit \
         log (storage.rs Add event now stamps the author); got: {out}"
    );
    // A bad token is still rejected on the audit route — the trail isn't public.
    let (code, _) = curl(&[
        "-X",
        "POST",
        "-H",
        "Authorization: Bearer WRONG",
        "-H",
        "Content-Type: application/json",
        "-d",
        "{}",
        &audit,
    ]);
    assert_eq!(code, 401, "/audit must reject a bad token with 401");

    // Best-effort cleanup so repeated runs don't accumulate collections in the
    // shared Qdrant. The guard kills the server regardless.
    let _ = Command::new(bin())
        .args(["drop", &lib])
        .env("MGIMIND_HOME", &mind)
        .env("ORT_DYLIB_PATH", &ort)
        .output();
}

/// Locks the v2.0 contract end-to-end: the fail-closed `scope_gate` middleware,
/// write-aware `apply_scope` (library ACL), per-author write flood control, and
/// the duel verdict on `/fact/add` + `/fact/contested`. Same env-gate and idiom
/// as `http_surface_full_contract`; reuses `curl`/`ServerGuard`/`copy_dir`.
///
/// Runs on a DIFFERENT fixed port and DIFFERENT library prefix so it can run in
/// parallel with the other http test against the same shared Qdrant. It never
/// hard-codes a duel outcome (that depends on trust scoring), and every write
/// asserts against a per-author quota budget spelled out in comments (three
/// independent buckets, quota=3), so no 200 can flake from a neighbour's bucket.
#[test]
fn http_v2_acl_flood_verdict_contract() {
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

    // Isolated MGIMIND_HOME with an e5 config + a copy of the model. Quota=3 so a
    // 4th write in one author's rolling window is the 429 boundary.
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
        "write_quota_per_min": 5,
    });
    std::fs::write(
        mind.join("config.json"),
        serde_json::to_string(&cfg).unwrap(),
    )
    .unwrap();

    // A distinct port + library prefix from the other http test (they share this
    // process and Qdrant). pid keeps fact subjects unique across parallel runs.
    let http_port = 47291u16;
    let pid = std::process::id();
    let lib_a = format!("itacl_a_{pid}"); // bob's scoped library
    let lib_priv = format!("itacl_priv_{pid}"); // a library bob is NOT scoped to
    let (tok_admin, tok_bob, tok_carol, tok_flood) =
        ("TOK_admin_1", "TOK_bob_2", "TOK_carol_3", "TOK_flood_4");

    let mut server = ServerGuard(
        Command::new(bin())
            .args(["serve-http", "--port", &http_port.to_string()])
            .arg("--agent-token")
            .arg(format!("admin:{tok_admin}"))
            .arg("--agent-token")
            .arg(format!("bob:{tok_bob}:{lib_a}")) // scoped to lib_a only
            .arg("--agent-token")
            .arg(format!("carol:{tok_carol}"))
            .arg("--agent-token")
            .arg(format!("flood:{tok_flood}"))
            .env("MGIMIND_HOME", &mind)
            .env("ORT_DYLIB_PATH", &ort)
            .spawn()
            .expect("spawn serve-http"),
    );

    let base = format!("http://127.0.0.1:{http_port}");
    let h_admin = format!("Authorization: Bearer {tok_admin}");
    let h_bob = format!("Authorization: Bearer {tok_bob}");
    let h_carol = format!("Authorization: Bearer {tok_carol}");
    let h_flood = format!("Authorization: Bearer {tok_flood}");
    let ct = "Content-Type: application/json";

    // POST helper: (auth header, path, json body) -> (code, body).
    let post = |auth: &str, path: &str, body: &str| -> (u16, String) {
        let url = format!("{base}{path}");
        curl(&["-X", "POST", "-H", auth, "-H", ct, "-d", body, &url])
    };

    // 1) Health up (fast-fail if the port is busy / server died).
    let health = format!("{base}/health");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(Some(status)) = server.0.try_wait() {
            panic!("serve-http exited before /health ({status}) — port {http_port} likely busy");
        }
        if curl(&["-H", &h_admin, &health]).0 == 200 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "server /health never returned 200"
        );
        std::thread::sleep(Duration::from_millis(200));
    }

    // admin creates both libraries (library/create is not quota-counted).
    for l in [&lib_a, &lib_priv] {
        let (code, out) = post(
            &h_admin,
            "/library/create",
            &format!("{{\"name\":\"{l}\"}}"),
        );
        assert_eq!(code, 200, "admin create {l} should 200, got {code}: {out}");
    }

    // 2) scope_gate is fail-closed: bob (scoped) is 403 on every route NOT in
    //    ALLOWED_FOR_SCOPED (http_api.rs), and 200 on /should-search.
    for path in [
        "/memory/by-agent",
        "/fact/add",
        "/fact/query",
        "/fact/invalidate",
        "/fact/contested",
        "/library/create",
        "/library/list",
        "/procedure/learn",
        "/procedure/recall",
        "/consolidate",
        "/quarantine/list",
        "/quarantine/promote",
        "/memory/restore",
        "/session/start",
        "/session/end",
        "/session/last",
        "/session/context",
        "/stats/activity",
        "/audit",
        "/kv/set",
        "/kv/get",
    ] {
        let (code, out) = post(&h_bob, path, "{}");
        assert_eq!(
            code, 403,
            "scoped token must be 403 on {path}, got {code}: {out}"
        );
    }
    assert_eq!(
        post(&h_bob, "/should-search", "{\"query\":\"x\"}").0,
        200,
        "/should-search is on the scoped allowlist"
    );
    // The middleware does not replace auth: a bad bearer is still 401, not 403.
    assert_eq!(
        post(
            "Authorization: Bearer WRONG",
            "/memory/search",
            "{\"query\":\"x\"}"
        )
        .0,
        401,
        "a bad token must 401 even on an allowlisted route"
    );

    // 3) apply_scope write branch. bob may write ONLY into lib_a.
    //    bob write-budget: these two 200s are writes #1 and #2 (of 5).
    let (code, out) = post(
        &h_bob,
        "/memory/ingest",
        "{\"candidates\":[{\"type\":\"memory\",\"content\":\"anything\"}]}",
    );
    assert_eq!(
        code, 403,
        "scoped write with no library must 403, got {code}: {out}"
    );
    let (code, out) = post(
        &h_bob,
        "/memory/ingest",
        &format!(
            "{{\"library\":\"{lib_priv}\",\"candidates\":[{{\"type\":\"memory\",\"content\":\"x\"}}]}}"
        ),
    );
    assert_eq!(
        code, 403,
        "scoped write into a non-allowlisted library must 403, got {code}: {out}"
    );
    let (code, out) = post(&h_bob, "/memory/add", "123"); // non-object body
    assert_eq!(
        code, 403,
        "scoped write with a non-object body must 403, got {code}: {out}"
    );
    let bob_own = "the standup rotates to a new facilitator each sprint";
    let (code, out) = post(
        &h_bob,
        "/memory/ingest",
        &format!(
            "{{\"library\":\"{lib_a}\",\"candidates\":[{{\"type\":\"memory\",\"content\":\"{bob_own}\"}}]}}"
        ),
    );
    assert_eq!(
        code, 200,
        "scoped write into the allowlisted library should 200 (#1), got {code}: {out}"
    );
    let (code, out) = post(
        &h_bob,
        "/memory/add",
        &format!("{{\"library\":\"{lib_a}\",\"content\":\"a second note in lib a\"}}"),
    );
    assert_eq!(
        code, 200,
        "scoped /memory/add into the allowlisted library should 200 (#2), got {code}: {out}"
    );

    // 4) apply_scope read branch + a falsifiable canary. admin plants a secret in
    //    lib_priv; bob must never see it, but MUST see his own lib_a content
    //    (positive control, so "canary absent" can't pass on an empty/broken read).
    let canary = "the vault rotation code is plum-otter-seventine";
    let (code, out) = post(
        &h_admin,
        "/memory/ingest",
        &format!(
            "{{\"library\":\"{lib_priv}\",\"candidates\":[{{\"type\":\"memory\",\"content\":\"{canary}\"}}]}}"
        ),
    );
    assert_eq!(
        code, 200,
        "admin plant canary should 200, got {code}: {out}"
    );
    // Positive control on the CANARY itself: admin (unscoped) must actually find
    // it in lib_priv. A 200 from ingest does not prove storage (the relevance gate
    // could quarantine, the secret scanner could skip), so without this the later
    // "bob does not see the canary" asserts could pass on a canary that was never
    // stored. This is a read, so it spends no write quota.
    let (code, out) = post(
        &h_admin,
        "/memory/search",
        &format!("{{\"query\":\"vault rotation code\",\"library\":\"{lib_priv}\",\"tier\":3}}"),
    );
    assert_eq!(
        code, 200,
        "admin canary search should 200, got {code}: {out}"
    );
    assert!(
        out.contains("plum-otter-seventine"),
        "the canary must actually be stored+searchable in lib_priv (else the ACL \
         asserts below are vacuous), got: {out}"
    );

    // positive control: bob search (no library) finds his OWN content (scope
    // injected lib_a, search works).
    let (code, out) = post(
        &h_bob,
        "/memory/search",
        "{\"query\":\"who facilitates the standup\",\"tier\":3}",
    );
    assert_eq!(code, 200, "bob search should 200, got {code}: {out}");
    assert!(
        out.contains("standup"),
        "bob must find his own lib_a content (positive control), got: {out}"
    );
    // canary is invisible to bob on every read shape. Each negative read asserts
    // 200 first, so a 500/400 can't make "canary absent" pass vacuously.
    let (code, out) = post(
        &h_bob,
        "/memory/search",
        "{\"query\":\"vault rotation code\",\"tier\":3}",
    );
    assert_eq!(code, 200, "bob canary search should 200, got {code}: {out}");
    assert!(
        !out.contains("plum-otter-seventine"),
        "bob search must not surface another library's canary, got: {out}"
    );
    assert_eq!(
        post(
            &h_bob,
            "/memory/search",
            &format!("{{\"query\":\"vault\",\"library\":\"{lib_priv}\"}}")
        )
        .0,
        403,
        "bob naming a non-allowlisted library must 403"
    );
    let (code, out) = post(&h_bob, "/memory/browse", "{}");
    assert_eq!(code, 200, "bob browse should 200, got {code}: {out}");
    assert!(
        !out.contains("plum-otter-seventine"),
        "bob browse must not surface the canary, got: {out}"
    );
    let (code, out) = post(&h_bob, "/memory/browse", "123"); // non-object read → coerced to allowlist
    assert_eq!(
        code, 200,
        "bob non-object browse should 200, got {code}: {out}"
    );
    assert!(
        !out.contains("plum-otter-seventine"),
        "bob non-object browse must stay confined, got: {out}"
    );
    assert_eq!(
        post(
            &h_bob,
            "/memory/recall",
            "{\"query\":\"vault\",\"format\":\"text\"}"
        )
        .0,
        403,
        "scoped recall in text mode must 403 (text render is not library-confined)"
    );
    let (code, out) = post(
        &h_bob,
        "/memory/recall",
        "{\"query\":\"vault rotation code\"}",
    );
    assert_eq!(code, 200, "bob json recall should 200, got {code}: {out}");
    assert!(
        !out.contains("plum-otter-seventine"),
        "bob json recall memories must not surface the canary, got: {out}"
    );

    // 5) Flood control: the `flood` token's own bucket (quota=5). 5 writes 200,
    //    the 6th 429. bob's writes are a different bucket, so they don't interfere.
    for n in 1..=5 {
        let (code, out) = post(
            &h_flood,
            "/memory/add",
            &format!("{{\"library\":\"{lib_a}\",\"content\":\"flood note {n}\"}}"),
        );
        assert_eq!(code, 200, "flood write {n}/5 should 200, got {code}: {out}");
    }
    let (code, out) = post(
        &h_flood,
        "/memory/add",
        &format!("{{\"library\":\"{lib_a}\",\"content\":\"flood note 6\"}}"),
    );
    assert_eq!(
        code, 429,
        "the 6th write in the window (quota=5) must be 429, got {code}: {out}"
    );
    // A malformed fact does NOT spend budget: carol's 400 leaves her bucket empty.
    assert_eq!(
        post(
            &h_carol,
            "/fact/add",
            "{\"subject\":\"x\",\"predicate\":\"y\"}"
        )
        .0,
        400,
        "a fact missing 'object' is a 400 (validated before quota)"
    );

    // 6) Duel verdict wiring on /fact/add, and /fact/contested reachability. The
    //    (subject, predicate) axis is pid-unique, so it can NOT match a registered
    //    cardinality in any shared predicate registry: it defaults to Multi, no
    //    conflict runs, and both adds are deterministically "recorded". This locks
    //    the verdict PLUMBING (that /fact/add returns {id, verdict}) without
    //    depending on a registered predicate's non-deterministic duel outcome.
    //    admin write-budget (quota=5): canary was ingest #1; these are #2 and #3.
    let subj = format!("svc_{pid}");
    let pred = format!("runs_on_{pid}"); // pid-unique → guaranteed Multi (unregistered)
    let (code, out) = post(
        &h_admin,
        "/fact/add",
        &format!("{{\"subject\":\"{subj}\",\"predicate\":\"{pred}\",\"object\":\"port_9001\"}}"),
    );
    assert_eq!(code, 200, "fact/add should 200, got {code}: {out}");
    assert!(
        out.contains("\"id\"") && out.contains("\"verdict\":\"recorded\""),
        "first fact/add must report {{id, verdict:recorded}}, got: {out}"
    );
    let (code, out) = post(
        &h_admin,
        "/fact/add",
        &format!("{{\"subject\":\"{subj}\",\"predicate\":\"{pred}\",\"object\":\"port_9002\"}}"),
    );
    assert_eq!(code, 200, "second fact/add should 200, got {code}: {out}");
    // Multi predicate → the second value coexists, also "recorded" (no duel runs).
    assert!(
        out.contains("\"verdict\":\"recorded\""),
        "second fact/add on a Multi axis must also be 'recorded', got: {out}"
    );
    // /fact/contested is a reachability + shape smoke check: the contested set is
    // global and may be empty; this Multi axis never contests.
    let (code, out) = post(&h_admin, "/fact/contested", "{}");
    assert_eq!(code, 200, "/fact/contested should 200, got {code}: {out}");
    assert!(
        out.contains("\"results\""),
        "/fact/contested must return a results array, got: {out}"
    );

    // Cleanup: drop both libraries (facts in the shared _kg_facts are pid-unique).
    for l in [&lib_a, &lib_priv] {
        let _ = Command::new(bin())
            .args(["drop", l])
            .env("MGIMIND_HOME", &mind)
            .env("ORT_DYLIB_PATH", &ort)
            .output();
    }
}
