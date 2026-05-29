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
