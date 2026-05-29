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

    let home = tempfile::tempdir().expect("tempdir");
    let mind = home.path().join("mgimind");
    std::fs::create_dir_all(mind.join("models")).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&model_src, mind.join("models/multilingual-e5-base")).unwrap();
    std::fs::write(
        mind.join("config.json"),
        format!(
            r#"{{"version":"it","data_dir":"{}","model_name":"multilingual-e5-base","qdrant_port":{},"vector_size":768,"pooling":"mean","uses_token_type_ids":false,"query_prefix":"query: ","passage_prefix":"passage: ","rerank_enabled":false}}"#,
            mind.display(),
            port
        ),
    )
    .unwrap();

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
