mod access;
mod audit;
mod bench;
mod bench_policy;
mod bench_procedural;
mod bench_stale;
mod cli;
mod confidence;
mod config;
mod consolidate;
mod doubt;
mod duel;
mod embedder;
mod error;
#[cfg(feature = "extractor")]
mod extractor;
mod ingest;
mod install_detect;
mod install_mode;
mod integrity;
mod knowledge;
mod mcp;
mod md_reconcile;
mod migrate_v14;
mod outcome;
mod procedure;
mod provenance;
mod relevance;
mod reranker;
mod secrets;
mod session;
mod session_ingest;
mod storage;
mod util;
mod vault;
mod viewer;

use anyhow::Result;
use clap::Parser;
use cli::Cli;
use tracing_subscriber::EnvFilter;

/// Tokio runtime built with an explicit 4 MB worker stack.
///
/// v1.6.4 Windows fix (#23): Windows default thread stack is 1 MB; the v1.5
/// background re-test loop's futures (MindConfig clone + payload HashMaps +
/// Vec<String> candidates) blow past it. Linux default is 8 MB so this is
/// only observable on Windows. 4 MB is the smallest power-of-two that
/// reliably fits the loop body's live state with a safety margin.
fn main() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(4 * 1024 * 1024)
        .build()?;
    runtime.block_on(async_main())
}

async fn async_main() -> Result<()> {
    // Logs go to stderr: in `mgimind mcp` mode stdout is the JSON-RPC channel
    // and must stay clean. stderr is also fine for every other subcommand.
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    // Auto-detect ORT library if not explicitly set
    if std::env::var("ORT_DYLIB_PATH").is_err() {
        let exe = std::env::current_exe().unwrap_or_default();
        let ort_lib = exe.parent().unwrap_or(std::path::Path::new(".")).join(
            if cfg!(target_os = "windows") {
                "onnxruntime.dll"
            } else if cfg!(target_os = "macos") {
                "libonnxruntime.dylib"
            } else {
                "libonnxruntime.so"
            },
        );
        if ort_lib.exists() {
            // SAFETY: set_var is unsafe in edition 2024 because a concurrent
            // getenv/setenv is UB. This runs at the very top of main, before we
            // spawn any task or load ORT; the runtime's worker threads exist but
            // none touch the environment here, so there is no concurrent access.
            unsafe {
                std::env::set_var("ORT_DYLIB_PATH", &ort_lib);
            }
        }
    }

    let cli = Cli::parse();
    cli::run(cli).await
}
