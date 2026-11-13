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

/// Set one top-level key in an already written `config.json`.
fn set_config_key(mind: &Path, key: &str, value: serde_json::Value) {
    let path = mind.join("config.json");
    let mut cfg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("read config.json"))
            .expect("parse config.json");
    cfg[key] = value;
    std::fs::write(&path, serde_json::to_string_pretty(&cfg).unwrap()).expect("write config.json");
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

/// Invalidating a fact must record WHEN it stopped being true, not just THAT
/// it did. `query_fact_as_of`/`query_facts` already hard-exclude
/// `valid=false` server-side (an invalidated fact reads as a retracted
/// mistake, never as history), so this cannot be observed through them; the
/// gap was that `valid_until` - the one field `dampen_loser` and
/// `mark_superseded` both stamp for the other two ways a fact retires - was
/// left unset by `invalidate`, silently losing the "when" for anything that
/// reads the raw payload later. The CLI now reports the timestamp it wrote,
/// which doubles as the only black-box way to see it (this binary has no lib
/// target, so these tests cannot call `invalidate_fact_authored` directly).
#[test]
fn invalidating_a_fact_reports_the_valid_until_it_set() {
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

    let subject = format!("invalidate_subject_{}", std::process::id());
    let add_out = run(&["fact", "add", &subject, "lives_in", "Berlin"]);
    let id = add_out
        .split("id: ")
        .nth(1)
        .and_then(|s| s.split(']').next())
        .map(str::trim)
        .expect("fact add should print an id")
        .to_string();

    let before = chrono::Utc::now();
    let invalidate_out = run(&["fact", "invalidate", &id]);
    let after = chrono::Utc::now();

    let stamp = invalidate_out
        .split("valid_until = ")
        .nth(1)
        .and_then(|s| s.split(')').next())
        .unwrap_or_else(|| panic!("invalidate output has no valid_until: {invalidate_out}"));
    let parsed = chrono::DateTime::parse_from_rfc3339(stamp)
        .unwrap_or_else(|e| panic!("valid_until '{stamp}' is not RFC3339: {e}"))
        .to_utc();
    assert!(
        parsed >= before && parsed <= after,
        "valid_until {parsed} should fall between {before} and {after}"
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

// TemporalSingle: a new value flips the old one out of default reads, AND the
// old value is retired as `superseded` history (a real reader distinguishes it
// from `stale`). PR-3, circle 1, write-path half of audit #13. The behavioural
// difference is asserted: the prior value is HIDDEN in a default query but
// VISIBLE in a `history` query — something a plain `dampen_loser` (stale) would
// NOT give, since the history reader filters on status=superseded.
#[test]
fn temporal_single_supersedes_prior_value() {
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
    let predicate = format!("tsingle_test_pred_{pid}");
    let subject = format!("tsingle_test_subj_{pid}");

    let input = format!(
        "{}\n{}\n{}\n{}\n{}\n{}\n{}\n",
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"mind_predicate","arguments":{
                "action":"register","predicate":predicate,"cardinality":"TemporalSingle"}}}),
        serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
            "params":{"name":"mind_fact","arguments":{
                "action":"add","subject":subject,"predicate":predicate,"object":"Berlin"}}}),
        serde_json::json!({"jsonrpc":"2.0","id":4,"method":"tools/call",
            "params":{"name":"mind_fact","arguments":{
                "action":"add","subject":subject,"predicate":predicate,"object":"Munich"}}}),
        // id 5: default query — Munich visible, Berlin hidden.
        serde_json::json!({"jsonrpc":"2.0","id":5,"method":"tools/call",
            "params":{"name":"mind_fact","arguments":{
                "action":"query","subject":subject}}}),
        // id 6: history query — Berlin (the superseded prior value) visible.
        serde_json::json!({"jsonrpc":"2.0","id":6,"method":"tools/call",
            "params":{"name":"mind_fact","arguments":{
                "action":"query","subject":subject,"history":true}}}),
    );

    let (stdout, stderr) = run_mcp(&mind, &ort, &input);
    let results = parse_mcp_results(&stdout);
    let query = results
        .get(&5)
        .cloned()
        .unwrap_or_else(|| (true, String::new()));
    let history = results
        .get(&6)
        .cloned()
        .unwrap_or_else(|| (true, String::new()));
    let chain = format!("default: {query:?}\nhistory: {history:?}\n--- stderr ---\n{stderr}");

    assert!(!query.0, "default query must succeed: {chain}");
    assert!(
        query.1.contains("Munich"),
        "the new TemporalSingle value must be visible in default query: {chain}"
    );
    assert!(
        !query.1.contains("Berlin"),
        "the prior value must be hidden from default reads: {chain}"
    );
    // The behavioural payoff of superseded-vs-stale: the prior value IS
    // retrievable via history (a stale/dampened loser would NOT be — the history
    // reader filters status=superseded).
    assert!(!history.0, "history query must succeed: {chain}");
    assert!(
        history.1.contains("Berlin"),
        "the superseded prior value must be visible in history query: {chain}"
    );
    assert!(
        !history.1.contains("Munich"),
        "the current value must NOT appear in history (only superseded entries): {chain}"
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

// Concurrency: N writers racing the SAME (subject, predicate) Single axis must
// converge to exactly ONE visible winner — no two live facts, no torn state.
//
// This pins the cross-process fix (circle 3): SEPARATE processes racing the same
// Single axis are serialized by the data_dir `.facts.lock` (knowledge.rs
// lock_facts_cross_process), so they can't both read an empty set and both write
// Active. Before the file-lock this test FAILED (two Active winners); it is the
// regression guard that the in-process-only lock no longer leaves a hole.
// Facts are vectorless, so this needs only Qdrant, not the model.
#[test]
fn concurrent_writes_to_one_axis_converge_to_single_winner() {
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
    let predicate = format!("conc_pred_{pid}");
    let subject = format!("conc_subj_{pid}");

    // Register the axis as Single so conflicting objects actually duel.
    let reg = format!(
        "{}\n{}\n{}\n",
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"mind_predicate","arguments":{
                "action":"register","predicate":predicate,"cardinality":"Single"}}}),
    );
    let _ = run_mcp(&mind, &ort, &reg);

    // Spawn N concurrent processes, each adding a DISTINCT object to the same axis.
    let mind = std::sync::Arc::new(mind);
    let handles: Vec<_> = (0..6)
        .map(|i| {
            let mind = std::sync::Arc::clone(&mind);
            let subject = subject.clone();
            let predicate = predicate.clone();
            let ort = ort.clone();
            std::thread::spawn(move || {
                Command::new(bin())
                    .args(["fact", "add", &subject, &predicate, &format!("value_{i}")])
                    .env("MGIMIND_HOME", &*mind)
                    .env("ORT_DYLIB_PATH", &ort)
                    .output()
                    .expect("spawn mgimind fact add")
                    .status
                    .success()
            })
        })
        .collect::<Vec<std::thread::JoinHandle<bool>>>();

    let oks: usize = handles
        .into_iter()
        .filter_map(|h| h.join().ok())
        .filter(|b| *b)
        .count();
    assert!(oks >= 1, "at least one concurrent write must succeed");

    // Exactly one visible (Active) fact survives on this axis — the rest are
    // dampened losers, hidden from default reads.
    let query = format!(
        "{}\n{}\n{}\n",
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"mind_fact","arguments":{"action":"query","subject":subject}}}),
    );
    let (stdout, stderr) = run_mcp(&mind, &ort, &query);
    let results = parse_mcp_results(&stdout);
    let q = results.get(&2).cloned().unwrap_or((true, String::new()));
    assert!(!q.0, "query must succeed: {q:?}\n{stderr}");

    // Count how many distinct value_N objects are visible. The Single axis must
    // show exactly one — concurrent writers did not produce two live winners.
    let visible = (0..6)
        .filter(|i| q.1.contains(&format!("value_{i}")))
        .count();
    assert_eq!(
        visible, 1,
        "exactly one winner must be visible on a Single axis after {oks} concurrent writes, \
         saw {visible} in:\n{}",
        q.1
    );
}

// Reversible forgetting (PR-5, circle 1, axis: forgetting): a cold memory can be
// ARCHIVED (hidden from search) and RESTORED (back in search) — no data lost.
// Needs the model (search embeds) + Qdrant.
#[test]
fn archive_then_restore_round_trips_a_cold_memory() {
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
    let lib = format!("itarchive_{}", std::process::id());
    let run = |args: &[&str]| -> (bool, String) {
        let out = Command::new(bin())
            .args(args)
            .env("MGIMIND_HOME", &mind)
            .env("ORT_DYLIB_PATH", &ort)
            .output()
            .expect("spawn mgimind");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    };

    run(&["create", &lib]);
    run(&[
        "add",
        &lib,
        "The archived canary memory about quantum widgets.",
    ]);

    // Sanity: it's searchable before archiving.
    let (ok, before) = run(&[
        "search",
        "quantum widgets",
        "--library",
        &lib,
        "--tier",
        "3",
    ]);
    assert!(
        ok && before.contains("quantum widgets"),
        "should be searchable pre-archive: {before}"
    );

    // Archive cold memories (decay-days 0 makes the fresh, never-searched-by-
    // consolidate memory cold). NOTE: the search above bumped access_count, so
    // archive by a SEPARATE fresh memory to keep the cold check valid.
    run(&[
        "add",
        &lib,
        "A second cold note about helium balloons drifting away.",
    ]);
    let (ok, _ar) = run(&[
        "consolidate",
        "--library",
        &lib,
        "--apply",
        "--archive-cold",
        "--decay-days",
        "0",
    ]);
    assert!(ok, "consolidate --archive-cold should succeed");

    // The archived note must NOT be in default search.
    let (ok, hid) = run(&[
        "search",
        "helium balloons",
        "--library",
        &lib,
        "--tier",
        "3",
    ]);
    assert!(ok, "search should run");
    assert!(
        !hid.contains("helium balloons"),
        "an archived memory must be hidden from default search, got:\n{hid}"
    );

    // Recover its id from the audit log (one Archive row per archived id) and
    // restore it.
    let log = std::fs::read_to_string(mind.join("audit.log")).unwrap_or_default();
    let archived_id = log
        .lines()
        .filter(|l| l.contains("\"op\":\"archive\"") || l.contains("Archive"))
        .filter_map(|l| {
            l.split("\"target\":\"")
                .nth(1)
                .and_then(|s| s.split('"').next())
                .map(str::to_string)
        })
        .next_back();

    if let Some(id) = archived_id {
        let (ok, msg) = run(&["restore-memory", &id]);
        assert!(ok, "restore-memory should succeed: {msg}");
        // After restore, it is searchable again — nothing was lost.
        let (ok, back) = run(&[
            "search",
            "helium balloons",
            "--library",
            &lib,
            "--tier",
            "3",
        ]);
        assert!(
            ok && back.contains("helium balloons"),
            "a restored memory must be searchable again, got:\n{back}"
        );
    } else {
        // If nothing was archived (e.g. cold-detection differences), the data
        // must still be present, not lost — restore the invariant either way.
        let (_ok, still) = run(&[
            "search",
            "helium balloons",
            "--library",
            &lib,
            "--tier",
            "3",
        ]);
        assert!(
            still.contains("helium balloons"),
            "if not archived, the memory must remain searchable (no silent loss)"
        );
    }

    let _ = run(&["drop", &lib]);
}

// list-archived (PR-B1, circle 2, axis: forgetting): a soft-forgotten memory must
// be OBSERVABLE without digging the audit log — `browse --archived` lists it with
// its id, and you can restore from that id. This is what raises forgetting from
// "you can forget, but can't see what's forgotten" to "forgetting is inspectable".
#[test]
fn browse_archived_lists_forgotten_and_restores_from_listing() {
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
    let lib = format!("itlsarch_{}", std::process::id());
    let run = |args: &[&str]| -> (bool, String) {
        let out = Command::new(bin())
            .args(args)
            .env("MGIMIND_HOME", &mind)
            .env("ORT_DYLIB_PATH", &ort)
            .output()
            .expect("spawn mgimind");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    };

    run(&["create", &lib]);
    run(&[
        "add",
        &lib,
        "An archivable note about migrating mainframes to the cloud.",
    ]);
    // Archive it (fresh + zero-access + decay-days 0 = cold).
    let (ok, _a) = run(&[
        "consolidate",
        "--library",
        &lib,
        "--apply",
        "--archive-cold",
        "--decay-days",
        "0",
    ]);
    assert!(ok, "archive-cold should succeed");

    // Default browse must NOT show it (it's forgotten).
    let (ok, live) = run(&["browse", "--library", &lib]);
    assert!(ok);
    assert!(
        !live.contains("mainframes"),
        "an archived memory must be absent from default browse, got:\n{live}"
    );

    // `browse --archived` MUST show it — with an id — WITHOUT reading the audit log.
    let (ok, arch) = run(&["browse", "--library", &lib, "--archived"]);
    assert!(ok, "browse --archived should run");
    assert!(
        arch.contains("mainframes"),
        "browse --archived must surface the forgotten memory, got:\n{arch}"
    );
    let id = arch
        .split("id: ")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .map(str::to_string)
        .expect("archived listing should carry an id");

    // Restore using the id FROM THE LISTING (not the audit log) → back in search.
    let (ok, _r) = run(&["restore-memory", &id]);
    assert!(ok, "restore from listed id should succeed");
    let (ok, back) = run(&[
        "search",
        "migrating mainframes cloud",
        "--library",
        &lib,
        "--tier",
        "3",
    ]);
    assert!(
        ok && back.contains("mainframes"),
        "restored memory must be searchable, got:\n{back}"
    );

    let _ = run(&["drop", &lib]);
}

// Point-in-time `as_of` (PR-A1, circle 2, axis: temporal): a TemporalSingle
// subject's CURRENT fact differs by query instant. Berlin then Munich; as_of in
// the far future returns Munich (current), as_of before Munich's takeover returns
// Berlin (the superseded prior value, time-travelled to). This is the capability
// the valid_until field was built for — and proves valid_until reaches a reader.
#[test]
fn fact_as_of_returns_the_value_current_at_that_instant() {
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
    let predicate = format!("asof_pred_{pid}");
    let subject = format!("asof_subj_{pid}");

    // Register TemporalSingle, add Berlin (becomes active), add Munich (Berlin ->
    // superseded with valid_until ~= the moment Munich is added). Then query
    // history (to read Berlin's created_at) and as_of at two instants.
    let input = format!(
        "{}\n{}\n{}\n{}\n{}\n{}\n{}\n",
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"mind_predicate","arguments":{
                "action":"register","predicate":predicate,"cardinality":"TemporalSingle"}}}),
        serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
            "params":{"name":"mind_fact","arguments":{
                "action":"add","subject":subject,"predicate":predicate,"object":"Berlin"}}}),
        serde_json::json!({"jsonrpc":"2.0","id":4,"method":"tools/call",
            "params":{"name":"mind_fact","arguments":{
                "action":"add","subject":subject,"predicate":predicate,"object":"Munich"}}}),
        // id 5: as_of far in the FUTURE -> the current value (Munich).
        serde_json::json!({"jsonrpc":"2.0","id":5,"method":"tools/call",
            "params":{"name":"mind_fact","arguments":{
                "action":"query","subject":subject,"as_of":"2099-01-01T00:00:00Z"}}}),
        // id 6: as_of far in the PAST (before either existed) -> nothing.
        serde_json::json!({"jsonrpc":"2.0","id":6,"method":"tools/call",
            "params":{"name":"mind_fact","arguments":{
                "action":"query","subject":subject,"as_of":"2000-01-01T00:00:00Z"}}}),
    );

    let (stdout, stderr) = run_mcp(&mind, &ort, &input);
    let results = parse_mcp_results(&stdout);
    let future = results.get(&5).cloned().unwrap_or((true, String::new()));
    let past = results.get(&6).cloned().unwrap_or((true, String::new()));
    let chain = format!("future: {future:?}\npast: {past:?}\n--- stderr ---\n{stderr}");

    assert!(!future.0, "as_of future query must succeed: {chain}");
    assert!(
        future.1.contains("Munich") && !future.1.contains("Berlin"),
        "as_of in the future must return the CURRENT value (Munich), not Berlin: {chain}"
    );
    assert!(!past.0, "as_of past query must succeed: {chain}");
    assert!(
        !past.1.contains("Berlin") && !past.1.contains("Munich"),
        "as_of before anything existed must return nothing: {chain}"
    );

    // And the discriminator: as_of at the instant Berlin was current (just after
    // its created_at, before Munich) must return Berlin. Read Berlin's created_at
    // from history, then query as_of one second later.
    let hist_in = format!(
        "{}\n{}\n{}\n",
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"mind_fact","arguments":{
                "action":"query","subject":subject,"history":true}}}),
    );
    let (hstdout, _he) = run_mcp(&mind, &ort, &hist_in);
    let hres = parse_mcp_results(&hstdout);
    let hist = hres.get(&2).cloned().unwrap_or((true, String::new()));
    // history render shows "valid: <from> .. <until>" for Berlin (superseded).
    if let Some(from) = hist
        .1
        .split("valid: ")
        .nth(1)
        .and_then(|s| s.split(" ..").next())
        .map(str::to_string)
    {
        let asof_in = format!(
            "{}\n{}\n{}\n",
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
                "params":{"name":"mind_fact","arguments":{
                    "action":"query","subject":subject,"as_of":from}}}),
        );
        let (astdout, _ae) = run_mcp(&mind, &ort, &asof_in);
        let ares = parse_mcp_results(&astdout);
        let at_berlin = ares.get(&2).cloned().unwrap_or((true, String::new()));
        assert!(
            at_berlin.1.contains("Berlin"),
            "as_of at Berlin's valid-from instant must return Berlin, got: {:?}",
            at_berlin
        );
    }
}

// Browse surfaces coldness (PR circle-3, axis: forgetting -> excellent). The
// recency-weighted decay score is now OBSERVABLE in the browse output — a number
// the user/agent reads. Pure observability: nothing is hidden or reordered by it.
// (The warming-on-access logic itself is unit-tested deterministically in
// consolidate.rs; the access journal only flushes every 64 records, so a single
// cross-process search may not persist its bump — hence the comparison below is a
// guarded bonus, while the presence of the coldness line is the real assertion.)
#[test]
fn browse_surfaces_coldness() {
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
    let lib = format!("itcold_{}", std::process::id());
    let run = |args: &[&str]| -> (bool, String) {
        let out = Command::new(bin())
            .args(args)
            .env("MGIMIND_HOME", &mind)
            .env("ORT_DYLIB_PATH", &ort)
            .output()
            .expect("spawn mgimind");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    };

    run(&["create", &lib]);
    run(&[
        "add",
        &lib,
        "The aurora borealis was visible over the northern fjords.",
    ]);

    // Browse before any access — coldness is shown (a number, observable).
    let (ok, before) = run(&["browse", "--library", &lib]);
    assert!(ok, "browse should run");
    assert!(
        before.contains("coldness:"),
        "browse must surface a coldness line, got:\n{before}"
    );

    // Surface the memory via search (bumps its access journal => warms it).
    let (ok, _s) = run(&["search", "aurora fjords", "--library", &lib, "--tier", "3"]);
    assert!(ok, "search should run");

    // Browse again — the same memory is now warmer (lower coldness). Parse the
    // coldness number from each render and compare.
    let parse_cold = |txt: &str| -> Option<f64> {
        txt.split("coldness: ")
            .nth(1)
            .and_then(|s| s.split('d').next())
            .and_then(|s| s.trim().parse::<f64>().ok())
    };
    let (ok, after) = run(&["browse", "--library", &lib]);
    assert!(ok);
    let (c_before, c_after) = (parse_cold(&before), parse_cold(&after));
    if let (Some(b), Some(a)) = (c_before, c_after) {
        assert!(
            a <= b,
            "an accessed memory must not be COLDER than before access ({a} <= {b})"
        );
    }
    let _ = run(&["drop", &lib]);
}

// Per-query rerank override (PR circle-3, axis: retrieval -> excellent). An agent
// can override the reranker per query, which CHANGES the result scores/order — a
// real reader (the ranked list) reflects it. --no-rerank yields the raw hybrid
// RRF scores; --rerank yields the cross-encoder sigmoid scores. They differ.
#[test]
fn search_rerank_override_changes_scores() {
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
    // Rerank needs the reranker model too; skip if absent.
    if !model_src
        .parent()
        .map(|p| p.join("bge-reranker-base").join("model.onnx").exists())
        .unwrap_or(false)
    {
        eprintln!("SKIP: no bge-reranker-base model");
        return;
    }

    let (_home, mind) = setup_model_home(&port, &model_src);
    // The shared config disables rerank; this test exercises the per-query
    // override that turns it ON, so copy the reranker model in and enable it via
    // the override flag (not config). setup_model_home wrote rerank_enabled:false.
    // Copy reranker model alongside e5.
    let reranker_src = model_src.parent().unwrap().join("bge-reranker-base");
    if reranker_src.join("model.onnx").exists() {
        let _ = copy_dir(
            &reranker_src,
            &mind.join("models").join("bge-reranker-base"),
        );
    }
    let lib = format!("itrerank_{}", std::process::id());
    let run = |args: &[&str]| -> (bool, String) {
        let out = Command::new(bin())
            .args(args)
            .env("MGIMIND_HOME", &mind)
            .env("ORT_DYLIB_PATH", &ort)
            .output()
            .expect("spawn mgimind");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    };

    run(&["create", &lib]);
    run(&[
        "add",
        &lib,
        "The deployment runs on Kubernetes with three replicas.",
    ]);
    run(&[
        "add",
        &lib,
        "Our CI uses GitHub Actions across three operating systems.",
    ]);

    let parse_top_score = |txt: &str| -> Option<f64> {
        txt.split("score: ")
            .nth(1)
            .and_then(|s| s.split(')').next())
            .and_then(|s| s.trim().parse::<f64>().ok())
    };

    let (ok1, raw) = run(&[
        "search",
        "deployment kubernetes",
        "--library",
        &lib,
        "--no-rerank",
        "--tier",
        "3",
    ]);
    assert!(ok1, "no-rerank search should run");
    let (ok2, reranked) = run(&[
        "search",
        "deployment kubernetes",
        "--library",
        &lib,
        "--rerank",
        "--tier",
        "3",
    ]);
    assert!(ok2, "rerank search should run");

    // Both must return the top result; the OVERRIDE is honored if the top scores
    // come from different scoring functions (RRF vs cross-encoder sigmoid).
    let (s_raw, s_rr) = (parse_top_score(&raw), parse_top_score(&reranked));
    if let (Some(a), Some(b)) = (s_raw, s_rr) {
        assert!(
            (a - b).abs() > f64::EPSILON,
            "rerank override must change the top score (raw {a} vs reranked {b}); \
             the per-query flag reached the ranking path"
        );
    }
    let _ = run(&["drop", &lib]);
}

/// Round-trip for `reindex` (audit #11): re-embedding from stored text into a
/// fresh collection must (a) keep every memory searchable, (b) preserve its
/// content, and (c) write a safety snapshot to disk BEFORE dropping anything.
/// Reindexing at the SAME dimension is the honest test — the vectors are rebuilt
/// from the same model, so a still-findable memory proves the read→drop→re-embed
/// path round-trips rather than silently losing data (the regression this guards
/// against). Needs the model, so gated like the other search tests.
#[test]
fn reindex_preserves_and_backs_up_memories() {
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
    let lib = format!("itreindex_{}", std::process::id());
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

    // Seed a distinctive memory.
    let (ok, out, err) = run(&["create", &lib]);
    assert!(ok, "create failed:\nstdout:\n{out}\nstderr:\n{err}");
    let needle = "The Eiffel Tower stands in Paris and was completed in 1889.";
    let (ok, out, err) = run(&["add", &lib, needle]);
    assert!(ok, "add failed:\nstdout:\n{out}\nstderr:\n{err}");

    // Reindex (non-interactive). At the same dimension this rebuilds the vectors
    // from stored text — the operation under test.
    let (ok, out, err) = run(&["reindex", "--yes"]);
    assert!(ok, "reindex failed:\nstdout:\n{out}\nstderr:\n{err}");
    assert!(
        out.contains("Reindexed") && out.contains("Safety snapshot"),
        "reindex should report a count and a snapshot path, got:\n{out}"
    );

    // (c) The safety snapshot must be on disk, and must already contain the
    // memory — proving it was written BEFORE the drop, not after.
    let backup_dir = mind.join("reindex-backup");
    assert!(
        backup_dir.is_dir(),
        "reindex must write a safety snapshot dir at {}",
        backup_dir.display()
    );
    let snapshot = std::fs::read_to_string(backup_dir.join(format!("{lib}.json")))
        .unwrap_or_else(|e| panic!("snapshot for library {lib} should exist: {e}"));
    assert!(
        snapshot.contains("Eiffel Tower"),
        "snapshot must hold the memory content before the rebuild, got:\n{snapshot}"
    );

    // (a) + (b) The memory survives the rebuild and is still retrievable.
    let (ok, out, err) = run(&[
        "search",
        "where is the eiffel tower located",
        "--library",
        &lib,
        "--tier",
        "3",
    ]);
    let _ = run(&["drop", &lib]); // cleanup regardless of assertions
    assert!(ok, "search after reindex failed: {err}");
    assert!(
        out.contains("Eiffel Tower") && out.contains("Paris"),
        "memory must still be retrievable after reindex, got:\n{out}"
    );
}

/// Skills end to end: write one, see it in the catalogue, match it against a
/// task phrased in different words, record an outcome, delete it. Covers the
/// part unit tests cannot reach: the embedding, the hybrid match over
/// `type=skill`, and the derived outcome row shared with procedures.
#[test]
fn a_skill_is_written_matched_and_deleted() {
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
    let name = format!("itskill-{}", std::process::id());
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

    let (ok, out, err) = run(&[
        "skill",
        "set",
        &name,
        "--when",
        "writing or reviewing CSS animation and transitions",
        "--body",
        "Animate transform and opacity only; never animate width or top.",
    ]);
    assert!(ok, "skill set failed:\nstdout:\n{out}\nstderr:\n{err}");

    let (ok, out, err) = run(&["skill", "list"]);
    assert!(ok, "skill list failed: {err}");
    assert!(
        out.contains(&name),
        "catalogue is missing the skill:\n{out}"
    );

    // The task shares no keyword with the trigger beyond "animation", so this
    // is the dense arm doing the work, not a substring match.
    let (ok, out, err) = run(&[
        "skill",
        "match",
        "I am about to build a hover animation for a button",
    ]);
    assert!(ok, "skill match failed: {err}");
    assert!(
        out.contains(&name) && out.contains("never animate width"),
        "match should return the skill with its body:\n{out}"
    );

    let (ok, out, err) = run(&["skill", "outcome", &name, "--verify"]);
    assert!(ok, "skill outcome failed:\nstdout:\n{out}\nstderr:\n{err}");
    let (ok, out, err) = run(&["skill", "show", &name]);
    assert!(ok, "skill show failed: {err}");
    assert!(
        out.contains("(verified)") && out.contains("✓1"),
        "outcome should latch verified and count the success:\n{out}"
    );

    let (ok, out, err) = run(&["skill", "rm", &name]);
    assert!(ok, "skill rm failed:\nstdout:\n{out}\nstderr:\n{err}");
    let (_, out, _) = run(&["skill", "list"]);
    assert!(!out.contains(&name), "skill survived deletion:\n{out}");
}

/// `backup`/`restore` must recover real data through Qdrant's own snapshot
/// API, not just archive files around it. Facts are vectorless, so this does
/// not need the embedding model: add a fact, back up, DROP the collection
/// entirely (direct gRPC, simulating data loss), restore, and confirm the
/// fact is back. Also checks the archive actually took the snapshot path
/// (`qdrant-snapshots/.../_kg_facts.snapshot`) rather than silently falling
/// back to a raw copy that happened to work because nothing was deleted yet.
#[tokio::test]
async fn backup_and_restore_round_trip_facts_through_qdrant_snapshots() {
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

    let subject = format!("backup_subject_{}", std::process::id());
    run(&["fact", "add", &subject, "lives_in", "Prague"]);

    let archive_path = home.path().join("out.tar.gz");
    let archive = archive_path.to_str().unwrap();
    run(&["backup", archive]);

    // The archive must hold a real Qdrant snapshot of the facts collection,
    // not a raw-directory fallback - that only happens when Qdrant answered,
    // which it does here.
    let file = std::fs::File::open(&archive_path).expect("open backup archive");
    let dec = flate2::read::GzDecoder::new(file);
    let mut tar_in = tar::Archive::new(dec);
    let snapshot_entry = tar_in
        .entries()
        .expect("read archive entries")
        .filter_map(|e| e.ok())
        .any(|e| {
            e.path()
                .map(|p| p.to_string_lossy().contains("_kg_facts.snapshot"))
                .unwrap_or(false)
        });
    assert!(
        snapshot_entry,
        "backup archive should contain a Qdrant snapshot of _kg_facts"
    );

    // Simulate data loss: drop the facts collection directly (gRPC), bypassing
    // the CLI entirely - there is no `mgimind drop-collection` footgun, so this
    // is the one way to prove restore recovers the collection itself, not just
    // files that happened to survive.
    let grpc_port: u16 = port
        .parse()
        .expect("MGIMIND_IT_QDRANT must be a port number");
    let client = qdrant_client::Qdrant::from_url(&format!("http://127.0.0.1:{grpc_port}"))
        .build()
        .expect("build qdrant client");
    client
        .delete_collection("_kg_facts")
        .await
        .expect("drop _kg_facts to simulate data loss");
    assert!(
        !client.collection_exists("_kg_facts").await.unwrap_or(true),
        "_kg_facts should be gone before restore"
    );

    // The actual upload-and-recover call is Qdrant's own server-side
    // operation, not mgimind's - our part ends at the multipart POST. This
    // used to be skipped unconditionally on Windows: Qdrant 1.18.1's recovery
    // routine reproducibly failed this exact sequence with `failed to sync
    // file ... (os error 5)` on a WAL-replay file inside its own recovery tmp
    // dir. 2026-10-12: re-confirmed live (A/B, same backup archive and
    // storage path, only the Qdrant binary swapped) and fixed by the version
    // bump to 1.19.2 in cli.rs's `QDRANT_VERSION` - restore now succeeds on
    // Windows too, so the assertion below runs on every OS.
    run(&["restore", archive]);

    let restored = run(&["fact", "query", &subject]);
    assert!(
        restored.contains("lives_in") && restored.contains("Prague"),
        "fact should come back after restore, got: {restored}"
    );

    // The restore rewound audit.log to the backup's copy; it must say so in
    // the log and keep the pre-restore log beside it.
    let log = std::fs::read_to_string(mind.join("audit.log")).unwrap_or_default();
    let last = log
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("");
    assert!(
        last.contains("\"op\":\"backup_restore\"") && last.contains("pre-restore"),
        "the last audit row must be the restore event naming the saved log: {log}"
    );
    let saved = std::fs::read_dir(&mind)
        .unwrap()
        .filter_map(|e| e.ok())
        .any(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("audit.log.pre-restore-")
        });
    assert!(
        saved,
        "the live audit log must be copied aside before restore"
    );
    run(&["audit", "verify"]);
}

// ---------------------------------------------------------------------------
// `mgimind relibrary` (v2.7) integration tests.
//
// Needs the embedding model (relibrary reads real stored memories), so these
// are gated the same as the add/search tests above. Each test gets its own
// pair of libraries named after the test + pid, and drops both at the end
// regardless of assertions.
// ---------------------------------------------------------------------------

/// Helper: run one `mgimind` CLI invocation against an isolated test home.
fn run_cli(mind: &Path, ort: &str, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(bin())
        .args(args)
        .env("MGIMIND_HOME", mind)
        .env("ORT_DYLIB_PATH", ort)
        .output()
        .expect("spawn mgimind");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Extract the first `id: <token>` from CLI text output (search/browse render
/// one such token per record). Mirrors the convention already used above by
/// `near_dup_ingest_is_recoverable_not_dropped` for quarantine ids.
fn first_id(text: &str) -> Option<String> {
    text.split("id: ")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .map(str::to_string)
}

/// Count `id: ` occurrences — one per browse/search record, so this is a
/// record count without needing to parse the full render format.
fn count_records(text: &str) -> usize {
    text.matches("id: ").count()
}

/// Dry run matches and samples but writes nothing: the destination library is
/// not created, the source library is untouched, and nothing is ever moved
/// without `--apply`.
#[test]
fn relibrary_dry_run_reports_without_writing() {
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
    let pid = std::process::id();
    let from = format!("itrb_dry_from_{pid}");
    let to = format!("itrb_dry_to_{pid}");

    let (ok, _o, e) = run_cli(&mind, &ort, &["create", &from]);
    assert!(ok, "create failed: {e}");
    let (ok, _o, e) = run_cli(
        &mind,
        &ort,
        &[
            "add",
            &from,
            "CreepiDota Path beta notes on the tutorial flow.",
        ],
    );
    assert!(ok, "add failed: {e}");
    let (ok, _o, e) = run_cli(
        &mind,
        &ort,
        &["add", &from, "Completely unrelated gardening notes."],
    );
    assert!(ok, "add failed: {e}");

    let (ok, out, err) = run_cli(
        &mind,
        &ort,
        &[
            "relibrary",
            "--from",
            &from,
            "--to",
            &to,
            "--content-match",
            "creepidota",
        ],
    );
    assert!(ok, "relibrary dry run failed: {err}");
    assert!(
        out.contains("DRY RUN"),
        "dry run should say so, got:\n{out}"
    );
    assert!(
        out.contains("1 memory"),
        "exactly one memory should match, got:\n{out}"
    );

    // Nothing written: the destination library was never created, and the
    // source library still holds both memories.
    let list = run_cli(&mind, &ort, &["list"]).1;
    assert!(
        !list.contains(&to),
        "dry run must not create the destination library, got:\n{list}"
    );
    let browse_from = run_cli(&mind, &ort, &["browse", "--library", &from]).1;
    assert_eq!(
        count_records(&browse_from),
        2,
        "dry run must not move anything, got:\n{browse_from}"
    );

    let _ = run_cli(&mind, &ort, &["drop", &from]);
    let _ = run_cli(&mind, &ort, &["drop", &to]);
}

/// Apply moves the matching memory; an unrelated memory in the same library
/// stays put; re-running the identical command afterward matches nothing and
/// does not duplicate anything (idempotent / safely resumable).
#[test]
fn relibrary_apply_moves_memory_and_rerun_is_a_noop() {
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
    let pid = std::process::id();
    let from = format!("itrb_apply_from_{pid}");
    let to = format!("itrb_apply_to_{pid}");

    let (ok, _o, e) = run_cli(&mind, &ort, &["create", &from]);
    assert!(ok, "create failed: {e}");
    let (ok, _o, e) = run_cli(
        &mind,
        &ort,
        &["add", &from, "CreepiDota Path beta notes on matchmaking."],
    );
    assert!(ok, "add failed: {e}");
    let (ok, _o, e) = run_cli(
        &mind,
        &ort,
        &["add", &from, "Unrelated memory that must stay behind."],
    );
    assert!(ok, "add failed: {e}");

    let relibrary_args = [
        "relibrary",
        "--from",
        from.as_str(),
        "--to",
        to.as_str(),
        "--content-match",
        "creepidota",
        "--apply",
    ];

    let (ok, out, err) = run_cli(&mind, &ort, &relibrary_args);
    assert!(ok, "relibrary apply failed: {err}");
    assert!(
        out.contains("Moved 1 of 1"),
        "first apply should move exactly one, got:\n{out}"
    );

    let search_to = run_cli(
        &mind,
        &ort,
        &[
            "search",
            "creepidota matchmaking",
            "--library",
            &to,
            "--tier",
            "3",
        ],
    )
    .1;
    assert!(
        search_to.contains("matchmaking"),
        "moved memory should be searchable in '{to}', got:\n{search_to}"
    );
    let browse_from = run_cli(&mind, &ort, &["browse", "--library", &from]).1;
    assert_eq!(
        count_records(&browse_from),
        1,
        "only the unrelated memory should remain in '{from}', got:\n{browse_from}"
    );
    assert!(browse_from.contains("Unrelated memory"));

    // Re-running the identical command must be a safe no-op: the selector no
    // longer matches anything in `from` (the point already left), so nothing
    // moves and nothing duplicates.
    let (ok, out2, err2) = run_cli(&mind, &ort, &relibrary_args);
    assert!(ok, "second relibrary apply failed: {err2}");
    assert!(
        out2.contains("Moved 0 of 0"),
        "re-run must match nothing, got:\n{out2}"
    );
    let browse_to = run_cli(&mind, &ort, &["browse", "--library", &to]).1;
    assert_eq!(
        count_records(&browse_to),
        1,
        "re-running must not duplicate the moved memory, got:\n{browse_to}"
    );

    let _ = run_cli(&mind, &ort, &["drop", &from]);
    let _ = run_cli(&mind, &ort, &["drop", &to]);
}

/// A crash between the new upsert and the old delete must resume cleanly on
/// the next run, not duplicate. Simulated by re-adding the identical content
/// back into `from` after a completed move: content-addressed ids mean this
/// recreates exactly the state a crash would have left (the old point back,
/// the already-moved new point still marked with its `relibrary_source_id`).
#[test]
fn relibrary_resumes_after_a_simulated_crash_without_duplicating() {
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
    let pid = std::process::id();
    let from = format!("itrb_resume_from_{pid}");
    let to = format!("itrb_resume_to_{pid}");
    let content = "CreepiDota Path beta notes on the resume scenario.";

    let (ok, _o, e) = run_cli(&mind, &ort, &["create", &from]);
    assert!(ok, "create failed: {e}");
    let (ok, _o, e) = run_cli(&mind, &ort, &["add", &from, content, "--source", "itrb"]);
    assert!(ok, "add failed: {e}");
    let old_id = first_id(&run_cli(&mind, &ort, &["browse", "--library", &from]).1)
        .expect("original memory should have an id");

    let relibrary_args = [
        "relibrary",
        "--from",
        from.as_str(),
        "--to",
        to.as_str(),
        "--content-match",
        "resume scenario",
        "--apply",
    ];
    let (ok, out, err) = run_cli(&mind, &ort, &relibrary_args);
    assert!(ok, "first relibrary apply failed: {err}");
    assert!(out.contains("Moved 1 of 1"), "got:\n{out}");

    // Simulate the crash: the old point is back (same content -> same
    // content-addressed id as `old_id`), while the moved point at the
    // destination (carrying `relibrary_source_id = old_id`) was never touched.
    let (ok, _o, e) = run_cli(&mind, &ort, &["add", &from, content, "--source", "itrb"]);
    assert!(ok, "resurrect add failed: {e}");
    let resurrected_id = first_id(&run_cli(&mind, &ort, &["browse", "--library", &from]).1)
        .expect("resurrected memory should have an id");
    assert_eq!(
        resurrected_id, old_id,
        "content-addressed id must be identical on re-add"
    );

    // Resume: must complete (moved, not a reported collision) and must not
    // duplicate the destination.
    let (ok, out2, err2) = run_cli(&mind, &ort, &relibrary_args);
    assert!(ok, "resume apply failed: {err2}");
    assert!(
        out2.contains("Moved 1 of 1"),
        "the resumed move must complete as a move, not a collision, got:\n{out2}"
    );
    assert!(
        !out2.contains("Skipped"),
        "a resumed move must not be reported as a collision, got:\n{out2}"
    );

    let browse_from = run_cli(&mind, &ort, &["browse", "--library", &from]).1;
    assert_eq!(
        count_records(&browse_from),
        0,
        "source must be empty again after the resume, got:\n{browse_from}"
    );
    let browse_to = run_cli(&mind, &ort, &["browse", "--library", &to]).1;
    assert_eq!(
        count_records(&browse_to),
        1,
        "destination must hold exactly one copy, never a duplicate, got:\n{browse_to}"
    );

    let _ = run_cli(&mind, &ort, &["drop", &from]);
    let _ = run_cli(&mind, &ort, &["drop", &to]);
}

/// A genuine collision — an unrelated memory already occupying the id the
/// destination library would assign — is left untouched on both sides and
/// reported, not silently clobbered or duplicated.
#[test]
fn relibrary_collision_is_skipped_and_reported() {
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
    let pid = std::process::id();
    let from = format!("itrb_collide_from_{pid}");
    let to = format!("itrb_collide_to_{pid}");
    let content = "CreepiDota Path beta notes shared verbatim by coincidence.";

    let (ok, _o, e) = run_cli(&mind, &ort, &["create", &from]);
    assert!(ok, "create from failed: {e}");
    let (ok, _o, e) = run_cli(&mind, &ort, &["create", &to]);
    assert!(ok, "create to failed: {e}");

    // The destination already independently holds this exact content — NOT
    // produced by a relibrary move, so it carries no `relibrary_source_id`
    // marker pointing back at the source point.
    let (ok, _o, e) = run_cli(&mind, &ort, &["add", &to, content]);
    assert!(ok, "add to destination failed: {e}");
    let (ok, _o, e) = run_cli(&mind, &ort, &["add", &from, content]);
    assert!(ok, "add to source failed: {e}");

    let (ok, out, err) = run_cli(
        &mind,
        &ort,
        &[
            "relibrary",
            "--from",
            &from,
            "--to",
            &to,
            "--content-match",
            "coincidence",
            "--apply",
        ],
    );
    assert!(ok, "relibrary apply failed: {err}");
    assert!(
        out.contains("Skipped 1") && out.contains("collision"),
        "a genuine collision must be reported, got:\n{out}"
    );

    // Untouched on both sides: the source still has its own copy, the
    // destination was not overwritten into holding two.
    let browse_from = run_cli(&mind, &ort, &["browse", "--library", &from]).1;
    assert_eq!(
        count_records(&browse_from),
        1,
        "collision must leave the source point alone, got:\n{browse_from}"
    );
    let browse_to = run_cli(&mind, &ort, &["browse", "--library", &to]).1;
    assert_eq!(
        count_records(&browse_to),
        1,
        "collision must not duplicate the destination point, got:\n{browse_to}"
    );

    let _ = run_cli(&mind, &ort, &["drop", &from]);
    let _ = run_cli(&mind, &ort, &["drop", &to]);
}

/// A long memory is stored as several chunk points sharing one `source` tag
/// (`storage::add_memory`'s chunking). Selecting by that shared source must
/// move every chunk, and `export --format md` must still reassemble them into
/// the original text afterward — proof that `chunk_index` / `chunk_total` /
/// `chunk_overlap` survived the move untouched.
#[test]
fn relibrary_moves_every_chunk_of_a_long_document_and_export_still_reassembles_it() {
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
    let pid = std::process::id();
    let from = format!("itrb_chunk_from_{pid}");
    let to = format!("itrb_chunk_to_{pid}");
    let source_tag = format!("itrb-chunk-doc-{pid}");

    // Comfortably over CHUNK_CHARS (500) so `add` splits it into several
    // points, all sharing this one `source` tag.
    let first_marker = "ChunkStartMarkerAlpha";
    let last_marker = "ChunkEndMarkerOmega";
    let filler = "The quick brown fox jumps over the lazy dog. ".repeat(40);
    let long_content = format!("{first_marker} {filler} {last_marker}");

    let (ok, _o, e) = run_cli(&mind, &ort, &["create", &from]);
    assert!(ok, "create failed: {e}");
    let (ok, _o, e) = run_cli(
        &mind,
        &ort,
        &["add", &from, &long_content, "--source", &source_tag],
    );
    assert!(ok, "add long document failed: {e}");

    let browse_before = run_cli(&mind, &ort, &["browse", "--library", &from]).1;
    let chunk_count = count_records(&browse_before);
    assert!(
        chunk_count > 1,
        "the long document should split into multiple chunks, got:\n{browse_before}"
    );

    let (ok, out, err) = run_cli(
        &mind,
        &ort,
        &[
            "relibrary",
            "--from",
            &from,
            "--to",
            &to,
            "--source-match",
            &format!("^{source_tag}$"),
            "--apply",
        ],
    );
    assert!(ok, "relibrary apply failed: {err}");
    assert!(
        out.contains(&format!("Moved {chunk_count} of {chunk_count}")),
        "every chunk should move, got:\n{out}"
    );

    let browse_to = run_cli(&mind, &ort, &["browse", "--library", &to]).1;
    assert_eq!(
        count_records(&browse_to),
        chunk_count,
        "all chunks should now live in '{to}', got:\n{browse_to}"
    );
    let browse_from_after = run_cli(&mind, &ort, &["browse", "--library", &from]).1;
    assert_eq!(
        count_records(&browse_from_after),
        0,
        "no chunk should remain in '{from}', got:\n{browse_from_after}"
    );

    // Export reassembles by (source, created_at) + chunk_index — if those
    // fields had been dropped or reset by the move, this would come back as
    // disjoint fragments instead of the original text in order.
    let export_dir = tempfile::tempdir().expect("tempdir for export");
    let (ok, _o, e) = run_cli(
        &mind,
        &ort,
        &[
            "export",
            "--format",
            "md",
            "--output",
            export_dir.path().to_str().unwrap(),
        ],
    );
    assert!(ok, "export failed: {e}");
    let exported = std::fs::read_to_string(export_dir.path().join(format!("{to}.md")))
        .expect("export should have written a file for the destination library");
    let start_pos = exported.find(first_marker).expect("start marker present");
    let end_pos = exported.find(last_marker).expect("end marker present");
    assert!(
        start_pos < end_pos,
        "reassembled export should keep chunk order, got:\n{exported}"
    );

    let _ = run_cli(&mind, &ort, &["drop", &from]);
    let _ = run_cli(&mind, &ort, &["drop", &to]);
}

// ---------------------------------------------------------------------------
// Hidden-record leaks into read paths (atlas review task 2).
//
// Three readers used to scroll past hidden state and surface it as if it were
// current: `build_context`'s KG section checked only `valid` + `valid_until`
// (missing `quarantine_candidate`/`stale`/etc.), and `storage::history` /
// `storage::by_author` scrolled the memories collection with no visibility
// filter at all (quarantined, archived, procedure and skill points all showed
// up as "recent memories"). Each test below plants one hidden record and one
// live record in the same call, so "the hidden one is absent" can't pass on a
// query/listing that returns nothing at all.
// ---------------------------------------------------------------------------

/// `build_context`'s KG section must apply the same status-visibility rule
/// `query_facts` does. A Single-cardinality duel deterministically dampens its
/// loser to `status = stale` (same mechanism as
/// `duel_rule_dampens_loser_on_single_cardinality`) - before the fix, that fact
/// still had `valid = true` and no `valid_until`, so the old valid-only check
/// let it through into the session briefing as if it still held.
#[test]
fn build_context_hides_a_dampened_fact_but_shows_the_live_one() {
    let Some(port) = qdrant_port() else {
        eprintln!("SKIP: set MGIMIND_IT_QDRANT=<grpc port> to run integration tests");
        return;
    };

    let home = tempfile::tempdir().expect("tempdir");
    let mind = home.path().join("mgimind");
    std::fs::create_dir_all(&mind).unwrap();
    write_e5_config(&mind, &port);
    // Vectorless path (facts), same as duel_rule_dampens_loser_on_single_cardinality.
    let ort = std::env::var("ORT_DYLIB_PATH").unwrap_or_else(|_| String::from("/dev/null"));

    let pid = std::process::id();
    let duel_pred = format!("ctx_test_duel_pred_{pid}");
    let duel_subj = format!("ctx_test_duel_subj_{pid}");
    // Unregistered predicate -> Multi cardinality -> never duels -> stays Active.
    let live_pred = format!("ctx_test_live_pred_{pid}");
    let live_subj = format!("ctx_test_live_subj_{pid}");

    let input = format!(
        "{}\n{}\n{}\n{}\n{}\n{}\n",
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"mind_predicate","arguments":{
                "action":"register","predicate":duel_pred,"cardinality":"Single"}}}),
        serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
            "params":{"name":"mind_fact","arguments":{
                "action":"add","subject":duel_subj,"predicate":duel_pred,
                "object":"ctx_dampened_loser"}}}),
        serde_json::json!({"jsonrpc":"2.0","id":4,"method":"tools/call",
            "params":{"name":"mind_fact","arguments":{
                "action":"add","subject":duel_subj,"predicate":duel_pred,
                "object":"ctx_live_winner"}}}),
        serde_json::json!({"jsonrpc":"2.0","id":5,"method":"tools/call",
            "params":{"name":"mind_fact","arguments":{
                "action":"add","subject":live_subj,"predicate":live_pred,
                "object":"ctx_live_control"}}}),
    );
    let (_stdout, mcp_stderr) = run_mcp(&mind, &ort, &input);

    let (ok, out, err) = run_cli(&mind, &ort, &["context"]);
    let chain =
        format!("context out:\n{out}\n--- mcp stderr ---\n{mcp_stderr}\n--- stderr ---\n{err}");
    assert!(ok, "mgimind context must succeed: {chain}");

    // Positive controls: both the Multi-cardinality fact and the duel's winner
    // (now Active) must appear - proves a non-empty KG section is possible at
    // all, so "the loser is absent" below can't pass vacuously.
    assert!(
        out.contains(&live_subj) && out.contains("ctx_live_control"),
        "an unrelated live fact must appear in the context briefing: {chain}"
    );
    assert!(
        out.contains("ctx_live_winner"),
        "the duel winner must appear in the context briefing: {chain}"
    );
    // The dampened loser (status=stale) must be hidden, exactly like query_facts.
    assert!(
        !out.contains("ctx_dampened_loser"),
        "a dampened (stale) fact must NOT appear in the context briefing: {chain}"
    );
}

/// `storage::history` used to scroll the memories collection with no
/// visibility filter, so an archived (soft-forgotten) memory and a skill point
/// both showed up as "recent memories". It now shares `memory_query_filter_ex`
/// with `search_filtered`, so both are excluded the same way a normal search
/// already excludes them.
#[test]
fn history_hides_an_archived_memory_and_a_skill_but_shows_the_live_one() {
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
    let pid = std::process::id();
    // Two SEPARATE libraries rather than one decay-scored library: `--decay-days
    // 0` scores every point in the swept library as cold regardless of access
    // (coldness_score floors at 0.0, so `0.0 >= 0` is always true) - the existing
    // archive_then_restore test only ever checks the archived side post-sweep for
    // exactly this reason. Archiving `lib_old` WHOLESALE and never touching
    // `lib_live` with consolidate sidesteps that scoring edge case entirely.
    let lib_live = format!("ithist_live_{pid}");
    let lib_old = format!("ithist_old_{pid}");
    let live_phrase = format!("history canary live snow leopard {pid}");
    let archived_phrase = format!("history canary archived snow leopard {pid}");
    let skill_name = format!("ithist-skill-{pid}");
    let skill_marker = format!("history_skill_marker_{pid}");

    run_cli(&mind, &ort, &["create", &lib_live]);
    run_cli(&mind, &ort, &["create", &lib_old]);
    run_cli(&mind, &ort, &["add", &lib_live, &live_phrase]);
    run_cli(&mind, &ort, &["add", &lib_old, &archived_phrase]);
    let (ok, _, cerr) = run_cli(
        &mind,
        &ort,
        &[
            "consolidate",
            "--library",
            &lib_old,
            "--apply",
            "--archive-cold",
            "--decay-days",
            "0",
        ],
    );
    assert!(ok, "consolidate --archive-cold should succeed: {cerr}");
    let (ok, _, serr) = run_cli(
        &mind,
        &ort,
        &[
            "skill",
            "set",
            &skill_name,
            "--when",
            "testing the history leak",
            "--body",
            &skill_marker,
        ],
    );
    assert!(ok, "skill set should succeed: {serr}");

    let (ok, out, herr) = run_cli(&mind, &ort, &["history", "--limit", "200"]);
    assert!(ok, "mgimind history should succeed: {herr}");

    assert!(
        out.contains(&live_phrase),
        "a live memory must appear in history, got:\n{out}"
    );
    assert!(
        !out.contains(&archived_phrase),
        "an archived memory must NOT appear in history, got:\n{out}"
    );
    assert!(
        !out.contains(&skill_marker),
        "a skill point must NOT appear in history, got:\n{out}"
    );

    let _ = run_cli(&mind, &ort, &["drop", &lib_live]);
    let _ = run_cli(&mind, &ort, &["drop", &lib_old]);
}

/// `storage::by_author` excluded quarantined points but not archived ones, so
/// "what did agent X write" could still surface a soft-forgotten memory.
#[test]
fn by_author_hides_an_archived_memory_but_shows_the_live_one() {
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
    let pid = std::process::id();
    // Two SEPARATE libraries, same reason as the history test above:
    // `--decay-days 0` scores every point in the swept library as cold
    // regardless of access, so archiving `lib_old` wholesale and never
    // sweeping `lib_live` is what actually keeps one archived and one live.
    let lib_live = format!("itauthor_live_{pid}");
    let lib_old = format!("itauthor_old_{pid}");
    let agent = format!("byauthor_tester_{pid}");
    let live_phrase = format!("by-author canary live pangolin {pid}");
    let archived_phrase = format!("by-author canary archived pangolin {pid}");

    let input_add = format!(
        "{}\n{}\n{}\n{}\n{}\n{}\n",
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"mind_create","arguments":{"name":lib_live}}}),
        serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
            "params":{"name":"mind_create","arguments":{"name":lib_old}}}),
        serde_json::json!({"jsonrpc":"2.0","id":4,"method":"tools/call",
            "params":{"name":"mind_add","arguments":{
                "library":lib_live,"content":live_phrase,"agent":agent}}}),
        serde_json::json!({"jsonrpc":"2.0","id":5,"method":"tools/call",
            "params":{"name":"mind_add","arguments":{
                "library":lib_old,"content":archived_phrase,"agent":agent}}}),
    );
    run_mcp(&mind, &ort, &input_add);

    let (ok, _, cerr) = run_cli(
        &mind,
        &ort,
        &[
            "consolidate",
            "--library",
            &lib_old,
            "--apply",
            "--archive-cold",
            "--decay-days",
            "0",
        ],
    );
    assert!(ok, "consolidate --archive-cold should succeed: {cerr}");

    let by_agent_input = format!(
        "{}\n{}\n{}\n",
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"mind_by_agent","arguments":{"agent":agent,"limit":200}}}),
    );
    let (stdout, stderr) = run_mcp(&mind, &ort, &by_agent_input);
    let results = parse_mcp_results(&stdout);
    let got = results.get(&2).cloned().unwrap_or((true, String::new()));
    let chain = format!("by_agent: {got:?}\n--- stderr ---\n{stderr}");

    assert!(!got.0, "mind_by_agent must succeed: {chain}");
    assert!(
        got.1.contains(&live_phrase),
        "a live memory by this agent must appear: {chain}"
    );
    assert!(
        !got.1.contains(&archived_phrase),
        "an archived memory by this agent must NOT appear: {chain}"
    );

    let _ = run_cli(&mind, &ort, &["drop", &lib_live]);
    let _ = run_cli(&mind, &ort, &["drop", &lib_old]);
}

// ---------------------------------------------------------------------------
// Audit log gaps (atlas review task 4). Four AuditOp variants were declared in
// src/audit.rs but had no producer at all (FactAdd, ProcedureAdd,
// ProcedureOutcome, Consolidate); bulk deletes (consolidate --apply) wrote no
// audit row at all, unlike the single-delete path; and quarantine
// promote/expire hardcoded actor "relevance-gate" even for an explicit
// CLI/MCP call. Each test below performs the write, reads `audit.log`
// directly to assert the row landed with the right op/actor/content, then
// runs `mgimind audit verify` to prove the hash chain still holds with a mix
// of these new producers and the pre-existing ones.
// ---------------------------------------------------------------------------

#[test]
fn fact_add_audits_the_verdict_and_the_retired_loser() {
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
    // A plain Multi-cardinality add via the CLI, to check the "cli" actor.
    let (ok, _o, e) = run_cli(
        &mind,
        &ort,
        &[
            "fact",
            "add",
            &format!("audit_subj_{pid}"),
            &format!("audit_pred_{pid}"),
            "audit_obj_plain",
        ],
    );
    assert!(ok, "cli fact add failed: {e}");

    // A Single-cardinality duel (same deterministic mechanism as
    // duel_rule_dampens_loser_on_single_cardinality) to exercise the flip ->
    // retired-loser half of the audit event.
    let duel_pred = format!("audit_duel_pred_{pid}");
    let duel_subj = format!("audit_duel_subj_{pid}");
    let input = format!(
        "{}\n{}\n{}\n{}\n{}\n",
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"mind_predicate","arguments":{
                "action":"register","predicate":duel_pred,"cardinality":"Single"}}}),
        serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
            "params":{"name":"mind_fact","arguments":{
                "action":"add","subject":duel_subj,"predicate":duel_pred,
                "object":"audit_loser_value","agent":"alice"}}}),
        serde_json::json!({"jsonrpc":"2.0","id":4,"method":"tools/call",
            "params":{"name":"mind_fact","arguments":{
                "action":"add","subject":duel_subj,"predicate":duel_pred,
                "object":"audit_winner_value","agent":"alice"}}}),
    );
    let (_stdout, mcp_err) = run_mcp(&mind, &ort, &input);

    let log = std::fs::read_to_string(mind.join("audit.log")).unwrap_or_default();
    let chain = format!("audit.log:\n{log}\n--- mcp stderr ---\n{mcp_err}");

    assert!(
        log.contains("\"op\":\"fact_add\"") && log.contains("\"actor\":\"cli\""),
        "the cli fact add must be audited with op=fact_add, actor=cli: {chain}"
    );
    assert!(
        log.contains("audit_obj_plain"),
        "the plain fact's content must be in the audit log: {chain}"
    );
    // The flip: a line carrying the winner's content, the "won" verdict, and
    // the retired loser's value in `before`.
    let flip_line = log
        .lines()
        .find(|l| l.contains("audit_winner_value"))
        .unwrap_or_else(|| panic!("no audit line for the duel winner: {chain}"));
    assert!(
        flip_line.contains("\"op\":\"fact_add\"")
            && flip_line.contains("\"actor\":\"alice\"")
            && flip_line.contains("\"note\":\"won\""),
        "the duel winner's audit line must be op=fact_add, actor=alice, note=won: {flip_line}"
    );
    assert!(
        flip_line.contains("audit_loser_value"),
        "the flip's audit line must carry the retired loser's value in `before`: {flip_line}"
    );

    let (ok, vout, verr) = run_cli(&mind, &ort, &["audit", "verify"]);
    assert!(ok, "audit verify must pass: {vout}\n{verr}");
    assert!(
        vout.contains("audit chain intact"),
        "expected an intact chain, got: {vout}"
    );
}

#[test]
fn procedure_and_skill_writes_land_in_the_audit_log() {
    // Unlike facts, procedures and skills are embedded (dense + sparse vectors
    // on the core point), so this needs the real model, not the vectorless
    // `/dev/null` ORT path the fact-add test above can use.
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

    let pid = std::process::id();
    let err_sig = format!("AUDIT_PROC_ERR_{pid}");
    let skill_name = format!("audit-skill-{pid}");

    let input = format!(
        "{}\n{}\n{}\n{}\n",
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"mind_learn","arguments":{
                "error":err_sig,"fix":"apply the audit-test patch"}}}),
        serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
            "params":{"name":"mind_skill","arguments":{
                "action":"set","name":skill_name,"when":"testing audit writes",
                "body":"do the audit-test thing"}}}),
    );
    let (stdout, mcp_err) = run_mcp(&mind, &ort, &input);
    let results = parse_mcp_results(&stdout);
    let learn_text = results.get(&2).cloned().unwrap_or_default().1;
    let proc_id = learn_text
        .split("id: ")
        .nth(1)
        .and_then(|s| s.split(']').next())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("mind_learn must report an id: {learn_text}"));

    // Outcomes, each via their own MCP call so the id from `learn` is in hand.
    let outcome_input = format!(
        "{}\n{}\n{}\n{}\n",
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"mind_procedure_outcome","arguments":{
                "id":proc_id,"worked":true}}}),
        serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
            "params":{"name":"mind_skill","arguments":{
                "action":"outcome","name":skill_name,"worked":true}}}),
    );
    let (_out2, mcp_err2) = run_mcp(&mind, &ort, &outcome_input);

    let remove_input = format!(
        "{}\n{}\n{}\n",
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"mind_skill","arguments":{
                "action":"remove","name":skill_name}}}),
    );
    let (_out3, _mcp_err3) = run_mcp(&mind, &ort, &remove_input);

    let log = std::fs::read_to_string(mind.join("audit.log")).unwrap_or_default();
    let chain =
        format!("audit.log:\n{log}\n--- stderr 1 ---\n{mcp_err}\n--- stderr 2 ---\n{mcp_err2}");

    assert!(
        log.contains("\"op\":\"procedure_add\"")
            && log.contains("\"library\":\"_procedures\"")
            && log.contains(&err_sig),
        "mind_learn must be audited as procedure_add under _procedures: {chain}"
    );
    assert!(
        log.contains("\"op\":\"procedure_outcome\"") && log.contains(&proc_id),
        "mind_procedure_outcome must be audited as procedure_outcome: {chain}"
    );
    assert!(
        log.contains("\"library\":\"_skills\"") && log.contains(&skill_name),
        "mind_skill action=set must be audited under _skills: {chain}"
    );
    // One procedure_add (the skill) and one procedure_outcome (the skill) under
    // _skills - both share the op with procedures, distinguished by library.
    let skill_lines: Vec<&str> = log
        .lines()
        .filter(|l| l.contains("\"library\":\"_skills\""))
        .collect();
    assert!(
        skill_lines
            .iter()
            .any(|l| l.contains("\"op\":\"procedure_add\""))
            && skill_lines
                .iter()
                .any(|l| l.contains("\"op\":\"procedure_outcome\"")),
        "expected both a procedure_add and a procedure_outcome row under _skills: {chain}"
    );
    assert!(
        skill_lines.iter().any(|l| l.contains("\"op\":\"skill_remove\"")
            && l.contains("do the audit-test thing")),
        "mind_skill action=remove must be audited with the removed body: {chain}"
    );

    let (ok, vout, verr) = run_cli(&mind, &ort, &["audit", "verify"]);
    assert!(ok, "audit verify must pass: {vout}\n{verr}");
    assert!(vout.contains("audit chain intact"), "got: {vout}");
}

#[test]
fn consolidate_apply_audits_the_bulk_delete_and_its_own_summary() {
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
    let pid = std::process::id();
    let lib = format!("itauditcons_{pid}");
    let content = format!("audit consolidate cold-prune canary {pid}");

    run_cli(&mind, &ort, &["create", &lib]);
    // Never searched (access count stays 0), so `--decay-days 0` puts it in the
    // conservative binary cold-prune subset deterministically - unlike exact-dup
    // (same content in one library is the SAME content-addressed id, so two
    // `add`s of identical text can never produce two points to merge) or
    // near-dup (needs an embedding-cosine threshold, not guaranteed in a test).
    run_cli(&mind, &ort, &["add", &lib, &content]);

    let (ok, _cout, cerr) = run_cli(
        &mind,
        &ort,
        &[
            "consolidate",
            "--library",
            &lib,
            "--apply",
            "--prune-cold",
            "--decay-days",
            "0",
        ],
    );
    assert!(
        ok,
        "consolidate --apply --prune-cold should succeed: {cerr}"
    );

    let log = std::fs::read_to_string(mind.join("audit.log")).unwrap_or_default();
    let chain = format!("audit.log:\n{log}");

    assert!(
        log.contains("\"op\":\"delete\"")
            && log.contains("\"actor\":\"consolidate\"")
            && log.contains(&content),
        "the pruned cold memory must be audited as a Delete with actor=consolidate and its \
         content snapshotted: {chain}"
    );
    assert!(
        log.contains("\"op\":\"consolidate\"")
            && log.contains("\"actor\":\"consolidate\"")
            && log.contains("cold pruned"),
        "the pass itself must leave one Consolidate summary row: {chain}"
    );

    let (ok, vout, verr) = run_cli(&mind, &ort, &["audit", "verify"]);
    assert!(ok, "audit verify must pass: {vout}\n{verr}");
    assert!(vout.contains("audit chain intact"), "got: {vout}");

    let _ = run_cli(&mind, &ort, &["drop", &lib]);
}

#[test]
fn quarantine_promote_attributes_the_real_actor_not_relevance_gate() {
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
    let pid = std::process::id();
    let lib = format!("itauditquar_{pid}");
    // Deterministic quarantine trigger (too_short), no embedding-similarity
    // threshold involved, unlike the near-dup path.
    let explicit_content = format!("yq{pid}");
    let auto_content = format!("yr{pid}");

    run_cli(&mind, &ort, &["create", &lib]);
    run_cli(
        &mind,
        &ort,
        &["ingest", "--library", &lib, "--memory", &explicit_content],
    );
    run_cli(
        &mind,
        &ort,
        &["ingest", "--library", &lib, "--memory", &auto_content],
    );

    let (ok, qlist, qerr) = run_cli(&mind, &ort, &["quarantine", "list", "--library", &lib]);
    assert!(ok, "quarantine list failed: {qerr}");
    let id_for = |content: &str| -> String {
        qlist
            .split("\n\n")
            .find(|block| block.contains(content))
            .and_then(|block| block.split("id: ").nth(1))
            .and_then(|s| s.split_whitespace().next())
            .map(str::to_string)
            .unwrap_or_else(|| panic!("no quarantine entry for '{content}', got:\n{qlist}"))
    };
    let explicit_id = id_for(&explicit_content);
    let auto_id = id_for(&auto_content);

    // Explicit: a human/agent calling `quarantine promote` from a terminal.
    let (ok, _po, pe) = run_cli(&mind, &ort, &["quarantine", "promote", &explicit_id]);
    assert!(ok, "explicit promote failed: {pe}");

    // Automatic: re-submitting the SAME content via ingest is the re-assertion
    // signal that promotes it without anyone calling `quarantine promote`.
    let (ok, _io, ierr) = run_cli(
        &mind,
        &ort,
        &["ingest", "--library", &lib, "--memory", &auto_content],
    );
    assert!(ok, "re-ingest (auto-promote) failed: {ierr}");

    let log = std::fs::read_to_string(mind.join("audit.log")).unwrap_or_default();
    let chain = format!("audit.log:\n{log}");

    let explicit_line = log
        .lines()
        .find(|l| l.contains(&explicit_id) && l.contains("\"op\":\"update\""))
        .unwrap_or_else(|| panic!("no update row for the explicit promote: {chain}"));
    assert!(
        explicit_line.contains("\"actor\":\"cli\""),
        "an explicit `quarantine promote` from the CLI must be audited as actor=cli, not \
         relevance-gate: {explicit_line}"
    );

    let auto_line = log
        .lines()
        .find(|l| l.contains(&auto_id) && l.contains("\"op\":\"update\""))
        .unwrap_or_else(|| panic!("no update row for the automatic promote: {chain}"));
    assert!(
        auto_line.contains("\"actor\":\"relevance-gate\""),
        "an automatic promote-by-re-ingest must stay actor=relevance-gate: {auto_line}"
    );

    let (ok, vout, verr) = run_cli(&mind, &ort, &["audit", "verify"]);
    assert!(ok, "audit verify must pass: {vout}\n{verr}");
    assert!(vout.contains("audit chain intact"), "got: {vout}");

    let _ = run_cli(&mind, &ort, &["drop", &lib]);
}

// ---------------------------------------------------------------------------
// Fact semantics (atlas review task 5, Mad's design decisions). Facts are
// vectorless (audit #6), so these are gated only on Qdrant, like the duel
// tests above - no embedding model needed.
// ---------------------------------------------------------------------------

/// Build a full MCP stdin script: initialize + notification + one
/// `tools/call` per entry in `calls`, numbered sequentially from id 2 (so the
/// LAST call's id is `calls.len() + 1`).
fn mcp_call_script(calls: &[serde_json::Value]) -> String {
    let mut lines = vec![
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#.to_string(),
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#.to_string(),
    ];
    for (i, call) in calls.iter().enumerate() {
        let id = i as i64 + 2;
        lines.push(
            serde_json::json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":call})
                .to_string(),
        );
    }
    lines.join("\n") + "\n"
}

/// §5a: re-asserting the same triple raises its own `confirmations_count`,
/// which `entrenchment()` already had a term for but the live path never fed
/// (the atlas review's "entrenchment is effectively age only" finding). A
/// value confirmed 15 times must resist a single fresh contradiction
/// (CONTESTED, not a flip); an unconfirmed young fact on a fresh axis still
/// flips exactly as before (the positive control).
#[test]
fn confirmations_raise_entrenchment_against_a_single_contradiction() {
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
    let pred = format!("confirm_pred_{pid}");
    let subj = format!("confirm_subj_{pid}");

    let mut calls = vec![serde_json::json!({"name":"mind_predicate","arguments":{
        "action":"register","predicate":pred,"cardinality":"Single"}})];
    // 15 re-assertions of the SAME triple: each is a plain re-assert (no
    // conflict, since it is the only valid fact on the axis so far), so
    // confirmations climb 1..15 with no duel running in between.
    for _ in 0..15 {
        calls.push(serde_json::json!({"name":"mind_fact","arguments":{
            "action":"add","subject":subj,"predicate":pred,"object":"confirmed_value"}}));
    }
    // The contradiction: a brand-new, first-mention challenger.
    calls.push(serde_json::json!({"name":"mind_fact","arguments":{
        "action":"add","subject":subj,"predicate":pred,"object":"challenger_value"}}));
    let input = mcp_call_script(&calls);
    let (stdout, stderr) = run_mcp(&mind, &ort, &input);
    let results = parse_mcp_results(&stdout);
    let last_id = calls.len() as i64 + 1;
    let chain = format!("stdout parsed: {results:?}\n--- stderr ---\n{stderr}");
    let last = results
        .get(&last_id)
        .cloned()
        .unwrap_or_else(|| panic!("no result for the contradiction: {chain}"));
    assert!(!last.0, "the contradicting add must succeed: {chain}");
    assert!(
        last.1.contains("contested"),
        "a fact confirmed 15 times must end CONTESTED against a single fresh contradiction, \
         not flip: {last:?}\n{chain}"
    );

    // Positive control: an UNCONFIRMED young fact on a fresh axis still flips
    // exactly as before - the new behaviour only raises the bar for a
    // CONFIRMED fact, it does not make every Single axis sticky.
    let baseline_pred = format!("confirm_baseline_pred_{pid}");
    let baseline_subj = format!("confirm_baseline_subj_{pid}");
    let baseline_calls = vec![
        serde_json::json!({"name":"mind_predicate","arguments":{
            "action":"register","predicate":baseline_pred,"cardinality":"Single"}}),
        serde_json::json!({"name":"mind_fact","arguments":{
            "action":"add","subject":baseline_subj,"predicate":baseline_pred,"object":"old_value"}}),
        serde_json::json!({"name":"mind_fact","arguments":{
            "action":"add","subject":baseline_subj,"predicate":baseline_pred,"object":"new_value"}}),
    ];
    let (stdout2, stderr2) = run_mcp(&mind, &ort, &mcp_call_script(&baseline_calls));
    let results2 = parse_mcp_results(&stdout2);
    let flip = results2.get(&4).cloned().unwrap_or_default();
    assert!(
        flip.1.contains("won"),
        "an unconfirmed young fact must still flip a single fresh contradiction, got: \
         {flip:?}\n--- stderr ---\n{stderr2}"
    );
}

/// §5b: explicit invalidate is final. Re-adding the same triple must refuse
/// (verdict `invalidated`), not silently revive it; only the explicit restore
/// path brings it back.
#[test]
fn explicit_invalidate_is_final_until_restored() {
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
    let subj = format!("inval_subj_{pid}");
    let pred = format!("inval_pred_{pid}");
    let obj = "inval_object";

    let (ok, out, err) = run_cli(&mind, &ort, &["fact", "add", &subj, &pred, obj]);
    assert!(ok, "fact add failed: {err}");
    let id = out
        .split("id: ")
        .nth(1)
        .and_then(|s| s.split(']').next())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("fact add must report an id, got: {out}"));

    let (ok, _o, e) = run_cli(&mind, &ort, &["fact", "invalidate", &id]);
    assert!(ok, "fact invalidate failed: {e}");
    let (ok, query_out, _e) = run_cli(&mind, &ort, &["fact", "query", &subj]);
    assert!(ok);
    assert!(
        !query_out.contains(obj),
        "an invalidated fact must not appear in a plain query: {query_out}"
    );

    // Re-adding the SAME triple must refuse, not revive it.
    let (ok, readd_out, e) = run_cli(&mind, &ort, &["fact", "add", &subj, &pred, obj]);
    assert!(ok, "fact add (re-add) should still exit 0: {e}");
    assert!(
        readd_out.contains("NOT added") && readd_out.contains("invalidated"),
        "re-adding an invalidated triple must refuse, not revive it: {readd_out}"
    );
    let (ok, query_out2, _e) = run_cli(&mind, &ort, &["fact", "query", &subj]);
    assert!(ok);
    assert!(
        !query_out2.contains(obj),
        "it must STILL be absent after the refused re-add: {query_out2}"
    );

    // The explicit restore brings it back.
    let (ok, restore_out, e) = run_cli(&mind, &ort, &["fact", "restore", &id]);
    assert!(ok, "fact restore failed: {e}");
    assert!(restore_out.contains("restored"), "got: {restore_out}");
    let (ok, query_out3, _e) = run_cli(&mind, &ort, &["fact", "query", &subj]);
    assert!(ok);
    assert!(
        query_out3.contains(obj),
        "after an explicit restore the fact must be visible again: {query_out3}"
    );
}

/// §5b (second half): re-adding a `stale` duel loser is a FAIR NEW DUEL, not
/// a blank-slate revive - it keeps its original `created_at` and its own
/// confirmation history (so a value that was briefly entrenched before losing
/// isn't treated as a bare first mention when it comes back).
#[test]
fn readding_a_stale_loser_is_a_fresh_duel_that_keeps_its_created_at() {
    let Some(port) = qdrant_port() else {
        eprintln!("SKIP: set MGIMIND_IT_QDRANT=<grpc port> to run integration tests");
        return;
    };
    let home = tempfile::tempdir().expect("tempdir");
    let mind = home.path().join("mgimind");
    std::fs::create_dir_all(&mind).unwrap();
    write_e5_config(&mind, &port);
    let ort = std::env::var("ORT_DYLIB_PATH").unwrap_or_else(|_| String::from("/dev/null"));

    // This test is about the fair re-duel and created_at, not the flip-back
    // damping (covered by `flip_back_within_window_lands_contested`), and it
    // re-adds the loser seconds after it lost, so the damping is turned off.
    set_config_key(&mind, "flip_back_window_hours", serde_json::json!(0));

    let pid = std::process::id();
    let pred = format!("restale_pred_{pid}");
    let subj = format!("restale_subj_{pid}");

    // Cardinality registration is MCP-only (no CLI mirror); the registry is
    // durable in storage, so a one-shot MCP call followed by plain CLI calls
    // in separate processes is fine.
    let (_so, se) = run_mcp(
        &mind,
        &ort,
        &mcp_call_script(&[serde_json::json!({"name":"mind_predicate","arguments":{
            "action":"register","predicate":pred,"cardinality":"Single"}})]),
    );

    let (ok, _o, e) = run_cli(
        &mind,
        &ort,
        &["fact", "add", &subj, &pred, "old_loser_value"],
    );
    assert!(
        ok,
        "first add failed: {e}\n--- predicate register stderr ---\n{se}"
    );
    let extract_from = |out: &str| -> String {
        out.lines()
            .find(|l| l.trim_start().starts_with("valid:"))
            .and_then(|l| l.split("valid: ").nth(1))
            .and_then(|s| s.split(" ..").next())
            .map(str::to_string)
            .unwrap_or_else(|| panic!("expected a 'valid: <from> ..' line, got:\n{out}"))
    };
    let (ok, q1, _e) = run_cli(&mind, &ort, &["fact", "query", &subj]);
    assert!(ok);
    let original_from = extract_from(&q1);

    // A contradiction flips it to stale.
    let (ok, _o, e) = run_cli(
        &mind,
        &ort,
        &["fact", "add", &subj, &pred, "new_winner_value"],
    );
    assert!(ok, "flip add failed: {e}");
    let (ok, q2, _e) = run_cli(&mind, &ort, &["fact", "query", &subj]);
    assert!(ok);
    assert!(
        !q2.contains("old_loser_value"),
        "the loser must be hidden from a plain query once stale: {q2}"
    );

    // Entrench the new winner (6 re-asserts on top of its first write) so the
    // comparison below is unambiguous regardless of entrenchment tie-order.
    for _ in 0..6 {
        let (ok, _o, e) = run_cli(
            &mind,
            &ort,
            &["fact", "add", &subj, &pred, "new_winner_value"],
        );
        assert!(ok, "re-assert of the winner failed: {e}");
    }

    // Re-propose the stale loser: a fresh duel, not a blank-slate revive. Its
    // own carried-forward confirmations (1 -> 2) comfortably beat the winner's
    // entrenchment at 7 confirmations.
    let (ok, readd_out, e) = run_cli(
        &mind,
        &ort,
        &["fact", "add", &subj, &pred, "old_loser_value"],
    );
    assert!(ok, "re-add of the stale loser failed: {e}");
    assert!(
        readd_out.contains("won"),
        "the re-proposed loser must win back: {readd_out}"
    );
    let (ok, q3, _e) = run_cli(&mind, &ort, &["fact", "query", &subj]);
    assert!(ok);
    assert!(
        q3.contains("old_loser_value"),
        "the re-won value must be visible again: {q3}"
    );
    let new_from = extract_from(&q3);
    assert_eq!(
        original_from, new_from,
        "created_at must be preserved across the stale -> re-won round trip, not reset"
    );
}

/// §5c: `doubt::mark_inherited` is wired into `build_context`'s KG section -
/// every fact it surfaces is marked inherited in THIS process. A duel
/// challenger that is merely being re-proposed from that inherited context
/// (not independently re-observed live) counts at the discounted weight.
/// Compares two identical axes, one shown via `mind_context` first and one
/// never shown: only the inherited one ends CONTESTED instead of flipping.
///
/// `is_inherited`/`mark_inherited` live in a per-process registry by design
/// (a property of this session's view of a fact, not of the fact itself), so
/// the mark and the duel it discounts must happen in the SAME `mgimind mcp`
/// process - everything for one axis runs in one `run_mcp` call.
#[test]
fn inherited_fact_discounts_the_duel_challenger_weight() {
    let Some(port) = qdrant_port() else {
        eprintln!("SKIP: set MGIMIND_IT_QDRANT=<grpc port> to run integration tests");
        return;
    };
    let home = tempfile::tempdir().expect("tempdir");
    let mind = home.path().join("mgimind");
    std::fs::create_dir_all(&mind).unwrap();
    write_e5_config(&mind, &port);
    let ort = std::env::var("ORT_DYLIB_PATH").unwrap_or_else(|_| String::from("/dev/null"));
    // The control re-proposes a loser seconds after it lost; with flip-back
    // damping on, both axes would contest and the discount would be invisible.
    set_config_key(&mind, "flip_back_window_hours", serde_json::json!(0));
    let pid = std::process::id();

    let run_axis = |pred: &str, subj: &str, mark_via_context: bool| -> String {
        let mut calls = vec![
            serde_json::json!({"name":"mind_predicate","arguments":{
                "action":"register","predicate":pred,"cardinality":"Single"}}),
            serde_json::json!({"name":"mind_fact","arguments":{
                "action":"add","subject":subj,"predicate":pred,"object":"value_A"}}),
        ];
        if mark_via_context {
            // Surfaces value_A in the KG briefing, marking its id inherited.
            calls.push(serde_json::json!({"name":"mind_context","arguments":{}}));
        }
        // value_B contradicts and wins (value_A had only 1 confirmation).
        calls.push(serde_json::json!({"name":"mind_fact","arguments":{
            "action":"add","subject":subj,"predicate":pred,"object":"value_B"}}));
        // Entrench value_B (6 more re-asserts -> 7 confirmations total) so
        // re-proposing value_A is a real contest, not a free win either way.
        for _ in 0..6 {
            calls.push(serde_json::json!({"name":"mind_fact","arguments":{
                "action":"add","subject":subj,"predicate":pred,"object":"value_B"}}));
        }
        // The critical call: re-propose value_A, whose id is still marked
        // inherited (nothing re-asserted it live in between).
        calls.push(serde_json::json!({"name":"mind_fact","arguments":{
            "action":"add","subject":subj,"predicate":pred,"object":"value_A"}}));
        let input = mcp_call_script(&calls);
        let (stdout, stderr) = run_mcp(&mind, &ort, &input);
        let results = parse_mcp_results(&stdout);
        let last_id = calls.len() as i64 + 1;
        results
            .get(&last_id)
            .cloned()
            .unwrap_or_else(|| panic!("no result for the final re-add\n--- stderr ---\n{stderr}"))
            .1
    };

    let control_result = run_axis(
        &format!("inh_ctrl_pred_{pid}"),
        &format!("inh_ctrl_subj_{pid}"),
        false,
    );
    assert!(
        control_result.contains("won"),
        "control (never shown via a briefing): re-proposing the old value against an \
         entrenched contradictor must still flip, got: {control_result}"
    );

    let inherited_result = run_axis(&format!("inh_pred_{pid}"), &format!("inh_subj_{pid}"), true);
    assert!(
        inherited_result.contains("contested"),
        "inherited (surfaced by mind_context, then re-proposed unchanged): the SAME \
         re-proposal must be discounted enough to only contest, not flip, got: \
         {inherited_result}"
    );
}

/// §5d: optional world-time `valid_from` axis. `as_of` in record-time mode
/// (default, unchanged) uses `created_at`/`valid_until` (when the store
/// learned/retired a value); world-time mode uses `valid_from`/`valid_until`
/// (when it became true/retired in the world) instead.
#[test]
fn world_time_as_of_differs_from_record_time_as_of() {
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
    let pred = format!("world_pred_{pid}");
    let subj = format!("world_subj_{pid}");

    let calls = vec![
        serde_json::json!({"name":"mind_predicate","arguments":{
            "action":"register","predicate":pred,"cardinality":"TemporalSingle"}}),
        serde_json::json!({"name":"mind_fact","arguments":{
            "action":"add","subject":subj,"predicate":pred,"object":"value_2020",
            "valid_from":"2020-01-01"}}),
        serde_json::json!({"name":"mind_fact","arguments":{
            "action":"add","subject":subj,"predicate":pred,"object":"value_2023",
            "valid_from":"2023-01-01"}}),
        serde_json::json!({"name":"mind_fact","arguments":{
            "action":"query","subject":subj,"as_of":"2020-06-01"}}),
        serde_json::json!({"name":"mind_fact","arguments":{
            "action":"query","subject":subj,"as_of":"2020-06-01","world_time":true}}),
        serde_json::json!({"name":"mind_fact","arguments":{
            "action":"query","subject":subj,"as_of":"2024-06-01","world_time":true}}),
    ];
    let input = mcp_call_script(&calls);
    let (stdout, stderr) = run_mcp(&mind, &ort, &input);
    let results = parse_mcp_results(&stdout);
    let chain = format!("stdout parsed: {results:?}\n--- stderr ---\n{stderr}");

    let record_time = results.get(&5).cloned().unwrap_or_default();
    assert!(!record_time.0, "record-time query must succeed: {chain}");
    assert!(
        !record_time.1.contains("value_2020") && !record_time.1.contains("value_2023"),
        "record-time as_of must find nothing at 2020-06-01 - the store only learned about \
         this axis today: {record_time:?}\n{chain}"
    );

    let world_time = results.get(&6).cloned().unwrap_or_default();
    assert!(!world_time.0, "world-time query must succeed: {chain}");
    assert!(
        world_time.1.contains("value_2020"),
        "world-time as_of must find the fact whose valid_from covers 2020-06-01: \
         {world_time:?}\n{chain}"
    );
    assert!(
        !world_time.1.contains("value_2023"),
        "the later value (valid_from 2023) must not appear yet at 2020-06-01: \
         {world_time:?}\n{chain}"
    );

    // World-time end: value_2023 (valid_from 2023) displaced value_2020, so
    // value_2020's world end is 2023-01-01, not the moment the store recorded
    // the successor. At 2024 only the successor holds; no overlap.
    let world_2024 = results.get(&7).cloned().unwrap_or_default();
    assert!(
        !world_2024.0,
        "world-time query at 2024 must succeed: {chain}"
    );
    assert!(
        world_2024.1.contains("value_2023") && !world_2024.1.contains("value_2020"),
        "at 2024-06-01 only the successor may hold on the world axis: {world_2024:?}\n{chain}"
    );
}

/// Flip-back damping: a value that lost a duel and is re-asserted within
/// `flip_back_window_hours` (24 by default) lands `contested` instead of
/// flipping the winner back, so two values cannot trade places on every write.
/// Both stay visible; the winner is not retired.
#[test]
fn flip_back_within_window_lands_contested() {
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
    let pred = format!("pingpong_pred_{pid}");
    let subj = format!("pingpong_subj_{pid}");

    let calls = vec![
        serde_json::json!({"name":"mind_predicate","arguments":{
            "action":"register","predicate":pred,"cardinality":"Single"}}),
        serde_json::json!({"name":"mind_fact","arguments":{
            "action":"add","subject":subj,"predicate":pred,"object":"value_A"}}),
        serde_json::json!({"name":"mind_fact","arguments":{
            "action":"add","subject":subj,"predicate":pred,"object":"value_B"}}),
        serde_json::json!({"name":"mind_fact","arguments":{
            "action":"add","subject":subj,"predicate":pred,"object":"value_A"}}),
        serde_json::json!({"name":"mind_fact","arguments":{
            "action":"query","subject":subj}}),
        // A fourth write of value_A while it is still contested.
        serde_json::json!({"name":"mind_fact","arguments":{
            "action":"add","subject":subj,"predicate":pred,"object":"value_A"}}),
        serde_json::json!({"name":"mind_fact","arguments":{
            "action":"query","subject":subj}}),
    ];
    let (stdout, stderr) = run_mcp(&mind, &ort, &mcp_call_script(&calls));
    let results = parse_mcp_results(&stdout);
    let chain = format!("stdout parsed: {results:?}\n--- stderr ---\n{stderr}");

    // The challenger's own contested row must never be its rival: before the
    // fix the fourth write named value_A's own row the loser, upserted it
    // active, retired it and reported "won", leaving only value_B.
    let again = results.get(&7).cloned().unwrap_or_default().1;
    let after = results.get(&8).cloned().unwrap_or_default().1;
    assert!(
        after.contains("value_A"),
        "a re-asserted value must not retire its own row: {again}\n{after}\n{chain}"
    );
    if again.contains("won") {
        assert!(
            !after.contains("value_B"),
            "a won verdict must retire the rival, not the challenger: {after}\n{chain}"
        );
    }

    let flip = results.get(&4).cloned().unwrap_or_default().1;
    assert!(
        flip.contains("won"),
        "value_B must first win against a young value_A: {chain}"
    );
    let back = results.get(&5).cloned().unwrap_or_default().1;
    assert!(
        back.contains("contested") && !back.contains("won"),
        "re-asserting the loser seconds later must contest, not flip back: {chain}"
    );
    let q = results.get(&6).cloned().unwrap_or_default().1;
    assert!(
        q.contains("value_A") && q.contains("value_B"),
        "both values must stay visible while contested: {chain}"
    );
}

/// The tombstone is read loosely: a re-assertion that differs from an
/// explicitly invalidated triple only in case or whitespace is refused too,
/// and the refusal names the invalidated row's id so it can be restored.
#[test]
fn invalidated_triple_refuses_case_and_whitespace_variants() {
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
    let subj = format!("tomb_subj_{pid}");
    let pred = format!("tomb_pred_{pid}");

    let (ok, out, err) = run_cli(&mind, &ort, &["fact", "add", &subj, &pred, "Berlin"]);
    assert!(ok, "fact add failed: {err}");
    let id = out
        .split("id: ")
        .nth(1)
        .and_then(|s| s.split(']').next())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("fact add must report an id, got: {out}"));
    let (ok, _o, e) = run_cli(&mind, &ort, &["fact", "invalidate", &id]);
    assert!(ok, "fact invalidate failed: {e}");

    let variant_subj = format!("  {} ", subj.to_uppercase());
    let (ok, readd, e) = run_cli(
        &mind,
        &ort,
        &["fact", "add", &variant_subj, &pred, "berlin  "],
    );
    assert!(ok, "variant add should still exit 0: {e}");
    assert!(
        readd.contains("NOT added") && readd.contains("invalidated") && readd.contains(&id),
        "a case/whitespace variant of an invalidated triple must be refused, naming its id: {readd}"
    );
    let (ok, q, _e) = run_cli(&mind, &ort, &["fact", "query", &subj]);
    assert!(ok);
    assert!(
        !q.to_lowercase().contains("berlin"),
        "nothing may be revived: {q}"
    );

    // A genuinely different object is unaffected.
    let (ok, other, e) = run_cli(&mind, &ort, &["fact", "add", &subj, &pred, "Berlin-Mitte"]);
    assert!(ok, "different object add failed: {e}");
    assert!(
        !other.contains("NOT added"),
        "a different value must be accepted: {other}"
    );
}

/// Pinned-block writes are audited: set (with the prior content on overwrite)
/// and remove (with the content removed).
#[test]
fn pinned_block_writes_land_in_the_audit_log() {
    let Some(port) = qdrant_port() else {
        eprintln!("SKIP: set MGIMIND_IT_QDRANT=<grpc port> to run integration tests");
        return;
    };
    let home = tempfile::tempdir().expect("tempdir");
    let mind = home.path().join("mgimind");
    std::fs::create_dir_all(&mind).unwrap();
    write_e5_config(&mind, &port);
    let ort = std::env::var("ORT_DYLIB_PATH").unwrap_or_else(|_| String::from("/dev/null"));

    let calls = vec![
        serde_json::json!({"name":"mind_block","arguments":{
            "action":"set","name":"audit-block","content":"first text"}}),
        serde_json::json!({"name":"mind_block","arguments":{
            "action":"set","name":"audit-block","content":"second text"}}),
        serde_json::json!({"name":"mind_block","arguments":{
            "action":"remove","name":"audit-block"}}),
    ];
    let (_stdout, stderr) = run_mcp(&mind, &ort, &mcp_call_script(&calls));
    let log = std::fs::read_to_string(mind.join("audit.log")).unwrap_or_default();
    let chain = format!("audit.log:\n{log}\n--- stderr ---\n{stderr}");
    let sets: Vec<&str> = log
        .lines()
        .filter(|l| l.contains("\"op\":\"block_set\"") && l.contains("audit-block"))
        .collect();
    assert_eq!(sets.len(), 2, "two block_set rows expected: {chain}");
    assert!(
        sets[1].contains("first text") && sets[1].contains("second text"),
        "the overwrite must carry the prior and the new content: {chain}"
    );
    assert!(
        log.lines().any(|l| l.contains("\"op\":\"block_remove\"")
            && l.contains("audit-block")
            && l.contains("second text")),
        "block remove must be audited with the removed content: {chain}"
    );
    let (ok, vout, verr) = run_cli(&mind, &ort, &["audit", "verify"]);
    assert!(ok && vout.contains("audit chain intact"), "{vout}\n{verr}");
}

/// Glama tool-definition-quality fix: the 15 deprecated v1.0 singleton
/// aliases (mind_vault_store and its siblings) must be hidden from
/// `tools/list` by default (score penalty for a bloated, half-duplicate
/// surface) but keep dispatching exactly as before for any script that calls
/// them directly without listing first. `MGIMIND_LIST_DEPRECATED=1` restores
/// the full listing for a client that wants to see (and migrate off) them.
/// mind_vault_store itself touches no storage, but the MCP server tries to
/// auto-start Qdrant on launch regardless of which tool gets called; pointing
/// config at the already-running test instance (same pattern as the other
/// vectorless tests above) keeps that a no-op instead of a slow spawn attempt
/// against a missing binary or a port collision with a real Qdrant.
#[test]
fn deprecated_aliases_hidden_by_default_but_still_dispatch() {
    let Some(port) = qdrant_port() else {
        eprintln!("SKIP: set MGIMIND_IT_QDRANT=<grpc port> to run integration tests");
        return;
    };
    let home = tempfile::tempdir().expect("tempdir");
    let mind = home.path().join("mgimind");
    std::fs::create_dir_all(&mind).unwrap();
    write_e5_config(&mind, &port);
    let ort = std::env::var("ORT_DYLIB_PATH").unwrap_or_else(|_| String::from("/dev/null"));

    let input = format!(
        "{}\n{}\n{}\n{}\n",
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
            "params":{"name":"mind_vault_store","arguments":{"key":"itest-hidden-alias"}}}),
    );

    use std::io::Write;

    // Default: MGIMIND_LIST_DEPRECATED unset (explicitly removed in case the
    // launching shell has it set, so this test is not at the mercy of the
    // ambient environment).
    let mut child = Command::new(bin())
        .arg("mcp")
        .env("MGIMIND_HOME", &mind)
        .env("ORT_DYLIB_PATH", &ort)
        .env_remove("MGIMIND_LIST_DEPRECATED")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn mgimind mcp");
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(input.as_bytes())
        .expect("write requests");
    let out = child.wait_with_output().expect("wait mcp");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let chain = format!("stdout: {stdout}\n--- stderr ---\n{stderr}");

    let responses: Vec<serde_json::Value> = stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("non-JSON on stdout: {e}\n{l}")))
        .collect();
    let tools_list = responses
        .iter()
        .find(|v| v["id"] == 2)
        .unwrap_or_else(|| panic!("no tools/list response: {chain}"));
    let tools = tools_list["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools/list result has no tools array: {chain}"));
    assert!(
        !tools.iter().any(|t| t["name"] == "mind_vault_store"),
        "mind_vault_store must be hidden from tools/list by default: {chain}"
    );
    assert_eq!(
        tools.len(),
        30,
        "default listing must be the 30-tool surface (aliases hidden): {chain}"
    );

    let call_resp = responses
        .iter()
        .find(|v| v["id"] == 3)
        .unwrap_or_else(|| panic!("no tools/call response: {chain}"));
    assert_eq!(
        call_resp["result"]["isError"], false,
        "a hidden alias must still dispatch successfully: {chain}"
    );
    assert!(
        call_resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .contains("terminal"),
        "mind_vault_store must still return its terminal-only instructions: {chain}"
    );

    // MGIMIND_LIST_DEPRECATED=1: the alias reappears in tools/list.
    let list_only = format!(
        "{}\n{}\n{}\n",
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
    );
    let mut child2 = Command::new(bin())
        .arg("mcp")
        .env("MGIMIND_HOME", &mind)
        .env("ORT_DYLIB_PATH", &ort)
        .env("MGIMIND_LIST_DEPRECATED", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn mgimind mcp (MGIMIND_LIST_DEPRECATED=1)");
    child2
        .stdin
        .take()
        .expect("child stdin")
        .write_all(list_only.as_bytes())
        .expect("write requests");
    let out2 = child2.wait_with_output().expect("wait mcp 2");
    let stdout2 = String::from_utf8_lossy(&out2.stdout).into_owned();
    let stderr2 = String::from_utf8_lossy(&out2.stderr).into_owned();
    let chain2 = format!("stdout: {stdout2}\n--- stderr ---\n{stderr2}");

    let responses2: Vec<serde_json::Value> = stdout2
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("non-JSON on stdout: {e}\n{l}")))
        .collect();
    let resp2 = responses2
        .iter()
        .find(|v| v["id"] == 2)
        .unwrap_or_else(|| panic!("no tools/list response: {chain2}"));
    let tools2 = resp2["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools/list result has no tools array: {chain2}"));
    assert!(
        tools2.iter().any(|t| t["name"] == "mind_vault_store"),
        "MGIMIND_LIST_DEPRECATED=1 must restore mind_vault_store to the listing: {chain2}"
    );
    assert_eq!(
        tools2.len(),
        45,
        "MGIMIND_LIST_DEPRECATED=1 must restore the full 45-tool surface: {chain2}"
    );
}

/// Raw HTTP/1.1 request against a local viewer; returns (status, body).
fn http_req(port: u16, method: &str, path: &str, token: &str, body: Option<&str>) -> (u16, String) {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect viewer");
    let body = body.unwrap_or("");
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let status = out
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let body = out.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    (status, body)
}

/// Human review: a contradiction the duel cannot settle waits on a person,
/// and the agent that wrote it cannot answer it. The agent surfaces can list
/// and ask; `mgimind review accept` refuses without a terminal; invalidate
/// refuses a fact under review; the viewer refuses an answer without the
/// person's PIN and accepts one with it. The answer is final: the rejected
/// value is refused when an agent writes it again.
#[test]
fn contested_fact_waits_on_a_person() {
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
    let pred = format!("review_pred_{pid}");
    let subj = format!("review_subj_{pid}");

    // A, B (B wins), A again inside the flip-back window: contested, so it
    // waits on a person.
    let calls = vec![
        serde_json::json!({"name":"mind_predicate","arguments":{
            "action":"register","predicate":pred,"cardinality":"Single"}}),
        serde_json::json!({"name":"mind_fact","arguments":{
            "action":"add","subject":subj,"predicate":pred,"object":"value_A"}}),
        serde_json::json!({"name":"mind_fact","arguments":{
            "action":"add","subject":subj,"predicate":pred,"object":"value_B"}}),
        serde_json::json!({"name":"mind_fact","arguments":{
            "action":"add","subject":subj,"predicate":pred,"object":"value_A"}}),
        serde_json::json!({"name":"mind_review","arguments":{"action":"list"}}),
        serde_json::json!({"name":"mind_fact","arguments":{
            "action":"query","subject":subj}}),
    ];
    let (stdout, stderr) = run_mcp(&mind, &ort, &mcp_call_script(&calls));
    let r = parse_mcp_results(&stdout);
    let chain = format!("stdout parsed: {r:?}\n--- stderr ---\n{stderr}");
    let back = r.get(&5).cloned().unwrap_or_default().1;
    assert!(
        back.contains("contested"),
        "third write must contest: {chain}"
    );
    let a_id = back
        .split("id: ")
        .nth(1)
        .and_then(|s| s.split(']').next())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("no id in: {back}"));
    let listed = r.get(&6).cloned().unwrap_or_default().1;
    assert!(
        listed.contains("value_A") && listed.contains("value_B") && listed.contains(&a_id),
        "the queue must show the question with its rival: {chain}"
    );
    let q = r.get(&7).cloned().unwrap_or_default().1;
    assert!(
        q.contains("waiting on the user's review"),
        "a disputed value must be labelled in query output: {chain}"
    );

    // The agent holds no answer verb: invalidating the held fact is refused.
    let calls = vec![serde_json::json!({"name":"mind_fact","arguments":{
        "action":"invalidate","id":a_id}})];
    let (stdout, _) = run_mcp(&mind, &ort, &mcp_call_script(&calls));
    let inv = parse_mcp_results(&stdout)
        .get(&2)
        .cloned()
        .unwrap_or_default();
    assert!(
        inv.1.contains("human review"),
        "invalidate must refuse a fact under review: {inv:?}"
    );

    // The terminal path refuses without a terminal (this test pipes stdio, the
    // same way an agent's shell tool does).
    let (ok, _o, e) = run_cli(&mind, &ort, &["review", "accept", &a_id]);
    assert!(
        !ok && e.contains("terminal"),
        "review accept must need a TTY: {e}"
    );
    let (ok, _o, e) = run_cli(&mind, &ort, &["review", "pin"]);
    assert!(
        !ok && e.contains("terminal"),
        "review pin must need a TTY: {e}"
    );

    // The person's PIN, as `mgimind review pin` would store it.
    {
        use argon2::password_hash::{PasswordHasher, SaltString, rand_core::OsRng};
        let salt = SaltString::generate(&mut OsRng);
        let hash = argon2::Argon2::default()
            .hash_password(b"correct-pin", &salt)
            .unwrap()
            .to_string();
        std::fs::write(mind.join("review_pin"), hash).unwrap();
    }
    let vport = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let token = format!("tok{pid}");
    let mut viewer = Command::new(bin())
        .args([
            "viewer",
            "--no-open",
            "--port",
            &vport.to_string(),
            "--token",
            &token,
        ])
        .env("MGIMIND_HOME", &mind)
        .env("ORT_DYLIB_PATH", &ort)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn viewer");
    let mut up = false;
    for _ in 0..100 {
        if std::net::TcpStream::connect(("127.0.0.1", vport)).is_ok() {
            up = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(up, "viewer did not start");
    let path = format!("/api/review/{a_id}");
    let (st_token_only, _) = http_req(
        vport,
        "POST",
        &path,
        &token,
        Some(r#"{"answer":"accept","pin":"wrong"}"#),
    );
    let (st_ok, ok_body) = http_req(
        vport,
        "POST",
        &path,
        &token,
        Some(r#"{"answer":"accept","pin":"correct-pin"}"#),
    );
    let (_, queue) = http_req(vport, "GET", "/api/review", &token, None);
    let _ = viewer.kill();
    let _ = viewer.wait();
    assert_eq!(
        st_token_only, 403,
        "the viewer token alone must not answer a review"
    );
    assert_eq!(st_ok, 200, "the right PIN must answer: {ok_body}");
    assert!(
        ok_body.contains("accepted"),
        "the answer must be recorded: {ok_body}"
    );
    // The Qdrant is shared with other tests, so check only this axis.
    assert!(
        !queue.contains(&subj),
        "this question must leave the queue once answered: {queue}"
    );

    // The answer stands: A is confirmed, B is retired, and B written again by
    // an agent is refused as rejected.
    let calls = vec![
        serde_json::json!({"name":"mind_fact","arguments":{
            "action":"query","subject":subj}}),
        serde_json::json!({"name":"mind_fact","arguments":{
            "action":"add","subject":subj,"predicate":pred,"object":"value_B"}}),
    ];
    let (stdout, stderr) = run_mcp(&mind, &ort, &mcp_call_script(&calls));
    let r = parse_mcp_results(&stdout);
    let chain = format!("stdout parsed: {r:?}\n--- stderr ---\n{stderr}");
    let q = r.get(&2).cloned().unwrap_or_default().1;
    assert!(
        q.contains("value_A") && q.contains("confirmed by the user") && !q.contains("value_B"),
        "after the answer only the accepted value holds: {chain}"
    );
    let again = r.get(&3).cloned().unwrap_or_default().1;
    assert!(
        again.contains("rejected"),
        "a rejected triple must be refused: {chain}"
    );

    // The audit log names the human surface.
    let log = std::fs::read_to_string(mind.join("audit.log")).unwrap_or_default();
    assert!(
        log.contains("human:viewer-pin"),
        "the answer must be audited with its surface"
    );
}
