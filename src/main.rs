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
mod http_api;
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
mod retrieval_policy;
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

/// v1.6.4 Windows fix (#23): the process main thread runs on the OS default
/// stack (Windows: 1 MB; Linux: 8 MB; macOS: 512 KB but historically 8 MB
/// for the binary's main thread). The v1.5 background re-test loop's futures
/// (MindConfig clone + payload HashMaps + Vec<String> candidates) overflow
/// the 1 MB Windows main thread.
///
/// Two layers:
/// 1. Re-launch `main` on a `std::thread` with an explicit 8 MB stack. This
///    fixes the *main* thread budget across platforms.
/// 2. Build the tokio runtime with `thread_stack_size(8 * 1024 * 1024)` so
///    every worker thread (where `tokio::spawn` lands futures) also gets
///    8 MB.
///
/// 8 MB matches the Linux default — the most-tested configuration — and is
/// enough headroom for the loop body's live state on any platform.
fn main() -> Result<()> {
    let stack_size = 8 * 1024 * 1024; // 8 MB
    let handle = std::thread::Builder::new()
        .name("mgimind-main".into())
        .stack_size(stack_size)
        .spawn(move || -> Result<()> {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_stack_size(stack_size)
                .build()?;
            runtime.block_on(async_main())
        })?;
    handle.join().expect("mgimind main thread panicked")
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
