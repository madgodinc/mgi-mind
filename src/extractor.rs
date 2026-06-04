//! v1.4 Phase 5: local LLM auto-extractor for knowledge-graph facts.
//!
//! Why this exists — the gap the Phase 1 migration surfaced. The
//! published memory products (mem0, Zep, supermemory) run an LLM
//! extractor on every ingested message via cloud API. Our local-first
//! contract forbids paid API in the default path. Without an
//! extractor of our own, the knowledge graph stays empty: users write
//! to `mind_add` (memories) but rarely call `mind_fact_add` by hand,
//! so the Phase 2 duel rule has nothing to operate on.
//!
//! The auto-extractor closes that gap with a **local LLM that ships
//! out of the box**. After a successful `mind_add`, a background pass
//! runs the new memory through the extractor; resulting subject-
//! predicate-object triples are written to the knowledge graph via
//! `mind_fact_add`. The user sees memories appear; facts accumulate
//! silently underneath.
//!
//! ## Model variants (this commit)
//!
//! - **Lite** — Qwen 2.5 1.5B Instruct Q4_K_M, ~990 MB on disk,
//!   ~1.5 GB RAM, ~7 sec per extraction on a current x86 CPU.
//!   Weaker multilingual; default choice for small machines.
//! - **Default** — Qwen 2.5 3B Instruct Q4_K_M, ~1.93 GB on disk,
//!   ~2.5 GB RAM, ~12 sec per extraction on the same CPU. Native
//!   RU+EN+ZH support; recommended for the author's mixed-language
//!   base.
//!
//! Same Qwen 2.5 family for both: identical chat template, identical
//! tokenizer behaviour, identical output structure. Switching between
//! the two is a config flag, not a code path.
//!
//! ## Why CPU-only default (mirrors the embedder decision)
//!
//! - Distribution: one binary, no CUDA toolkit at build time, no
//!   NVIDIA driver requirement at runtime. Works on every Mac, every
//!   Linux, every Windows.
//! - VRAM stays free for embedder/reranker/games/anything else the
//!   user is running.
//! - CPU inference of 1.5B/3B Q4 on modern hardware is "background-
//!   task fast" — slow for an interactive prompt but fine for an
//!   async post-ingest pass.
//! - Optional CUDA backend lands as a feature flag in a follow-up
//!   commit (the same pattern the embedder uses).
//!
//! ## TODO(phase-5-step-2..5)
//!
//! This commit lands the variant enum, the pinned-hash slots, and
//! the CLI surface contract. The actual GGUF download, model load,
//! prompt template, JSON parse, and `mind_ingest` auto-integration
//! land as separate bisectable commits on this same branch:
//!
//! - Step 5.2: `llama-cpp-2` cargo dependency + GGUF download via
//!   `util::download_file` (same pattern as the embedder ONNX and
//!   the bundled Qdrant binary).
//! - Step 5.3: lazy model load in a `OnceCell<Mutex<LlamaModel>>`,
//!   ChatML prompt template, JSON parser for the triples.
//! - Step 5.4: `mind_extractor` consolidated MCP tool +
//!   `mgimind extractor install/info/uninstall` CLI.
//! - Step 5.5: hook from `mind_ingest` write-path to enqueue a
//!   background extraction task per accepted memory.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};

// ===== Variant selection =====

/// Which Qwen 2.5 weight set to use. Both are Q4_K_M for the same
/// inference path and the same prompt template; the difference is
/// parameter count, which trades disk/RAM/quality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExtractorVariant {
    /// Qwen 2.5 1.5B Instruct Q4_K_M. ~990 MB on disk, ~1.5 GB RAM.
    /// Faster, weaker multilingual. Suitable for laptops with limited
    /// RAM or for EN-only content.
    Lite,
    /// Qwen 2.5 3B Instruct Q4_K_M. ~1.93 GB on disk, ~2.5 GB RAM.
    /// Default. Recommended for mixed RU+EN+ZH content, including
    /// the author's own base.
    Default,
}

impl Default for ExtractorVariant {
    fn default() -> Self {
        ExtractorVariant::Default
    }
}

impl ExtractorVariant {
    /// Parse the variant from a CLI / config string. Case- and
    /// whitespace-tolerant. Returns `None` on unknown values so the
    /// caller can prompt or fall back explicitly instead of silently
    /// picking a default that may not be what the user expected.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "lite" | "1.5b" | "small" | "qwen-1.5b" => Some(ExtractorVariant::Lite),
            "default" | "3b" | "qwen-3b" | "" => Some(ExtractorVariant::Default),
            _ => None,
        }
    }

    /// Lowercase wire format. Stable from v1.4 — do not rename existing
    /// variants; new ones append.
    pub fn as_str(self) -> &'static str {
        match self {
            ExtractorVariant::Lite => "lite",
            ExtractorVariant::Default => "default",
        }
    }

    /// Filename the GGUF model is stored under inside
    /// `$MGIMIND_HOME/models/extractor/`. Different per variant so a
    /// user can swap without re-downloading the variant they already
    /// have.
    pub fn gguf_filename(self) -> &'static str {
        match self {
            ExtractorVariant::Lite => "qwen2.5-1.5b-instruct-q4_k_m.gguf",
            ExtractorVariant::Default => "qwen2.5-3b-instruct-q4_k_m.gguf",
        }
    }

    /// HuggingFace download URL for the GGUF artifact. Both come from
    /// the same Qwen team upload — `Qwen/Qwen2.5-*-Instruct-GGUF` —
    /// so the user is reading from the model authors' own release
    /// channel, not a third-party rehosting.
    pub fn hf_url(self) -> &'static str {
        match self {
            ExtractorVariant::Lite => {
                "https://huggingface.co/Qwen/Qwen2.5-1.5B-Instruct-GGUF/resolve/main/qwen2.5-1.5b-instruct-q4_k_m.gguf"
            }
            ExtractorVariant::Default => {
                "https://huggingface.co/Qwen/Qwen2.5-3B-Instruct-GGUF/resolve/main/qwen2.5-3b-instruct-q4_k_m.gguf"
            }
        }
    }

    /// On-disk size in megabytes (rounded). Surfaced to the user
    /// during install so they confirm the download size before the
    /// long wait.
    pub fn approx_size_mb(self) -> u32 {
        match self {
            ExtractorVariant::Lite => 990,
            ExtractorVariant::Default => 1930,
        }
    }

    /// Approximate RAM used by the model + KV cache at default
    /// context length when loaded. Surfaced before install so the user
    /// understands the operational cost, not just the disk cost.
    pub fn approx_ram_mb(self) -> u32 {
        match self {
            ExtractorVariant::Lite => 1500,
            ExtractorVariant::Default => 2500,
        }
    }

    /// Pinned SHA-256 for the GGUF file. Used by `util::download_file`
    /// fail-closed verification (audit #6 pattern). Both pins live in
    /// `integrity.rs`; this method just routes the variant to its pin.
    pub fn pinned_hash(self) -> Option<&'static str> {
        match self {
            ExtractorVariant::Lite => {
                crate::integrity::pin(crate::integrity::EXTRACTOR_QWEN_1_5B_Q4_K_M)
            }
            ExtractorVariant::Default => {
                crate::integrity::pin(crate::integrity::EXTRACTOR_QWEN_3B_Q4_K_M)
            }
        }
    }

    /// One-line operator-facing summary printed by `mgimind extractor
    /// install` before the download starts and by `info` after.
    pub fn describe(self) -> String {
        format!(
            "Qwen 2.5 {} Instruct Q4_K_M — {} MB on disk, ~{} MB RAM loaded",
            match self {
                ExtractorVariant::Lite => "1.5B",
                ExtractorVariant::Default => "3B",
            },
            self.approx_size_mb(),
            self.approx_ram_mb()
        )
    }

    /// Multilingual warning surfaced when the Lite variant is selected.
    /// Empty for Default. The text is wired into the install CLI so the
    /// user is told before they commit to the smaller variant.
    pub fn multilingual_warning(self) -> &'static str {
        match self {
            ExtractorVariant::Lite => {
                "Note: lite variant has weaker non-English extraction. \
                 For Russian / Chinese / mixed-language content, consider \
                 --variant default (~1.93 GB on disk)."
            }
            ExtractorVariant::Default => "",
        }
    }
}

// ===== Download path (step 5.2) =====
//
// Downloads the chosen GGUF model into `$MGIMIND_HOME/models/extractor/`
// using the same `util::download_file` fail-closed integrity verification
// the embedder ONNX uses. The pinned sha256 lives in `integrity.rs`; when
// PIN_ME placeholders are present the download proceeds with a printed
// warning rather than failing, because Phase 5 is explicitly opt-in.

use anyhow::{Context, Result};
use std::path::PathBuf;

/// Directory where extractor GGUF files live.
pub fn extractor_dir() -> PathBuf {
    crate::config::mind_home().join("models").join("extractor")
}

/// Path to the GGUF file for the given variant.
pub fn gguf_path(variant: ExtractorVariant) -> PathBuf {
    extractor_dir().join(variant.gguf_filename())
}

/// Whether the variant's GGUF is already on disk.
pub fn is_installed(variant: ExtractorVariant) -> bool {
    gguf_path(variant).exists()
}

/// Download the variant's GGUF model. Idempotent: re-running with the
/// file already present skips the network round-trip. Verifies the
/// pinned sha256 if available; warns and proceeds if PIN_ME.
pub async fn download(variant: ExtractorVariant) -> Result<PathBuf> {
    let dir = extractor_dir();
    std::fs::create_dir_all(&dir).context("create extractor dir")?;
    let dest = gguf_path(variant);
    if dest.exists() {
        eprintln!(
            "  {} already present, skipping download",
            variant.gguf_filename()
        );
        return Ok(dest);
    }
    eprintln!(
        "  downloading {} ({} MB)...",
        variant.gguf_filename(),
        variant.approx_size_mb()
    );
    let pin = variant.pinned_hash();
    if pin.is_none() {
        eprintln!(
            "  [warn] no pinned checksum for {} (variant slot is PIN_ME) — integrity not verified",
            variant.gguf_filename()
        );
    }
    crate::util::download_file(variant.hf_url(), &dest, pin).await?;
    eprintln!("  saved to {}", dest.display());
    Ok(dest)
}

/// Remove the variant's GGUF from disk. Used by `mgimind extractor
/// uninstall`. Returns true if the file was removed, false if it
/// wasn't there to begin with.
pub fn uninstall(variant: ExtractorVariant) -> Result<bool> {
    let path = gguf_path(variant);
    if path.exists() {
        std::fs::remove_file(&path).context("remove gguf file")?;
        Ok(true)
    } else {
        Ok(false)
    }
}

// ===== llama-server subprocess + HTTP client (step 5.3) =====
//
// Architecture choice (driven by critic round, see PR #8 description):
//
// - Use upstream llama.cpp prebuilt `llama-server` binary, downloaded as
//   a Vulkan-enabled tarball into `$MGIMIND_HOME/bin/extractor/`. Same
//   pattern as bundled Qdrant — pin sha256, fail-closed verify, no C++
//   toolchain on user's machine, no CUDA driver requirement (Vulkan
//   works across NVIDIA / AMD / Intel iGPU).
//
// - Spawn the server as a subprocess on first extraction call, keep it
//   alive in the warm mgimind process (mind_extractor=long-running);
//   shut it down at mgimind mcp exit. Localhost-only `127.0.0.1` HTTP,
//   bearer token randomised per process start.
//
// - Each extraction is one /completion HTTP call with hard timeout
//   (60s default). On timeout / non-JSON output / schema-mismatch, we
//   retry once with a stricter prompt; on second failure, drop the
//   memory and log — better silent miss than poisoned graph.
//
// - Tokio integration: extraction runs inside spawn_blocking with a
//   semaphore capping concurrent calls to 1 (synthesis §10 q5
//   guarantee a + critic R3). This prevents an ingest burst from
//   starving the mind_search hot path.
//
// **Status: scaffold.** The actual subprocess management + HTTP call
// land as a separate commit on this branch once the surface contract
// here is reviewable.

use std::time::Duration;

/// Extracted subject-predicate-object triple from a chunk of text.
/// Emitted by `extract_facts`; consumed by the auto-ingest pipeline
/// that writes triples into the knowledge graph via `mind_fact_add`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Triple {
    pub subject: String,
    pub predicate: String,
    pub object: String,
}

/// Configuration for an extraction call. The defaults are chosen to
/// match the Phase 5 quality test: Vulkan inference of Qwen 2.5 3B
/// Q4_K_M at temp 0.1, single-turn, 300 token cap.
#[derive(Debug, Clone)]
pub struct ExtractConfig {
    pub variant: ExtractorVariant,
    pub temperature: f32,
    pub max_tokens: u32,
    pub timeout: Duration,
}

impl Default for ExtractConfig {
    fn default() -> Self {
        Self {
            variant: ExtractorVariant::Default,
            temperature: 0.1,
            max_tokens: 300,
            timeout: Duration::from_secs(60),
        }
    }
}

/// The prompt template the extractor uses. Chosen during the Phase 5
/// quality test as the simplest prompt that gives clean JSON on both
/// English and Russian input. Returns predicates in English snake_case
/// regardless of input language so the knowledge graph stays
/// canonical.
///
/// Quality observations from the test (commit Phase 5 scaffold):
/// - Simple declarative sentences: ~80% clean triples
/// - Sentences with dates / causality: ~20% structural errors
///   (timestamps incorrectly placed as subject). v1.5 work targets
///   this through few-shot examples in the prompt.
pub fn build_prompt(text: &str) -> String {
    format!(
        "Extract subject-predicate-object triples from this text. \
         Output ONLY a JSON array of objects with keys \"subject\", \
         \"predicate\", \"object\". Use English predicates with \
         underscores (e.g. \"lives_in\", \"is_a\", \"uses\"). \
         No explanation, no markdown fences. Text: {text}"
    )
}

/// Parse the model's response into a list of triples. Tolerant of the
/// most common malformations seen during the quality test:
/// - Markdown fences (```json ... ```) — stripped before parsing.
/// - Array-of-arrays instead of array-of-objects — caller logs and
///   triggers the retry-with-repair loop.
/// - Predicates in Russian — passed through unmodified for now;
///   normalisation lives in a separate post-process step.
///
/// Returns None on irrecoverable malformation; the caller is expected
/// to retry with a stricter prompt on None.
pub fn parse_response(raw: &str) -> Option<Vec<Triple>> {
    // Strip markdown fences and trim.
    let cleaned = raw
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    // Find the JSON array body.
    let start = cleaned.find('[')?;
    let end = cleaned.rfind(']')?;
    if end <= start {
        return None;
    }
    let body = &cleaned[start..=end];

    // Try as Vec<Triple> first.
    if let Ok(triples) = serde_json::from_str::<Vec<Triple>>(body) {
        return Some(triples);
    }

    // Fallback: array-of-arrays [["S","P","O"], ...].
    if let Ok(arrays) = serde_json::from_str::<Vec<Vec<String>>>(body) {
        let triples: Vec<Triple> = arrays
            .into_iter()
            .filter_map(|a| {
                if a.len() == 3 {
                    Some(Triple {
                        subject: a[0].clone(),
                        predicate: a[1].clone(),
                        object: a[2].clone(),
                    })
                } else {
                    None
                }
            })
            .collect();
        if !triples.is_empty() {
            return Some(triples);
        }
    }

    None
}

// ===== llama-server lifecycle =====

use once_cell::sync::OnceCell;
use std::process::{Child, Stdio};
use std::sync::Mutex;
use tokio::sync::Semaphore;

/// Process-global handle to the llama-server subprocess. Started on
/// first extraction call, kept alive for the lifetime of the warm
/// mgimind process, shut down on Drop or explicit `shutdown_server()`.
static LLAMA_SERVER: OnceCell<Mutex<Option<LlamaServerHandle>>> = OnceCell::new();

/// Semaphore capping concurrent extractions to 1 — single
/// llama-server can only process one request at a time, queueing on
/// the client side prevents the tokio runtime from piling up
/// background-blocking tasks during ingest bursts (critic R3).
static EXTRACTION_SEMAPHORE: OnceCell<Semaphore> = OnceCell::new();

pub(crate) struct LlamaServerHandle {
    pub child: Child,
    pub port: u16,
    pub api_key: String,
}

impl Drop for LlamaServerHandle {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Path to the llama-server binary inside `$MGIMIND_HOME/bin/extractor/`.
/// Step 5.4 install command places it there after downloading the
/// Vulkan tarball.
pub fn llama_server_path() -> PathBuf {
    crate::config::mind_home()
        .join("bin")
        .join("extractor")
        .join("llama-server")
}

/// Whether the llama-server binary is on disk and executable.
pub fn is_llama_server_installed() -> bool {
    llama_server_path().exists()
}

fn random_api_key() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::time::SystemTime;
    let mut h = DefaultHasher::new();
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .hash(&mut h);
    std::process::id().hash(&mut h);
    format!("mgimind-extractor-{:x}", h.finish())
}

fn pick_port() -> u16 {
    // Try-bind a TCP listener to ask the OS for a free port, then
    // immediately drop the listener and let llama-server bind it.
    // Brief race window between drop and bind is acceptable for a
    // single-process local-only deployment.
    use std::net::TcpListener;
    TcpListener::bind("127.0.0.1:0")
        .ok()
        .and_then(|l| l.local_addr().ok())
        .map(|a| a.port())
        .unwrap_or(8080)
}

/// Start the llama-server subprocess for the given variant. Returns
/// the existing handle if a server is already running. Idempotent.
async fn ensure_server(variant: ExtractorVariant) -> anyhow::Result<(u16, String)> {
    let slot = LLAMA_SERVER.get_or_init(|| Mutex::new(None));
    {
        let mut guard = slot
            .lock()
            .map_err(|_| anyhow::anyhow!("llama-server mutex poisoned"))?;
        if let Some(h) = guard.as_ref() {
            return Ok((h.port, h.api_key.clone()));
        }

        // Cold start.
        let server_path = llama_server_path();
        if !server_path.exists() {
            anyhow::bail!(
                "extractor server binary missing: {} (run `mgimind extractor install`)",
                server_path.display()
            );
        }
        let gguf = gguf_path(variant);
        if !gguf.exists() {
            anyhow::bail!(
                "extractor model missing: {} (run `mgimind extractor install`)",
                gguf.display()
            );
        }
        let port = pick_port();
        let api_key = random_api_key();
        let lib_dir = server_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));

        let mut cmd = std::process::Command::new(&server_path);
        cmd.arg("-m")
            .arg(&gguf)
            .arg("--host")
            .arg("127.0.0.1")
            .arg("--port")
            .arg(port.to_string())
            .arg("--api-key")
            .arg(&api_key)
            .arg("-ngl")
            .arg("99")
            .arg("--ctx-size")
            .arg("4096")
            .env("LD_LIBRARY_PATH", &lib_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let child = cmd
            .spawn()
            .context("failed to spawn llama-server subprocess")?;

        *guard = Some(LlamaServerHandle {
            child,
            port,
            api_key: api_key.clone(),
        });

        // Wait for server readiness — poll /health up to 30s.
        let url = format!("http://127.0.0.1:{port}/health");
        let client = reqwest::Client::new();
        for _ in 0..60 {
            tokio::time::sleep(Duration::from_millis(500)).await;
            if let Ok(resp) = client.get(&url).send().await {
                if resp.status().is_success() {
                    return Ok((port, api_key));
                }
            }
        }
        // Failed health check — shut down the subprocess.
        *guard = None;
        anyhow::bail!("llama-server failed to become ready within 30s");
    }
}

/// Shut down the llama-server subprocess if running. Called by
/// `mgimind extractor unload` and on warm-process shutdown.
pub fn shutdown_server() {
    let Some(slot) = LLAMA_SERVER.get() else { return };
    let Ok(mut guard) = slot.lock() else { return };
    *guard = None; // Drop runs kill+wait
}

/// Whether the server is currently running in this process.
pub fn is_server_running() -> bool {
    LLAMA_SERVER
        .get()
        .and_then(|s| s.lock().ok().map(|g| g.is_some()))
        .unwrap_or(false)
}

// ===== Extraction =====

#[derive(Debug, Serialize)]
struct CompletionRequest<'a> {
    prompt: &'a str,
    temperature: f32,
    n_predict: u32,
    stream: bool,
}

#[derive(Debug, Deserialize)]
struct CompletionResponse {
    content: String,
}

/// Extract S-P-O triples from a chunk of text. Wraps the model call in
/// the semaphore + retry-with-repair loop + hard timeout.
///
/// On success: returns a (possibly empty) list of triples. On failure
/// after retry: returns an error and the caller is expected to log
/// and drop the chunk — silent miss is better than poisoned graph.
pub async fn extract_facts(
    config: &ExtractConfig,
    text: &str,
) -> anyhow::Result<Vec<Triple>> {
    let sem = EXTRACTION_SEMAPHORE.get_or_init(|| Semaphore::new(1));
    let _permit = sem.acquire().await?;

    let (port, api_key) = ensure_server(config.variant).await?;
    let prompt = build_prompt(text);

    let first = call_completion(port, &api_key, &prompt, config).await;
    if let Ok(raw) = first {
        if let Some(triples) = parse_response(&raw) {
            return Ok(triples);
        }
        // Retry with stricter wording — happens ~5-15% of the time
        // per critic R2 on small models.
        let strict_prompt = format!(
            "{prompt}\n\nIMPORTANT: respond with a valid JSON array only. \
             No prose, no markdown, no explanation. Just the array."
        );
        let second = call_completion(port, &api_key, &strict_prompt, config).await?;
        return parse_response(&second).ok_or_else(|| {
            anyhow::anyhow!("extractor returned non-JSON twice; dropping chunk")
        });
    }
    first.map(|_| Vec::new())
}

async fn call_completion(
    port: u16,
    api_key: &str,
    prompt: &str,
    config: &ExtractConfig,
) -> anyhow::Result<String> {
    let url = format!("http://127.0.0.1:{port}/completion");
    let body = CompletionRequest {
        prompt,
        temperature: config.temperature,
        n_predict: config.max_tokens,
        stream: false,
    };
    let client = reqwest::Client::builder()
        .timeout(config.timeout)
        .build()
        .context("build reqwest client")?;
    let resp = client
        .post(&url)
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await
        .context("POST /completion failed")?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("llama-server returned {status}: {text}");
    }
    let body: CompletionResponse = resp.json().await.context("parse /completion response")?;
    Ok(body.content)
}

// ===== Tests =====

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_round_trips_lowercase_canonical() {
        assert_eq!(
            ExtractorVariant::parse("lite"),
            Some(ExtractorVariant::Lite)
        );
        assert_eq!(
            ExtractorVariant::parse("default"),
            Some(ExtractorVariant::Default)
        );
    }

    #[test]
    fn parse_accepts_size_aliases() {
        // Aliases let the user write "1.5b" or "3b" — friendlier than
        // forcing them to remember "lite" vs "default" especially
        // when they're choosing by spec rather than role.
        assert_eq!(
            ExtractorVariant::parse("1.5b"),
            Some(ExtractorVariant::Lite)
        );
        assert_eq!(
            ExtractorVariant::parse("3b"),
            Some(ExtractorVariant::Default)
        );
        assert_eq!(
            ExtractorVariant::parse("small"),
            Some(ExtractorVariant::Lite)
        );
    }

    #[test]
    fn parse_is_case_and_whitespace_tolerant() {
        assert_eq!(
            ExtractorVariant::parse("  LITE  "),
            Some(ExtractorVariant::Lite)
        );
        assert_eq!(
            ExtractorVariant::parse("Default"),
            Some(ExtractorVariant::Default)
        );
    }

    #[test]
    fn parse_empty_returns_default_variant() {
        // Empty input maps to Default rather than None so a CLI that
        // forgets to pass --variant lands on the recommended choice.
        assert_eq!(
            ExtractorVariant::parse(""),
            Some(ExtractorVariant::Default)
        );
    }

    #[test]
    fn parse_unknown_returns_none() {
        // None — not silent fallback — so the caller logs the unknown
        // value and decides (warning + Default fallback, hard error,
        // etc.) explicitly. Mirrors the Cardinality::parse contract.
        assert_eq!(ExtractorVariant::parse("turbo"), None);
        assert_eq!(ExtractorVariant::parse("qwen-72b"), None);
    }

    #[test]
    fn default_variant_is_default() {
        assert_eq!(ExtractorVariant::default(), ExtractorVariant::Default);
    }

    #[test]
    fn wire_format_round_trips_via_as_str_parse() {
        for v in [ExtractorVariant::Lite, ExtractorVariant::Default] {
            assert_eq!(ExtractorVariant::parse(v.as_str()), Some(v));
        }
    }

    #[test]
    fn gguf_filenames_distinguish_variants() {
        // Important: the two variants store under different filenames
        // so a user can swap without redownloading whichever they
        // already have. If this collapses, the migration path breaks.
        let a = ExtractorVariant::Lite.gguf_filename();
        let b = ExtractorVariant::Default.gguf_filename();
        assert_ne!(a, b);
        assert!(a.ends_with(".gguf"));
        assert!(b.ends_with(".gguf"));
    }

    #[test]
    fn hf_urls_come_from_qwen_official_team() {
        // Defensive: if a refactor ever rewires us to a third-party
        // mirror we want the test to scream. The Qwen team's own
        // GGUF release is the authoritative source.
        for v in [ExtractorVariant::Lite, ExtractorVariant::Default] {
            let url = v.hf_url();
            assert!(
                url.starts_with("https://huggingface.co/Qwen/"),
                "extractor variant {v:?} must download from Qwen's official HF org"
            );
            assert!(url.ends_with(".gguf"));
        }
    }

    #[test]
    fn approx_sizes_are_in_expected_bands() {
        // The numbers we promise the user in `describe()` and the
        // install confirmation. If a future requantization changes
        // the bytes we want the tests to flag it so the user-facing
        // copy stays accurate.
        assert!(ExtractorVariant::Lite.approx_size_mb() < 1100);
        assert!(ExtractorVariant::Default.approx_size_mb() < 2100);
        assert!(ExtractorVariant::Default.approx_size_mb() > ExtractorVariant::Lite.approx_size_mb());
    }

    #[test]
    fn lite_carries_multilingual_warning_default_does_not() {
        // Multilingual warning is a behaviour contract surfaced by
        // the install CLI. If a future refactor accidentally clears
        // it on Lite, the user gets the smaller model without being
        // told about the trade-off.
        assert!(!ExtractorVariant::Lite
            .multilingual_warning()
            .is_empty());
        assert_eq!(
            ExtractorVariant::Default.multilingual_warning(),
            ""
        );
    }

    #[test]
    fn describe_contains_size_and_ram() {
        // describe() is what the user sees in the install prompt; we
        // want both numbers in the same line so the trade-off is
        // visible at the moment of confirmation, not after.
        let s = ExtractorVariant::Default.describe();
        assert!(s.contains("MB on disk"));
        assert!(s.contains("MB RAM"));
        assert!(s.contains("3B"));
    }

    // ===== Prompt + parser tests =====

    #[test]
    fn build_prompt_includes_text_and_schema() {
        let p = build_prompt("Aurora is a dashboard.");
        assert!(p.contains("Aurora is a dashboard."));
        assert!(p.contains("subject"));
        assert!(p.contains("predicate"));
        assert!(p.contains("object"));
        assert!(p.contains("JSON array"));
    }

    #[test]
    fn parse_response_handles_clean_json_array_of_objects() {
        let raw = r#"[
            {"subject": "Mad", "predicate": "uses", "object": "Rust"},
            {"subject": "Mad", "predicate": "lives_in", "object": "Almaty"}
        ]"#;
        let triples = parse_response(raw).unwrap();
        assert_eq!(triples.len(), 2);
        assert_eq!(triples[0].subject, "Mad");
        assert_eq!(triples[0].predicate, "uses");
        assert_eq!(triples[0].object, "Rust");
    }

    #[test]
    fn parse_response_strips_markdown_fences() {
        let raw = "```json\n[{\"subject\":\"a\",\"predicate\":\"b\",\"object\":\"c\"}]\n```";
        let triples = parse_response(raw).unwrap();
        assert_eq!(triples.len(), 1);
    }

    #[test]
    fn parse_response_handles_array_of_arrays_fallback() {
        // Observed during quality test on Russian input — model
        // returned [["S","P","O"], ...] instead of [{...}, ...]. We
        // accept the malformation rather than retrying on a
        // structurally valid (if non-canonical) response.
        let raw = r#"[
            ["Mad", "uses", "Rust"],
            ["Mad", "lives_in", "Almaty"]
        ]"#;
        let triples = parse_response(raw).unwrap();
        assert_eq!(triples.len(), 2);
        assert_eq!(triples[1].subject, "Mad");
        assert_eq!(triples[1].predicate, "lives_in");
    }

    #[test]
    fn parse_response_drops_malformed_inner_arrays() {
        // Array-of-arrays with wrong arity inside one element — that
        // element is dropped but the rest of the valid ones pass.
        let raw = r#"[
            ["Mad", "uses", "Rust"],
            ["only", "two"],
            ["Mad", "lives_in", "Almaty"]
        ]"#;
        let triples = parse_response(raw).unwrap();
        assert_eq!(triples.len(), 2);
    }

    #[test]
    fn parse_response_returns_none_on_irrecoverable() {
        assert!(parse_response("not json").is_none());
        assert!(parse_response("").is_none());
        assert!(parse_response("[").is_none());
    }

    #[test]
    fn parse_response_ignores_text_before_array() {
        // Models often prefix their JSON with explanation. We tolerate
        // it by finding the first '[' and last ']'.
        let raw = "Here are the triples:\n```json\n[{\"subject\":\"x\",\"predicate\":\"y\",\"object\":\"z\"}]\n```";
        let triples = parse_response(raw).unwrap();
        assert_eq!(triples.len(), 1);
    }

    #[test]
    fn extract_config_default_uses_3b_variant() {
        let cfg = ExtractConfig::default();
        assert_eq!(cfg.variant, ExtractorVariant::Default);
        assert_eq!(cfg.temperature, 0.1);
    }

    #[test]
    fn triple_serde_round_trips() {
        let t = Triple {
            subject: "A".into(),
            predicate: "B".into(),
            object: "C".into(),
        };
        let json = serde_json::to_string(&t).unwrap();
        let back: Triple = serde_json::from_str(&json).unwrap();
        assert_eq!(t, back);
    }
}
