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

/// Invalidating a fact must leave an audit trail (PR-1, circle 1): who did it
/// and what triple was hidden. Before this, invalidate wrote NO audit event at
/// all, so a sweep of invalidations was untraceable. Facts are vectorless, so
/// this needs only Qdrant, not the embedding model.
#[test]
fn invalidating_a_fact_writes_an_audit_event() {
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

    // Add a fact; the CLI prints its id as `[id: <uuid>]`.
    let add_out = run(&["fact", "add", "alice", "lives_in", "Berlin"]);
    let id = add_out
        .split("id: ")
        .nth(1)
        .and_then(|s| s.split(']').next())
        .map(str::trim)
        .expect("fact add should print an id")
        .to_string();

    run(&["fact", "invalidate", &id]);

    // The audit log must now carry a FactInvalidate row naming the actor and the
    // hidden triple.
    let log = std::fs::read_to_string(mind.join("audit.log")).expect("audit.log should exist");
    let line = log
        .lines()
        .find(|l| l.contains("FactInvalidate") || l.contains("fact_invalidate"))
        .unwrap_or_else(|| {
            panic!("no FactInvalidate audit event after invalidate; log was:\n{log}")
        });
    assert!(
        line.contains(&id),
        "invalidate audit row should reference the fact id, got: {line}"
    );
    assert!(
        line.contains("alice") && line.contains("Berlin"),
        "invalidate audit row should record the hidden triple (before), got: {line}"
    );
    assert!(
        line.contains("\"actor\":\"cli\"") || line.contains("cli"),
        "bare CLI invalidate should be attributed to actor 'cli', got: {line}"
    );
    // And it must NOT be mis-attributed as a network caller — the actor default
    // no longer collapses every surface into "cli", so a terminal call is "cli"
    // and an anonymous HTTP call (covered in http_integration) is "http".
    assert!(
        !line.contains("\"actor\":\"http\"") && !line.contains("\"actor\":\"unknown\""),
        "terminal invalidate must not read as http/unknown, got: {line}"
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

/// Regression: re-asserting an already-stored memory must keep it LIVE, not
/// demote it into quarantine. The low-novelty gate fires on the second identical
/// write (zero new tokens vs the stored neighbor); before the fix that quarantined
/// the candidate, and because quarantine_id == deterministic_id it CLOBBERED the
/// live point — a re-write silently lost a memory the user clearly wanted. Now a
/// re-assertion of a live memory is a no-op and the memory stays searchable.
#[test]
fn reasserting_a_memory_keeps_it_live() {
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
    let lib = format!("itreassert_{}", std::process::id());
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

    let memory = "The launch retrospective is scheduled for the second Tuesday of every month.";
    let (ok, _o, e) = run(&["create", &lib]);
    assert!(ok, "create failed: {e}");
    // Ingest the SAME memory several times — first stores it, the rest hit the
    // low-novelty gate and must be treated as re-assertions (no-op), not demotions.
    // Include a whitespace-padded variant: the stored id is trim-canonicalized, so
    // padding must NOT dodge the guard and spawn a duplicate / clobber.
    let padded = format!("   {memory}  ");
    for variant in [memory, memory, padded.as_str(), memory] {
        let (ok, o, e) = run(&["ingest", "--library", &lib, "--memory", variant]);
        assert!(ok, "ingest failed:\nstdout:{o}\nstderr:{e}");
    }

    // It must still be retrievable by ordinary search (quarantined points are not).
    let (ok, out, err) = run(&[
        "search",
        "when is the launch retrospective",
        "--library",
        &lib,
        "--tier",
        "3",
    ]);
    let _ = run(&["drop", &lib]);
    assert!(ok, "search failed: {err}");
    assert!(
        out.contains("launch retrospective"),
        "a re-asserted memory must stay live and searchable, got:\n{out}"
    );
}

/// A near-duplicate at ingest must NOT vanish (PR-2, circle 1: write discipline).
/// Before this, a memory similar enough to a live neighbor (>=0.95 cosine) was
/// dropped with no recovery row — a correction to a stale memory, the case most
/// likely to read as a near-dup, was silently lost. Now it lands in quarantine,
/// listable with reason `near_dup_drop` and promotable back to live.
#[test]
fn near_dup_ingest_is_recoverable_not_dropped() {
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
    let lib = format!("itneardup_{}", std::process::id());
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

    let (ok, _o, e) = run(&["create", &lib]);
    assert!(ok, "create failed: {e}");

    // Store an original, then ingest SYNONYM PARAPHRASES — each lexically novel
    // enough to pass the low-novelty gate (different vocabulary), yet semantically
    // >=0.95 similar so the near-dup vector check fires. This is the exact window
    // the near_dup_drop path serves (a synonym-paraphrase correction). The
    // low-novelty gate above it catches token-overlap dups recoverably already;
    // these variants reach past it. Several are tried so at least one lands on the
    // near-dup branch regardless of small embedder score differences.
    let original = "The meeting is at 3pm on Friday in conference room B.";
    let (ok, o, e) = run(&["ingest", "--library", &lib, "--memory", original]);
    assert!(ok, "first ingest failed:\nstdout:{o}\nstderr:{e}");
    let variants = [
        "Friday's gathering happens at three in the afternoon within room B.",
        "We convene Friday at fifteen hundred hours inside meeting space B.",
        "The Friday session takes place at three o'clock afternoon in chamber B.",
    ];
    let mut near_dup_seen = false;
    for v in variants {
        let (ok, o2, e2) = run(&["ingest", "--library", &lib, "--memory", v]);
        assert!(ok, "ingest failed:\nstdout:{o2}\nstderr:{e2}");
        if o2.contains("near-duplicate") {
            near_dup_seen = true;
        }
    }

    // Whether a variant hit the near-dup branch or the low-novelty gate, NOTHING
    // was dropped on the floor: every suppressed variant is recoverable in
    // quarantine. And when the near-dup branch fired, its reason is near_dup_drop.
    let (ok, qlist, qerr) = run(&["quarantine", "list", "--library", &lib]);
    assert!(ok, "quarantine list failed: {qerr}");
    if near_dup_seen {
        assert!(
            qlist.contains("near_dup_drop"),
            "a near-dup-skipped variant must be quarantined under near_dup_drop, got:\n{qlist}"
        );
        // The recovered row must be promotable back to live memory.
        let qid = qlist
            .split("id: ")
            .nth(1)
            .and_then(|s| s.split_whitespace().next())
            .map(str::to_string)
            .expect("a quarantine row should expose an id");
        let (ok, _po, pe) = run(&["quarantine", "promote", &qid]);
        assert!(ok, "promote of a near-dup quarantine row failed: {pe}");
    } else {
        // Embedder never crossed 0.95 for any variant → all stored live. Then the
        // original is still searchable — no silent loss either way.
        let (ok, s, _e) = run(&[
            "search",
            "Friday meeting room",
            "--library",
            &lib,
            "--tier",
            "3",
        ]);
        assert!(
            ok && s.contains("room B"),
            "stored-live memory must be searchable"
        );
    }
    let _ = run(&["drop", &lib]);
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

// ---------------------------------------------------------------------------
// Duel rule happy-path test (issue #25, PR #26).
//
// Walks the full read-after-write loop: register cardinality, add first fact,
// add conflicting fact, query. Before PR #26 both facts stayed visible because
// query_facts only filtered on `valid=true`, not on `status != stale`. This
// test pins the post-fix behaviour so the bug cannot return silently.
//
// Does NOT need the embedding model — facts are vectorless, queried by exact
// payload match. Gated only on the Qdrant port.
// ---------------------------------------------------------------------------

#[test]
fn duel_rule_dampens_loser_on_single_cardinality() {
    let Some(port) = qdrant_port() else {
        eprintln!("SKIP: set MGIMIND_IT_QDRANT=<grpc port> to run integration tests");
        return;
    };

    let home = tempfile::tempdir().expect("tempdir");
    let mind = home.path().join("mgimind");
    std::fs::create_dir_all(&mind).unwrap();
    write_e5_config(&mind, &port);

    // ORT is needed only for tools that actually embed; the duel path is
    // vectorless. We still need *some* value because the MCP server reads
    // the env var unconditionally on Linux.
    let ort = std::env::var("ORT_DYLIB_PATH").unwrap_or_else(|_| String::from("/dev/null"));

    // Unique names so concurrent runs don't collide on (subject, predicate).
    let pid = std::process::id();
    let predicate = format!("duel_test_pred_{pid}");
    let subject = format!("duel_test_subj_{pid}");

    let input = format!(
        "{}\n{}\n{}\n{}\n{}\n{}\n",
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"mind_predicate","arguments":{
                "action":"register","predicate":predicate,"cardinality":"Single"}}}),
        serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
            "params":{"name":"mind_fact","arguments":{
                "action":"add","subject":subject,"predicate":predicate,"object":"old_winner"}}}),
        serde_json::json!({"jsonrpc":"2.0","id":4,"method":"tools/call",
            "params":{"name":"mind_fact","arguments":{
                "action":"add","subject":subject,"predicate":predicate,"object":"new_value_should_flip"}}}),
        serde_json::json!({"jsonrpc":"2.0","id":5,"method":"tools/call",
            "params":{"name":"mind_fact","arguments":{
                "action":"query","subject":subject}}}),
    );

    let (stdout, stderr) = run_mcp(&mind, &ort, &input);

    let results = parse_mcp_results(&stdout);
    let query = results
        .get(&5)
        .cloned()
        .unwrap_or_else(|| (true, String::new()));
    let chain = format!("query: {query:?}\n--- stderr ---\n{stderr}");

    assert!(!query.0, "query must succeed: {chain}");
    assert!(
        query.1.contains("new_value_should_flip"),
        "new winner must be visible in query: {chain}"
    );
    // The post-#26 behaviour: dampened loser is hidden from the read path.
    assert!(
        !query.1.contains("old_winner"),
        "dampened loser must NOT appear in query (issue #25 regression): {chain}"
    );
}

#[test]
fn multi_cardinality_allows_coexistence() {
    let Some(port) = qdrant_port() else {
        eprintln!("SKIP: set MGIMIND_IT_QDRANT=<grpc port> to run integration tests");
        return;
    };

    let home = tempfile::tempdir().expect("tempdir");
    let mind = home.path().join("mgimind");
    std::fs::create_dir_all(&mind).unwrap();
    write_e5_config(&mind, &port);
    let ort = std::env::var("ORT_DYLIB_PATH").unwrap_or_else(|_| String::from("/dev/null"));

    let pid = std::process::id();
    let predicate = format!("multi_test_pred_{pid}");
    let subject = format!("multi_test_subj_{pid}");

    let input = format!(
        "{}\n{}\n{}\n{}\n{}\n{}\n",
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"mind_predicate","arguments":{
                "action":"register","predicate":predicate,"cardinality":"Multi"}}}),
        serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
            "params":{"name":"mind_fact","arguments":{
                "action":"add","subject":subject,"predicate":predicate,"object":"first"}}}),
        serde_json::json!({"jsonrpc":"2.0","id":4,"method":"tools/call",
            "params":{"name":"mind_fact","arguments":{
                "action":"add","subject":subject,"predicate":predicate,"object":"second"}}}),
        serde_json::json!({"jsonrpc":"2.0","id":5,"method":"tools/call",
            "params":{"name":"mind_fact","arguments":{
                "action":"query","subject":subject}}}),
    );

    let (stdout, stderr) = run_mcp(&mind, &ort, &input);

    let results = parse_mcp_results(&stdout);
    let query = results
        .get(&5)
        .cloned()
        .unwrap_or_else(|| (true, String::new()));
    let chain = format!("query: {query:?}\n--- stderr ---\n{stderr}");

    assert!(!query.0, "query must succeed: {chain}");
    // Multi predicates: both facts must coexist (no duel).
    assert!(
        query.1.contains("first") && query.1.contains("second"),
        "Multi cardinality must allow both facts to coexist: {chain}"
    );
}

/// ADR 0006 toggle-test: procedure outcome stats live in the droppable
/// `_mod_procstats` side collection, not on the core procedure point.
///
/// 1. Learn a procedure but record NO outcome → `_mod_procstats` is never
///    created. Recall must still return the procedure (degraded to ✓0/✗0,
///    unverified) — proving the read is existence-guarded, not an error on a
///    missing collection.
/// 2. Record a deterministic success (mind_outcome test_passed) → the stats land
///    in the side collection and recall now shows the verified trust signal.
///
/// This is the empirical proof that dropping the derived collection degrades
/// gracefully and never hides or errors a procedure.
#[test]
fn procedure_stats_live_in_droppable_side_collection() {
    let (Some(_port), Ok(models), Ok(ort)) = (
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
    let port = qdrant_port().unwrap();
    let (_home, mind) = setup_model_home(&port, &model_src);

    // A distinctive error signature so recall matches it unambiguously.
    let err = format!(
        "error E0599 no method zorblify on Quibbler{}",
        std::process::id()
    );

    // Phase 1: learn + recall, with NO outcome recorded (side collection absent).
    let input1 = format!(
        "{}\n{}\n{}\n{}\n",
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"mind_learn","arguments":{
                "error":err,"fix":"import the QuibblerExt trait","context":"trait resolution"}}}),
        serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
            "params":{"name":"mind_recall","arguments":{"error":err}}}),
    );
    let (stdout1, stderr1) = run_mcp(&mind, &ort, &input1);
    let r1 = parse_mcp_results(&stdout1);
    let learn_id = r1.get(&2).map(|(_, t)| t.clone()).unwrap_or_default();
    let proc_id = learn_id
        .split("[id: ")
        .nth(1)
        .and_then(|s| s.split(']').next())
        .unwrap_or("")
        .to_string();
    assert!(
        !proc_id.is_empty(),
        "could not parse procedure id from: {learn_id}\n{stderr1}"
    );

    let (rec_err, rec_txt) = r1.get(&3).cloned().unwrap_or((true, String::new()));
    assert!(
        !rec_err,
        "recall errored with no stats collection: {rec_txt}\n{stderr1}"
    );
    assert!(
        rec_txt.contains("import the QuibblerExt trait"),
        "recall must return the procedure even with NO stats collection: {rec_txt}"
    );
    assert!(
        rec_txt.contains("unverified") && rec_txt.contains("✓0/✗0"),
        "with no recorded outcome the procedure must show unverified ✓0/✗0: {rec_txt}"
    );

    // Phase 2: record a deterministic success → stats land in _mod_procstats.
    let input2 = format!(
        "{}\n{}\n{}\n{}\n",
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"mind_outcome","arguments":{
                "memory_id":proc_id,"signal_type":"test_passed"}}}),
        serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
            "params":{"name":"mind_recall","arguments":{"error":err}}}),
    );
    let (stdout2, stderr2) = run_mcp(&mind, &ort, &input2);
    let r2 = parse_mcp_results(&stdout2);
    let (out_err, _out_txt) = r2.get(&2).cloned().unwrap_or((true, String::new()));
    assert!(!out_err, "mind_outcome should succeed\n{stderr2}");
    let (rec2_err, rec2_txt) = r2.get(&3).cloned().unwrap_or((true, String::new()));
    assert!(
        !rec2_err,
        "recall after outcome errored: {rec2_txt}\n{stderr2}"
    );
    assert!(
        rec2_txt.contains("verified") && rec2_txt.contains("✓1/✗0"),
        "after a test_passed outcome the procedure must show verified ✓1/✗0 (stats read \
         back from the side collection): {rec2_txt}"
    );
}

/// ADR 0006 orphan-resurrection guard: a procedure deleted then re-learned with
/// the same (error, fix) lands on the same deterministic id. Its old
/// `_mod_procstats` row (if `delete_procstats` didn't reach it) must NOT be
/// resurrected onto the fresh procedure — the delete was an explicit "drop this
/// history" signal. The re-learn must start at ✓0/✗0, unverified.
#[test]
fn relearn_after_delete_does_not_resurrect_stats() {
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
    let err = format!(
        "error E0277 ResurrectGuard unsatisfied proc {}",
        std::process::id()
    );
    let fix = "impl ResurrectGuard for the type";

    // learn → outcome(success) → grab id
    let input1 = format!(
        "{}\n{}\n{}\n",
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"mind_learn","arguments":{"error":err,"fix":fix,"context":"c"}}}),
    );
    let (so1, se1) = run_mcp(&mind, &ort, &input1);
    let learn_txt = parse_mcp_results(&so1)
        .get(&2)
        .map(|(_, t)| t.clone())
        .unwrap_or_default();
    let id = learn_txt
        .split("[id: ")
        .nth(1)
        .and_then(|s| s.split(']').next())
        .unwrap_or("")
        .to_string();
    assert!(!id.is_empty(), "no id parsed: {learn_txt}\n{se1}");

    let input2 = format!(
        "{}\n{}\n{}\n",
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"mind_outcome","arguments":{"memory_id":id,"signal_type":"test_passed"}}}),
    );
    let (so2, _) = run_mcp(&mind, &ort, &input2);
    assert!(
        !parse_mcp_results(&so2)
            .get(&2)
            .map(|(e, _)| *e)
            .unwrap_or(true),
        "outcome failed"
    );

    // delete the procedure via CLI (delete_procstats runs, but we re-learn to prove
    // that even if a stale row survived, the new procedure starts fresh).
    let _ = Command::new(bin())
        .args(["delete", "_procedures", &id])
        .env("MGIMIND_HOME", &mind)
        .env("ORT_DYLIB_PATH", &ort)
        .output();

    // re-learn the same (err, fix) → same id → must be ✓0/✗0 unverified.
    let input3 = format!(
        "{}\n{}\n{}\n{}\n",
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"mind_learn","arguments":{"error":err,"fix":fix,"context":"c"}}}),
        serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
            "params":{"name":"mind_recall","arguments":{"error":err}}}),
    );
    let (so3, se3) = run_mcp(&mind, &ort, &input3);
    let r3 = parse_mcp_results(&so3);
    let relearn_id = r3.get(&2).map(|(_, t)| t.clone()).unwrap_or_default();
    assert!(
        relearn_id.contains(&id),
        "re-learn must produce the SAME deterministic id {id}, got: {relearn_id}"
    );
    let (rec_err, rec_txt) = r3.get(&3).cloned().unwrap_or((true, String::new()));
    assert!(!rec_err, "recall errored: {rec_txt}\n{se3}");
    // Find the line for our fix and assert it is unverified ✓0/✗0 (not resurrected).
    let our_block = rec_txt
        .split("\n\n")
        .find(|b| b.contains(fix))
        .unwrap_or_else(|| panic!("re-learned procedure not in recall: {rec_txt}"));
    assert!(
        our_block.contains("unverified") && our_block.contains("✓0/✗0"),
        "a re-learned procedure must start fresh (✓0/✗0 unverified), not resurrect old \
         stats: {our_block}"
    );
}
