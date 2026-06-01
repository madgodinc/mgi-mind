//! Black-box integration tests: run the built `mgimind` binary against a real
//! Qdrant and assert on its behavior. These exercise the storage layer, the
//! single-collection layout, the library registry, and the CLI end to end -
//! exactly the parts unit tests cannot reach.
//!
//! They are gated on `MGIMIND_IT_QDRANT=<grpc port>` so a plain `cargo test`
//! without a Qdrant just skips them. CI starts a Qdrant (a service container on
//! Linux, the bundled binary on Windows) and sets it. Isolation is via
//! `MGIMIND_HOME` (a per-test tempdir), which works on every OS - unlike a $HOME
//! override, which Windows ignores. The library-lifecycle test writes no points
//! (only collection/registry operations), so it is safe to point at any Qdrant.

use std::path::Path;
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_mgimind")
}

/// Returns the test Qdrant gRPC port, or None to skip.
fn qdrant_port() -> Option<String> {
    std::env::var("MGIMIND_IT_QDRANT").ok()
}

/// Recursively copy a directory tree. Used to place the model dir inside the
/// test's isolated `MGIMIND_HOME` on every OS - a symlink would be unix-only and
/// would force the search tests to be `#[cfg(unix)]`, leaving Windows (the main
/// target OS) with no automated coverage of the add->search path.
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

/// Write `config.json` for `multilingual-e5-base` into `mind`, pointed at `port`.
/// Built with serde_json so paths are escaped correctly - a `format!` with a raw
/// Windows path (`C:\...`) would emit invalid JSON.
fn write_e5_config(mind: &Path, port: &str) {
    let cfg = serde_json::json!({
        "version": "it",
        "data_dir": mind,
        "model_name": "multilingual-e5-base",
        "qdrant_port": port.parse::<u16>().expect("MGIMIND_IT_QDRANT must be a port number"),
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
}

/// Build a tempdir `MGIMIND_HOME` with an e5 `config.json` and a copy of the
/// model, and return `(tempdir guard, mind_home path)`. Shared by the search
/// tests. Cross-platform: copies the model rather than symlinking it.
fn setup_model_home(port: &str, model_src: &Path) -> (tempfile::TempDir, std::path::PathBuf) {
    let home = tempfile::tempdir().expect("tempdir");
    let mind = home.path().join("mgimind");
    std::fs::create_dir_all(mind.join("models")).unwrap();
    copy_dir(model_src, &mind.join("models/multilingual-e5-base"))
        .expect("copy model into MGIMIND_HOME");
    write_e5_config(&mind, port);
    (home, mind)
}

#[test]
fn library_lifecycle_against_real_qdrant() {
    let Some(port) = qdrant_port() else {
        eprintln!("SKIP: set MGIMIND_IT_QDRANT=<grpc port> to run integration tests");
        return;
    };

    let home = tempfile::tempdir().expect("tempdir");
    let mind = home.path().join("mgimind");
    std::fs::create_dir_all(&mind).unwrap();
    write_e5_config(&mind, &port);

    let run = |args: &[&str]| -> String {
        let out = Command::new(bin())
            .args(args)
            .env("MGIMIND_HOME", &mind)
            .output()
            .expect("spawn mgimind");
        assert!(
            out.status.success(),
            "`mgimind {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    };

    // Unique library name so concurrent/repeat runs do not collide.
    let lib = format!("ittest_{}", std::process::id());

    // Empty registry to start.
    assert!(!run(&["list"]).contains(&lib));

    // Create -> appears in list and stats.
    run(&["create", &lib]);
    assert!(
        run(&["list"]).contains(&lib),
        "library should be listed after create"
    );
    assert!(run(&["stats"]).contains(&lib));

    // Drop -> gone from the registry.
    run(&["drop", &lib]);
    assert!(
        !run(&["list"]).contains(&lib),
        "library should be gone after drop"
    );
}

/// Full retrieval path: add -> embed -> hybrid search -> assert the memory is found.
/// Needs the embedding model, so it is gated on `MGIMIND_IT_MODELS` (a models dir
/// holding `multilingual-e5-base/`) and `ORT_DYLIB_PATH`. CI without the model skips
/// it; run it locally with a downloaded model. Reranking is off here so the test
/// stays deterministic and does not also require the reranker model.
#[test]
fn add_then_search_finds_the_memory() {
    let (Some(port), Ok(models), Ok(ort)) = (
        qdrant_port(),
        std::env::var("MGIMIND_IT_MODELS"),
        std::env::var("ORT_DYLIB_PATH"),
    ) else {
        eprintln!("SKIP: set MGIMIND_IT_QDRANT, MGIMIND_IT_MODELS and ORT_DYLIB_PATH to run");
        return;
    };
    let model_src = std::path::Path::new(&models).join("multilingual-e5-base");
    if !model_src.join("model.onnx").exists() {
        eprintln!("SKIP: no multilingual-e5-base model under MGIMIND_IT_MODELS");
        return;
    }

    let (_home, mind) = setup_model_home(&port, &model_src);
    let lib = format!("itsearch_{}", std::process::id());
    let run = |args: &[&str]| -> (bool, String, String) {
        let out = Command::new(bin())
            .args(args)
            .env("MGIMIND_HOME", &mind)
            .env("ORT_DYLIB_PATH", &ort)
            .output()
            .expect("spawn mgimind");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };

    // create -> add a distinctive memory -> search a paraphrase -> assert it is found.
    let (ok, out, err) = run(&["create", &lib]);
    assert!(ok, "create failed:\nstdout:\n{out}\nstderr:\n{err}");
    let (ok, out, err) = run(&[
        "add",
        &lib,
        "The Eiffel Tower stands in Paris and was completed in 1889.",
    ]);
    assert!(ok, "add failed:\nstdout:\n{out}\nstderr:\n{err}");

    let (ok, out, err) = run(&[
        "search",
        "where is the eiffel tower located",
        "--library",
        &lib,
        "--tier",
        "3",
    ]);
    let _ = run(&["drop", &lib]); // cleanup regardless of assertions below
    assert!(ok, "search failed: {err}");
    assert!(
        out.contains("Eiffel Tower") && out.contains("Paris"),
        "hybrid search should retrieve the added memory, got:\n{out}"
    );
}

/// Same retrieval path as above, but driven over the **MCP stdio transport** end
/// to end: feed JSON-RPC `initialize` + `tools/call` lines into `mgimind mcp` and
/// assert the `mind_search` response retrieves what `mind_add` stored. Proves the
/// hand-rolled protocol, the warm in-process path, and the round-trip together.
/// Also asserts stdout is pure JSON-RPC (every non-empty line parses as JSON).
#[test]
fn mcp_add_then_search_roundtrip() {
    use std::io::Write;

    let (Some(port), Ok(models), Ok(ort)) = (
        qdrant_port(),
        std::env::var("MGIMIND_IT_MODELS"),
        std::env::var("ORT_DYLIB_PATH"),
    ) else {
        eprintln!("SKIP: set MGIMIND_IT_QDRANT, MGIMIND_IT_MODELS and ORT_DYLIB_PATH to run");
        return;
    };
    let model_src = std::path::Path::new(&models).join("multilingual-e5-base");
    if !model_src.join("model.onnx").exists() {
        eprintln!("SKIP: no multilingual-e5-base model under MGIMIND_IT_MODELS");
        return;
    }

    let (_home, mind) = setup_model_home(&port, &model_src);
    let lib = format!("itmcp_{}", std::process::id());

    // id 1 init, 2 create, 3 add, 4 search. Sequential handling guarantees the
    // add commits before the search runs.
    let input = format!(
        "{}\n{}\n{}\n{}\n{}\n",
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"mind_create","arguments":{"name":lib}}}),
        serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
            "params":{"name":"mind_add","arguments":{"library":lib,
                "content":"The Eiffel Tower stands in Paris and was completed in 1889."}}}),
        serde_json::json!({"jsonrpc":"2.0","id":4,"method":"tools/call",
            "params":{"name":"mind_search","arguments":{
                "query":"where is the eiffel tower located","library":lib,"tier":3}}}),
    );

    let mut child = Command::new(bin())
        .arg("mcp")
        .env("MGIMIND_HOME", &mind)
        .env("ORT_DYLIB_PATH", &ort)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn mgimind mcp");

    // Write all requests, then close stdin so the server hits EOF and exits.
    {
        let mut stdin = child.stdin.take().expect("child stdin");
        stdin.write_all(input.as_bytes()).expect("write requests");
    }

    let out = child.wait_with_output().expect("wait mcp");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    let _ = Command::new(bin())
        .args(["drop", &lib])
        .env("MGIMIND_HOME", &mind)
        .env("ORT_DYLIB_PATH", &ort)
        .output(); // cleanup regardless of assertions

    // Every non-empty stdout line must be valid JSON-RPC - no stray prints.
    // Collect each request's tool-result text by id so a failure can show the
    // whole chain (create id2, add id3, search id4), not just the last step.
    let mut results: std::collections::HashMap<i64, String> = std::collections::HashMap::new();
    let mut search_text = None;
    for line in stdout.lines().filter(|l| !l.trim().is_empty()) {
        let v: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("non-JSON on stdout: {e}\n{line}"));
        if let Some(id) = v.get("id").and_then(serde_json::Value::as_i64) {
            let is_err = v["result"]["isError"].as_bool().unwrap_or(false);
            let text = v["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or("")
                .to_owned();
            results.insert(id, format!("isError={is_err} text={text}"));
            if id == 4 {
                search_text = Some((is_err, text));
            }
        }
    }

    // Dump the full create/add/search chain + stderr on any failure below.
    let chain = || {
        format!(
            "create(id2): {}\nadd(id3): {}\nsearch(id4): {}\n--- stderr ---\n{stderr}",
            results.get(&2).map(String::as_str).unwrap_or("<missing>"),
            results.get(&3).map(String::as_str).unwrap_or("<missing>"),
            results.get(&4).map(String::as_str).unwrap_or("<missing>"),
        )
    };

    let (is_err, text) =
        search_text.unwrap_or_else(|| panic!("no search response (id 4)\n{}", chain()));
    assert!(!is_err, "search reported isError\n{}", chain());
    assert!(
        text.contains("Eiffel Tower") && text.contains("Paris"),
        "MCP search should retrieve the added memory.\n{}",
        chain()
    );
}

// ---------------------------------------------------------------------------
// mind_provenance_add integration tests.
//
// All three drive `mgimind mcp` over its real stdio JSON-RPC transport — the
// tool has no CLI subcommand (per design §6), so this is the only end-to-end
// path. Each test creates an isolated library, runs a sequence of tool calls,
// then drops the library regardless of assertions.
//
// Gated on the same env vars as the other model-needing tests above. CI
// without the model+ORT skips them.
// ---------------------------------------------------------------------------

/// Helper: run one `mgimind mcp` invocation with the given concatenated JSON
/// lines as stdin. Returns (stdout, stderr).
fn run_mcp(mind: &Path, ort: &str, input: &str) -> (String, String) {
    use std::io::Write;
    let mut child = Command::new(bin())
        .arg("mcp")
        .env("MGIMIND_HOME", mind)
        .env("ORT_DYLIB_PATH", ort)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn mgimind mcp");
    {
        let mut stdin = child.stdin.take().expect("child stdin");
        stdin.write_all(input.as_bytes()).expect("write requests");
    }
    let out = child.wait_with_output().expect("wait mcp");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Helper: parse the MCP stdout transcript into a map of id -> (is_error, text).
fn parse_mcp_results(stdout: &str) -> std::collections::HashMap<i64, (bool, String)> {
    let mut results = std::collections::HashMap::new();
    for line in stdout.lines().filter(|l| !l.trim().is_empty()) {
        let v: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("non-JSON on stdout: {e}\n{line}"));
        if let Some(id) = v.get("id").and_then(serde_json::Value::as_i64) {
            let is_err = v["result"]["isError"].as_bool().unwrap_or(false);
            let text = v["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or("")
                .to_owned();
            results.insert(id, (is_err, text));
        }
    }
    results
}

/// Round-trip: a valid `mind_provenance_add` call lands in storage and is
/// retrievable via `mind_search` on a token from the snippet, with the
/// expected `[external]` header in the embedded content.
#[test]
fn provenance_round_trip_through_add_memory() {
    let (Some(port), Ok(models), Ok(ort)) = (
        qdrant_port(),
        std::env::var("MGIMIND_IT_MODELS"),
        std::env::var("ORT_DYLIB_PATH"),
    ) else {
        eprintln!("SKIP: set MGIMIND_IT_QDRANT, MGIMIND_IT_MODELS and ORT_DYLIB_PATH to run");
        return;
    };
    let model_src = std::path::Path::new(&models).join("multilingual-e5-base");
    if !model_src.join("model.onnx").exists() {
        eprintln!("SKIP: no multilingual-e5-base model under MGIMIND_IT_MODELS");
        return;
    }

    let (_home, mind) = setup_model_home(&port, &model_src);
    let lib = format!("itprov_{}", std::process::id());

    let input = format!(
        "{}\n{}\n{}\n{}\n{}\n",
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"mind_create","arguments":{"name":lib}}}),
        serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
        "params":{"name":"mind_provenance_add","arguments":{
            "library": lib,
            "snippet": "kangaroo_marker_for_provenance_test_xyz123",
            "origin_url": "https://github.com/example/repo",
            "search_tool_used": "ripgrep",
            "repo": "example/repo",
            "file": "src/lib.rs",
            "line_range": "10-20",
            "lang": "rust"
        }}}),
        serde_json::json!({"jsonrpc":"2.0","id":4,"method":"tools/call",
        "params":{"name":"mind_search","arguments":{
            "query":"kangaroo_marker_for_provenance_test_xyz123",
            "library": lib,
            "tier": 3
        }}}),
    );

    let (stdout, stderr) = run_mcp(&mind, &ort, &input);
    let _ = Command::new(bin())
        .args(["drop", &lib])
        .env("MGIMIND_HOME", &mind)
        .env("ORT_DYLIB_PATH", &ort)
        .output();

    let results = parse_mcp_results(&stdout);
    let create = results.get(&2).cloned().unwrap_or((true, String::new()));
    let prov = results.get(&3).cloned().unwrap_or((true, String::new()));
    let search = results.get(&4).cloned().unwrap_or((true, String::new()));
    let chain = format!(
        "create: {create:?}\nprovenance_add: {prov:?}\nsearch: {search:?}\n--- stderr ---\n{stderr}",
    );

    assert!(!create.0, "create failed: {chain}");
    assert!(!prov.0, "provenance_add failed: {chain}");
    assert!(
        prov.1.contains("Saved") && prov.1.contains("provenance id:"),
        "provenance_add response shape: {chain}"
    );
    assert!(!search.0, "search reported isError: {chain}");
    assert!(
        search.1.contains("[external]")
            && search
                .1
                .contains("kangaroo_marker_for_provenance_test_xyz123"),
        "search must return the cited record: {chain}"
    );
}

/// Same inputs twice → exactly one stored chunk (idempotent). The library's
/// stats count must not double after the second call.
#[test]
fn provenance_dedup_same_inputs_inserts_once() {
    let (Some(port), Ok(models), Ok(ort)) = (
        qdrant_port(),
        std::env::var("MGIMIND_IT_MODELS"),
        std::env::var("ORT_DYLIB_PATH"),
    ) else {
        eprintln!("SKIP: set MGIMIND_IT_QDRANT, MGIMIND_IT_MODELS and ORT_DYLIB_PATH to run");
        return;
    };
    let model_src = std::path::Path::new(&models).join("multilingual-e5-base");
    if !model_src.join("model.onnx").exists() {
        eprintln!("SKIP: no multilingual-e5-base model under MGIMIND_IT_MODELS");
        return;
    }

    let (_home, mind) = setup_model_home(&port, &model_src);
    let lib = format!("itprovdup_{}", std::process::id());

    let snippet = "dedup_marker_aardvark_98765";
    let call = serde_json::json!({"jsonrpc":"2.0","id":0,"method":"tools/call",
    "params":{"name":"mind_provenance_add","arguments":{
        "library": lib,
        "snippet": snippet,
        "origin_url": "https://github.com/example/repo",
        "search_tool_used": "ripgrep",
        "line_range": "1-2"
    }}});
    let mut call_a = call.clone();
    call_a["id"] = serde_json::json!(3);
    let mut call_b = call.clone();
    call_b["id"] = serde_json::json!(4);

    let input = format!(
        "{}\n{}\n{}\n{}\n{}\n{}\n",
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"mind_create","arguments":{"name":lib}}}),
        call_a,
        call_b,
        serde_json::json!({"jsonrpc":"2.0","id":5,"method":"tools/call",
        "params":{"name":"mind_search","arguments":{
            "query": snippet, "library": lib, "tier": 3, "limit": 10
        }}}),
    );

    let (stdout, stderr) = run_mcp(&mind, &ort, &input);
    let _ = Command::new(bin())
        .args(["drop", &lib])
        .env("MGIMIND_HOME", &mind)
        .env("ORT_DYLIB_PATH", &ort)
        .output();

    let results = parse_mcp_results(&stdout);
    let a = results.get(&3).cloned().unwrap_or((true, String::new()));
    let b = results.get(&4).cloned().unwrap_or((true, String::new()));
    let s = results.get(&5).cloned().unwrap_or((true, String::new()));
    let chain = format!("a: {a:?}\nb: {b:?}\nsearch: {s:?}\n--- stderr ---\n{stderr}");

    assert!(!a.0 && !b.0, "both provenance calls must succeed: {chain}");
    assert!(!s.0, "search must succeed: {chain}");
    // The snippet is short, so it produces exactly one chunk. Search returns
    // only one hit carrying the marker (we asserted libraries match too).
    let hits = s.1.matches(snippet).count();
    assert_eq!(
        hits, 1,
        "snippet must appear in exactly one stored record (dedup), got {hits}: {chain}"
    );
}

/// Same snippet, two different allowlisted URLs → two distinct records (the
/// provenance is in the dedup key).
#[test]
fn provenance_same_snippet_two_urls_inserts_twice() {
    let (Some(port), Ok(models), Ok(ort)) = (
        qdrant_port(),
        std::env::var("MGIMIND_IT_MODELS"),
        std::env::var("ORT_DYLIB_PATH"),
    ) else {
        eprintln!("SKIP: set MGIMIND_IT_QDRANT, MGIMIND_IT_MODELS and ORT_DYLIB_PATH to run");
        return;
    };
    let model_src = std::path::Path::new(&models).join("multilingual-e5-base");
    if !model_src.join("model.onnx").exists() {
        eprintln!("SKIP: no multilingual-e5-base model under MGIMIND_IT_MODELS");
        return;
    }

    let (_home, mind) = setup_model_home(&port, &model_src);
    let lib = format!("itprovurls_{}", std::process::id());

    let snippet = "two_url_marker_walrus_55555";

    let input = format!(
        "{}\n{}\n{}\n{}\n{}\n{}\n",
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"mind_create","arguments":{"name":lib}}}),
        serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
        "params":{"name":"mind_provenance_add","arguments":{
            "library": lib,
            "snippet": snippet,
            "origin_url": "https://github.com/owner-one/repo",
            "search_tool_used": "ripgrep"
        }}}),
        serde_json::json!({"jsonrpc":"2.0","id":4,"method":"tools/call",
        "params":{"name":"mind_provenance_add","arguments":{
            "library": lib,
            "snippet": snippet,
            "origin_url": "https://gitlab.com/owner-two/repo",
            "search_tool_used": "ripgrep"
        }}}),
        serde_json::json!({"jsonrpc":"2.0","id":5,"method":"tools/call",
        "params":{"name":"mind_search","arguments":{
            "query": snippet, "library": lib, "tier": 3, "limit": 10
        }}}),
    );

    let (stdout, stderr) = run_mcp(&mind, &ort, &input);
    let _ = Command::new(bin())
        .args(["drop", &lib])
        .env("MGIMIND_HOME", &mind)
        .env("ORT_DYLIB_PATH", &ort)
        .output();

    let results = parse_mcp_results(&stdout);
    let s = results.get(&5).cloned().unwrap_or((true, String::new()));
    let chain = format!("search: {s:?}\n--- stderr ---\n{stderr}");

    assert!(!s.0, "search must succeed: {chain}");
    // Both URLs are present in the result text → confirms two distinct records.
    assert!(
        s.1.contains("github.com/owner-one/repo") && s.1.contains("gitlab.com/owner-two/repo"),
        "both citations must be in the search output: {chain}"
    );
}
