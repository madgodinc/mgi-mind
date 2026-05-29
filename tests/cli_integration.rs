//! Black-box integration tests: run the built `mgimind` binary against a real
//! Qdrant and assert on its behavior. These exercise the storage layer, the
//! single-collection layout, the library registry, and the CLI end to end -
//! exactly the parts unit tests cannot reach.
//!
//! They are gated on `MGIMIND_IT_QDRANT=<grpc port>` so a plain `cargo test`
//! without a Qdrant just skips them. CI starts a Qdrant service and sets it.
//! The library-lifecycle test writes no points (only collection/registry
//! operations), so it is safe to point at any Qdrant.

use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_mgimind")
}

/// Write a tempdir `config.json` for `multilingual-e5-base` pointed at `port`,
/// symlink in the model, and return the fake HOME. Shared by the search tests.
#[cfg(unix)]
fn setup_model_home(port: &str, model_src: &std::path::Path) -> tempfile::TempDir {
    let home = tempfile::tempdir().expect("tempdir");
    let mind = home.path().join("mgimind");
    std::fs::create_dir_all(mind.join("models")).unwrap();
    std::os::unix::fs::symlink(model_src, mind.join("models/multilingual-e5-base")).unwrap();
    std::fs::write(
        mind.join("config.json"),
        format!(
            r#"{{"version":"it","data_dir":"{}","model_name":"multilingual-e5-base","qdrant_port":{},"vector_size":768,"pooling":"mean","uses_token_type_ids":false,"query_prefix":"query: ","passage_prefix":"passage: ","rerank_enabled":false}}"#,
            mind.display(),
            port
        ),
    )
    .unwrap();
    home
}

/// Returns the test Qdrant gRPC port, or None to skip.
fn qdrant_port() -> Option<String> {
    std::env::var("MGIMIND_IT_QDRANT").ok()
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
    std::fs::write(
        mind.join("config.json"),
        format!(
            r#"{{"version":"it","data_dir":"{}","model_name":"multilingual-e5-base","qdrant_port":{},"vector_size":768,"pooling":"mean","uses_token_type_ids":false}}"#,
            mind.display(),
            port
        ),
    )
    .unwrap();

    let run = |args: &[&str]| -> String {
        let out = Command::new(bin())
            .args(args)
            .env("HOME", home.path())
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
///
/// Unix-only: it symlinks the model dir into the tempdir HOME (via
/// `setup_model_home`), which uses `std::os::unix::fs::symlink`. On Windows CI it
/// skips on the env gate anyway, so gating the whole test keeps the Windows build
/// clean instead of referencing a `#[cfg(unix)]` helper that isn't compiled there.
#[cfg(unix)]
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

    let home = setup_model_home(&port, &model_src);
    let lib = format!("itsearch_{}", std::process::id());
    let run = |args: &[&str]| -> (bool, String, String) {
        let out = Command::new(bin())
            .args(args)
            .env("HOME", home.path())
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
    assert!(run(&["create", &lib]).0);
    let (ok, _out, err) = run(&[
        "add",
        &lib,
        "The Eiffel Tower stands in Paris and was completed in 1889.",
    ]);
    assert!(ok, "add failed: {err}");

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
#[cfg(unix)]
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

    let home = setup_model_home(&port, &model_src);
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
        .env("HOME", home.path())
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
        .env("HOME", home.path())
        .env("ORT_DYLIB_PATH", &ort)
        .output(); // cleanup regardless of assertions

    // Every non-empty stdout line must be valid JSON-RPC - no stray prints.
    let mut search_text = None;
    for line in stdout.lines().filter(|l| !l.trim().is_empty()) {
        let v: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("non-JSON on stdout: {e}\n{line}"));
        if v.get("id").and_then(serde_json::Value::as_i64) == Some(4) {
            assert_eq!(
                v["result"]["isError"], false,
                "search reported isError\n{line}"
            );
            search_text = v["result"]["content"][0]["text"]
                .as_str()
                .map(str::to_owned);
        }
    }

    let text = search_text.unwrap_or_else(|| {
        panic!("no search response (id 4)\nstdout:\n{stdout}\nstderr:\n{stderr}")
    });
    assert!(
        text.contains("Eiffel Tower") && text.contains("Paris"),
        "MCP search should retrieve the added memory, got:\n{text}"
    );
}
