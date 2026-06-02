use anyhow::{Context, Result};
use once_cell::sync::OnceCell;
use ort::session::Session;
use ort::value::Value;
use std::path::Path;
use std::sync::Mutex;

use crate::config::MindConfig;
use crate::integrity;

static SESSION: OnceCell<Mutex<Session>> = OnceCell::new();
// Tokenizer loaded once and reused, instead of re-read from disk on every embed (audit #17).
static TOKENIZER: OnceCell<tokenizers::Tokenizer> = OnceCell::new();

/// Max sequence length (MiniLM and XLM-R/e5 both cap at 512 positions). Longer
/// inputs overflow the position-embedding table → ONNX "invalid expand shape".
const MAX_SEQ_LEN: usize = 512;

fn get_model_path(config: &MindConfig) -> std::path::PathBuf {
    crate::config::models_dir()
        .join(&config.model_name)
        .join("model.onnx")
}

/// Load the ONNX session once and reuse it. `get_or_try_init` ensures it is built
/// exactly once even under concurrent first calls (no double-build race).
fn session(config: &MindConfig) -> Result<&'static Mutex<Session>> {
    SESSION.get_or_try_init(|| {
        let model_path = get_model_path(config);
        if !model_path.exists() {
            anyhow::bail!(
                "Model not found at {}. Run `mgimind doctor --fix` to download it.",
                model_path.display()
            );
        }
        let mut builder = Session::builder()
            .map_err(|e| anyhow::anyhow!("Failed to create session builder: {e}"))?
            .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)
            .map_err(|e| anyhow::anyhow!("Failed to set optimization level: {e}"))?;

        // Opt-in CUDA via env var. CPU stays the default so the standard release
        // (built without the `cuda` feature) behaves exactly as before. The
        // execution-provider call is only compiled in when the `cuda` cargo
        // feature is enabled (see Cargo.toml + Dockerfile).
        #[cfg(feature = "cuda")]
        if std::env::var("MGIMIND_USE_CUDA").ok().as_deref() == Some("1") {
            use ort::execution_providers::CUDAExecutionProvider;
            builder = builder
                .with_execution_providers([CUDAExecutionProvider::default().build()])
                .map_err(|e| anyhow::anyhow!("Failed to register CUDA EP (embedder): {e}"))?;
            eprintln!("[mgimind] embedder: CUDA execution provider registered");
        }

        let session = builder
            .commit_from_file(&model_path)
            .map_err(|e| anyhow::anyhow!("Failed to load ONNX model: {e}"))?;
        Ok(Mutex::new(session))
    })
}

fn get_tokenizer(config: &MindConfig) -> Result<&'static tokenizers::Tokenizer> {
    TOKENIZER.get_or_try_init(|| {
        let tokenizer_path = crate::config::models_dir()
            .join(&config.model_name)
            .join("tokenizer.json");
        tokenizers::Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| anyhow::anyhow!("Failed to load tokenizer: {e}"))
    })
}

/// Embed a search query - applies the model's query prefix (e5 needs "query: ";
/// MiniLM uses none). Audit #21.
pub async fn embed_query(config: &MindConfig, text: &str) -> Result<Vec<f32>> {
    embed_prefixed(config, &config.query_prefix, text).await
}

/// Embed a stored document - applies the passage prefix (e5 needs "passage: ").
pub async fn embed_passage(config: &MindConfig, text: &str) -> Result<Vec<f32>> {
    embed_prefixed(config, &config.passage_prefix, text).await
}

/// Embed many documents in ONE padded ONNX pass (audit #2). The reranker already
/// batches `[N, seq]`; the embedder did not, so a long note of N chunks meant N
/// sequential model runs. This runs them as a single `[N, max_len]` batch -
/// the throughput win for `add` of long content (and bulk import). The passage
/// prefix is applied per text, exactly like `embed_passage`.
pub async fn embed_passages(config: &MindConfig, texts: &[String]) -> Result<Vec<Vec<f32>>> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    let prefixed: Vec<String> = if config.passage_prefix.is_empty() {
        texts.to_vec()
    } else {
        texts
            .iter()
            .map(|t| format!("{}{t}", config.passage_prefix))
            .collect()
    };
    embed_batch(config, &prefixed).await
}

async fn embed_prefixed(config: &MindConfig, prefix: &str, text: &str) -> Result<Vec<f32>> {
    if prefix.is_empty() {
        embed(config, text).await
    } else {
        embed(config, &format!("{prefix}{text}")).await
    }
}

pub async fn embed(config: &MindConfig, text: &str) -> Result<Vec<f32>> {
    let session_lock = session(config)?;
    let mut session = session_lock
        .lock()
        .map_err(|e| anyhow::anyhow!("Session lock poisoned: {e}"))?;

    let tokenizer = get_tokenizer(config)?;

    let encoding = tokenizer
        .encode(text, true)
        .map_err(|e| anyhow::anyhow!("Tokenization failed: {e}"))?;

    // Cap to the model's max sequence length (512 for MiniLM/XLM-R). Longer inputs
    // overflow the position-embedding table → ONNX "invalid expand shape" crash.
    let mut input_ids: Vec<i64> = encoding.get_ids().iter().map(|&id| id as i64).collect();
    let mut attention_mask: Vec<i64> = encoding
        .get_attention_mask()
        .iter()
        .map(|&m| m as i64)
        .collect();
    if input_ids.len() > MAX_SEQ_LEN {
        input_ids.truncate(MAX_SEQ_LEN);
        attention_mask.truncate(MAX_SEQ_LEN);
    }

    let seq_len = input_ids.len();

    let ids_value = Value::from_array(([1usize, seq_len], input_ids))
        .map_err(|e| anyhow::anyhow!("Failed to create input_ids tensor: {e}"))?;
    let mask_value = Value::from_array(([1usize, seq_len], attention_mask.clone()))
        .map_err(|e| anyhow::anyhow!("Failed to create attention_mask tensor: {e}"))?;

    use ort::session::SessionInputValue;
    let mut inputs: Vec<(std::borrow::Cow<'_, str>, SessionInputValue<'_>)> = vec![
        (std::borrow::Cow::from("input_ids"), ids_value.into()),
        (std::borrow::Cow::from("attention_mask"), mask_value.into()),
    ];
    // BERT-family models (MiniLM) take token_type_ids; XLM-R models (bge-m3) do
    // not - passing it to a model that doesn't expect it is a hard error (#21).
    if config.uses_token_type_ids {
        let token_type_ids: Vec<i64> = encoding
            .get_type_ids()
            .iter()
            .take(seq_len)
            .map(|&t| t as i64)
            .collect();
        let type_value = Value::from_array(([1usize, seq_len], token_type_ids))
            .map_err(|e| anyhow::anyhow!("Failed to create token_type_ids tensor: {e}"))?;
        inputs.push((std::borrow::Cow::from("token_type_ids"), type_value.into()));
    }

    let outputs = session
        .run(inputs)
        .map_err(|e| anyhow::anyhow!("Inference failed: {e}"))?;

    let (shape, data) = outputs[0]
        .try_extract_tensor::<f32>()
        .map_err(|e| anyhow::anyhow!("Failed to extract output tensor: {e}"))?;

    // Use the model's actual hidden size; fall back to the configured dim (audit #11).
    let hidden_size = if shape.len() == 3 {
        shape[2] as usize
    } else {
        config.vector_size as usize
    };

    // Pool token embeddings → one vector. MiniLM uses attention-masked mean
    // pooling; XLM-R models (bge-m3) use the [CLS]/first-token representation (#21).
    let mut pooled = if config.pooling == "cls" {
        cls_pool(data, hidden_size)
    } else {
        mean_pool(data, &attention_mask, seq_len, hidden_size)
    };
    l2_normalize(&mut pooled);

    Ok(pooled)
}

/// Embed a batch of already-prefixed texts in a single padded ONNX pass (audit
/// #2). Right-pads every sequence to the batch max (capped at `MAX_SEQ_LEN`),
/// runs one `[N, max_len]` inference, then pools each row with its own mask -
/// identical math to the single-text `embed`, just N at a time.
async fn embed_batch(config: &MindConfig, texts: &[String]) -> Result<Vec<Vec<f32>>> {
    let session_lock = session(config)?;
    let tokenizer = get_tokenizer(config)?;

    let mut encodings = Vec::with_capacity(texts.len());
    for t in texts {
        let enc = tokenizer
            .encode(t.as_str(), true)
            .map_err(|e| anyhow::anyhow!("Tokenization failed: {e}"))?;
        encodings.push(enc);
    }

    let n = encodings.len();
    // Cap to the model's max sequence length, exactly like the single path.
    let max_len = encodings
        .iter()
        .map(|e| e.get_ids().len())
        .max()
        .unwrap_or(1)
        .clamp(1, MAX_SEQ_LEN);

    // Right-pad every sequence (pad id 0, mask 0) into rectangular [n, max_len]
    // buffers. Padded tokens carry mask 0, so masked mean pooling ignores them.
    let mut ids = vec![0i64; n * max_len];
    let mut mask = vec![0i64; n * max_len];
    let mut types = vec![0i64; n * max_len];
    for (i, enc) in encodings.iter().enumerate() {
        let eids = enc.get_ids();
        let emask = enc.get_attention_mask();
        let etypes = enc.get_type_ids();
        let len = eids.len().min(max_len);
        for j in 0..len {
            ids[i * max_len + j] = eids[j] as i64;
            mask[i * max_len + j] = emask[j] as i64;
            types[i * max_len + j] = etypes[j] as i64;
        }
    }

    let mut session = session_lock
        .lock()
        .map_err(|e| anyhow::anyhow!("Session lock poisoned: {e}"))?;

    let ids_value = Value::from_array(([n, max_len], ids))
        .map_err(|e| anyhow::anyhow!("Failed to create input_ids tensor: {e}"))?;
    let mask_value = Value::from_array(([n, max_len], mask.clone()))
        .map_err(|e| anyhow::anyhow!("Failed to create attention_mask tensor: {e}"))?;

    use ort::session::SessionInputValue;
    let mut inputs: Vec<(std::borrow::Cow<'_, str>, SessionInputValue<'_>)> = vec![
        (std::borrow::Cow::from("input_ids"), ids_value.into()),
        (std::borrow::Cow::from("attention_mask"), mask_value.into()),
    ];
    if config.uses_token_type_ids {
        let type_value = Value::from_array(([n, max_len], types))
            .map_err(|e| anyhow::anyhow!("Failed to create token_type_ids tensor: {e}"))?;
        inputs.push((std::borrow::Cow::from("token_type_ids"), type_value.into()));
    }

    let outputs = session
        .run(inputs)
        .map_err(|e| anyhow::anyhow!("Inference failed: {e}"))?;

    let (shape, data) = outputs[0]
        .try_extract_tensor::<f32>()
        .map_err(|e| anyhow::anyhow!("Failed to extract output tensor: {e}"))?;

    let hidden = if shape.len() == 3 {
        shape[2] as usize
    } else {
        config.vector_size as usize
    };

    // Pool each row independently using that row's slice of the [n, max_len,
    // hidden] output and its own attention mask.
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let row = &data[i * max_len * hidden..(i + 1) * max_len * hidden];
        let row_mask = &mask[i * max_len..(i + 1) * max_len];
        let mut pooled = if config.pooling == "cls" {
            cls_pool(row, hidden)
        } else {
            mean_pool(row, row_mask, max_len, hidden)
        };
        l2_normalize(&mut pooled);
        out.push(pooled);
    }

    Ok(out)
}

/// Attention-masked mean pooling over the token dimension of a `[1, seq_len,
/// hidden]` last-hidden-state buffer (flattened row-major in `data`).
fn mean_pool(data: &[f32], attention_mask: &[i64], seq_len: usize, hidden: usize) -> Vec<f32> {
    let mut pooled = vec![0.0f32; hidden];
    let mut total_weight = 0.0f32;
    for (token_idx, &m) in attention_mask.iter().enumerate().take(seq_len) {
        let mask = m as f32;
        total_weight += mask;
        let offset = token_idx * hidden;
        for (dim, p) in pooled.iter_mut().enumerate() {
            *p += data[offset + dim] * mask;
        }
    }
    if total_weight > 0.0 {
        for v in &mut pooled {
            *v /= total_weight;
        }
    }
    pooled
}

/// [CLS]/first-token pooling: the first token's hidden vector.
fn cls_pool(data: &[f32], hidden: usize) -> Vec<f32> {
    data[..hidden.min(data.len())].to_vec()
}

/// In-place L2 normalization (cosine-ready vectors).
fn l2_normalize(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v {
            *x /= norm;
        }
    }
}

pub fn is_model_downloaded(config: &MindConfig) -> bool {
    let model_dir = crate::config::models_dir().join(&config.model_name);
    model_dir.join("model.onnx").exists() && model_dir.join("tokenizer.json").exists()
}

/// Look up the pinned checksum for the default model's files (audit #6).
/// Custom models have no pin (returns None → download with a warning).
fn model_file_pin(model_name: &str, local_name: &str) -> Option<&'static str> {
    match (model_name, local_name) {
        ("multilingual-e5-base", "model.onnx") => integrity::pin(integrity::MODEL_E5_BASE_ONNX),
        ("multilingual-e5-base", "tokenizer.json") => {
            integrity::pin(integrity::MODEL_E5_BASE_TOKENIZER)
        }
        ("all-MiniLM-L6-v2", "model.onnx") => integrity::pin(integrity::MODEL_MINILM_ONNX),
        ("all-MiniLM-L6-v2", "tokenizer.json") => integrity::pin(integrity::MODEL_MINILM_TOKENIZER),
        _ => None,
    }
}

/// HuggingFace source (base URL + (remote_path, local_name) files) for a model's
/// ONNX + tokenizer. e5 ships ONNX under the Xenova mirror (quantized = CPU-
/// friendly); sentence-transformers models keep their own `onnx/` path. Audit #21.
fn model_source(model_name: &str) -> (String, [(&'static str, &'static str); 2]) {
    match model_name {
        "multilingual-e5-base" => (
            "https://huggingface.co/Xenova/multilingual-e5-base/resolve/main".to_string(),
            [
                ("onnx/model_quantized.onnx", "model.onnx"),
                ("tokenizer.json", "tokenizer.json"),
            ],
        ),
        _ => (
            format!("https://huggingface.co/sentence-transformers/{model_name}/resolve/main"),
            [
                ("onnx/model.onnx", "model.onnx"),
                ("tokenizer.json", "tokenizer.json"),
            ],
        ),
    }
}

pub async fn download_model(config: &MindConfig) -> Result<()> {
    let model_dir = crate::config::models_dir().join(&config.model_name);
    std::fs::create_dir_all(&model_dir)?;

    let (base_url, files) = model_source(&config.model_name);

    for (remote_path, local_name) in &files {
        let url = format!("{base_url}/{remote_path}");
        let dest = model_dir.join(local_name);

        if dest.exists() {
            eprintln!("  {local_name} already exists, skipping.");
            continue;
        }

        let pin = model_file_pin(&config.model_name, local_name);
        if pin.is_none() {
            eprintln!(
                "  [warn] no pinned checksum for {local_name} (custom model) - integrity not verified"
            );
        }
        eprintln!("  Downloading {local_name}...");
        crate::util::download_file(&url, &dest, pin).await?;
    }

    eprintln!("  Model downloaded to {}", model_dir.display());
    Ok(())
}

const ORT_VERSION: &str = "1.24.2";

fn ort_lib_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "onnxruntime.dll"
    } else if cfg!(target_os = "macos") {
        "libonnxruntime.dylib"
    } else {
        "libonnxruntime.so"
    }
}

fn ort_lib_path() -> std::path::PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    exe.parent()
        .unwrap_or(std::path::Path::new("."))
        .join(ort_lib_name())
}

pub fn is_ort_available() -> bool {
    if std::env::var("ORT_DYLIB_PATH").is_ok() {
        return true;
    }
    ort_lib_path().exists()
}

/// Extract a single member from a .tar.gz into `dest` (native, audit #19).
///
/// Refuses to extract a symlink/hardlink entry as a regular file — that would
/// silently produce a 0-byte file (tar symlinks carry no body), which then
/// hangs `dlopen` if the destination is a shared library. The caller must ask
/// for the resolved versioned path instead.
pub fn extract_member_tar_gz(archive: &Path, member: &str, dest: &Path) -> Result<()> {
    use tar::EntryType;
    let file = std::fs::File::open(archive)?;
    let dec = flate2::read::GzDecoder::new(file);
    let mut tar = tar::Archive::new(dec);
    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_string_lossy().to_string();
        if path == member {
            let etype = entry.header().entry_type();
            if matches!(etype, EntryType::Symlink | EntryType::Link) {
                let target = entry
                    .link_name()
                    .ok()
                    .flatten()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                anyhow::bail!(
                    "Refusing to extract symlink/hardlink entry '{member}' (-> '{target}') \
                     as a regular file. Tar symlinks have no body — copying them produces \
                     a 0-byte file that hangs dlopen. Ask for the resolved versioned \
                     filename inside the archive."
                );
            }
            let mut out = std::fs::File::create(dest)?;
            std::io::copy(&mut entry, &mut out)?;
            return Ok(());
        }
    }
    anyhow::bail!("Member {member} not found in archive {}", archive.display())
}

/// Extract a single member from a .zip into `dest` (native, audit #19).
pub fn extract_member_zip(archive: &Path, member: &str, dest: &Path) -> Result<()> {
    let file = std::fs::File::open(archive)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let mut entry = zip
        .by_name(member)
        .with_context(|| format!("Member {member} not found in zip"))?;
    let mut out = std::fs::File::create(dest)?;
    std::io::copy(&mut entry, &mut out)?;
    Ok(())
}

pub async fn download_ort_runtime() -> Result<()> {
    let dest = ort_lib_path();
    if dest.exists() {
        eprintln!("  ONNX Runtime already exists at {}", dest.display());
        return Ok(());
    }

    let is_x64 = cfg!(target_arch = "x86_64");
    let (os_name, archive_ext, lib_path_in_archive, expected) = if cfg!(target_os = "windows") {
        let a = if is_x64 { "win-x64" } else { "win-arm64" };
        (
            format!("onnxruntime-{a}-{ORT_VERSION}"),
            "zip",
            format!("onnxruntime-{a}-{ORT_VERSION}/lib/onnxruntime.dll"),
            None,
        )
    } else if cfg!(target_os = "macos") {
        let a = if cfg!(target_arch = "aarch64") {
            "osx-arm64"
        } else {
            "osx-x86_64"
        };
        (
            format!("onnxruntime-{a}-{ORT_VERSION}"),
            "tgz",
            format!("onnxruntime-{a}-{ORT_VERSION}/lib/libonnxruntime.dylib"),
            None,
        )
    } else if cfg!(target_arch = "aarch64") {
        // The archive ships `libonnxruntime.so` as a SYMLINK to the versioned
        // file (`libonnxruntime.so.1.24.2`). Extracting a tar symlink with
        // `std::io::copy(&mut entry, &mut out)` yields a **0-byte regular
        // file**, because tar symlinks have no body — only metadata in the
        // header. `dlopen` then attempts the empty file and hangs forever on
        // some platforms (Ubuntu 24.04 RunPod containers, observed
        // 2026-06-02). Pull the actual versioned file by exact name.
        (
            format!("onnxruntime-linux-aarch64-{ORT_VERSION}"),
            "tgz",
            format!(
                "onnxruntime-linux-aarch64-{ORT_VERSION}/lib/libonnxruntime.so.{ORT_VERSION}"
            ),
            None,
        )
    } else {
        (
            format!("onnxruntime-linux-x64-{ORT_VERSION}"),
            "tgz",
            format!("onnxruntime-linux-x64-{ORT_VERSION}/lib/libonnxruntime.so.{ORT_VERSION}"),
            integrity::pin(integrity::ORT_LINUX_X64_1_24_2),
        )
    };

    let url = format!(
        "https://github.com/microsoft/onnxruntime/releases/download/v{ORT_VERSION}/{os_name}.{archive_ext}"
    );

    let tmp_dir = std::env::temp_dir().join("mgimind_ort_download");
    std::fs::create_dir_all(&tmp_dir)?;
    let archive_path = tmp_dir.join(format!("ort.{archive_ext}"));

    if expected.is_none() {
        eprintln!(
            "  [warn] no pinned checksum for this platform's ONNX Runtime - integrity not verified"
        );
    }
    eprintln!("  Downloading ONNX Runtime v{ORT_VERSION}...");
    crate::util::download_file(&url, &archive_path, expected).await?;

    eprintln!("  Extracting...");
    if archive_ext == "zip" {
        extract_member_zip(&archive_path, &lib_path_in_archive, &dest)?;
    } else {
        extract_member_tar_gz(&archive_path, &lib_path_in_archive, &dest)?;
    }

    if !dest.exists() {
        anyhow::bail!(
            "Extraction finished but library not found at {}",
            dest.display()
        );
    }
    eprintln!("  ONNX Runtime installed to {}", dest.display());

    // SAFETY: called during `doctor --fix` before any ORT usage.
    unsafe {
        std::env::set_var("ORT_DYLIB_PATH", &dest);
    }

    let _ = std::fs::remove_dir_all(&tmp_dir);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mean_pool_averages_masked_tokens() {
        // 2 tokens, hidden=2; second token masked out → result == first token.
        let data = [1.0, 2.0, 9.0, 9.0];
        let mask = [1i64, 0];
        assert_eq!(mean_pool(&data, &mask, 2, 2), vec![1.0, 2.0]);
        // both tokens active → component-wise average.
        assert_eq!(mean_pool(&data, &[1, 1], 2, 2), vec![5.0, 5.5]);
    }

    #[test]
    fn cls_pool_takes_first_token() {
        let data = [1.0, 2.0, 3.0, 4.0];
        assert_eq!(cls_pool(&data, 2), vec![1.0, 2.0]);
    }

    #[test]
    fn l2_normalize_unit_length() {
        let mut v = vec![3.0f32, 4.0];
        l2_normalize(&mut v);
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6);
        assert!((v[0] - 0.6).abs() < 1e-6 && (v[1] - 0.8).abs() < 1e-6);
    }
}
