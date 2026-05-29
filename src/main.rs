mod cli;
mod config;
mod daemon;
mod embedder;
mod error;
mod integrity;
mod knowledge;
mod session;
mod storage;
mod util;
mod vault;

use anyhow::Result;
use clap::Parser;
use cli::Cli;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
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
            // SAFETY: single-threaded at this point (before tokio runtime starts work)
            unsafe {
                std::env::set_var("ORT_DYLIB_PATH", &ort_lib);
            }
        }
    }

    let cli = Cli::parse();
    cli::run(cli).await
}
