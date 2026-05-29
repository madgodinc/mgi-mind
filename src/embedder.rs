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

fn get_model_path(config: &MindConfig) -> std::path::PathBuf {
    crate::config::models_dir()
        .join(&config.model_name)
        .join("model.onnx")
}

fn init_session(config: &MindConfig) -> Result<()> {
    if SESSION.get().is_some() {
        return Ok(());
    }

    let model_path = get_model_path(config);

    if !model_path.exists() {
        anyhow::bail!(
            "Model not found at {}. Run `mgimind doctor --fix` to download it.",
            model_path.display()
        );
    }

    let session = Session::builder()
        .map_err(|e| anyhow::anyhow!("Failed to create session builder: {e}"))?
        .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)
        .map_err(|e| anyhow::anyhow!("Failed to set optimization level: {e}"))?
        .commit_from_file(&model_path)
        .map_err(|e| anyhow::anyhow!("Failed to load ONNX model: {e}"))?;

    let _ = SESSION.set(Mutex::new(session));
    Ok(())
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

pub async fn embed(config: &MindConfig, text: &str) -> Result<Vec<f32>> {
    init_session(config)?;

    let session_lock = SESSION.get().unwrap();
    let mut session = session_lock
        .lock()
        .map_err(|e| anyhow::anyhow!("Session lock poisoned: {e}"))?;

    let tokenizer = get_tokenizer(config)?;

    let encoding = tokenizer
        .encode(text, true)
        .map_err(|e| anyhow::anyhow!("Tokenization failed: {e}"))?;

    let input_ids: Vec<i64> = encoding.get_ids().iter().map(|&id| id as i64).collect();
    let attention_mask: Vec<i64> = encoding
        .get_attention_mask()
        .iter()
        .map(|&m| m as i64)
        .collect();
    let token_type_ids: Vec<i64> = encoding.get_type_ids().iter().map(|&t| t as i64).collect();

    let seq_len = input_ids.len();

    let ids_value = Value::from_array(([1usize, seq_len], input_ids))
        .map_err(|e| anyhow::anyhow!("Failed to create input_ids tensor: {e}"))?;
    let mask_value = Value::from_array(([1usize, seq_len], attention_mask.clone()))
        .map_err(|e| anyhow::anyhow!("Failed to create attention_mask tensor: {e}"))?;
    let type_value = Value::from_array(([1usize, seq_len], token_type_ids))
        .map_err(|e| anyhow::anyhow!("Failed to create token_type_ids tensor: {e}"))?;

    use ort::session::SessionInputValue;
    let inputs: Vec<(std::borrow::Cow<'_, str>, SessionInputValue<'_>)> = vec![
        (std::borrow::Cow::from("input_ids"), ids_value.into()),
        (std::borrow::Cow::from("attention_mask"), mask_value.into()),
        (std::borrow::Cow::from("token_type_ids"), type_value.into()),
    ];

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

    // Mean pooling over token dimension, masked by attention.
    let mut pooled = vec![0.0f32; hidden_size];
    let mut total_weight = 0.0f32;

    // Index-based loop is clearest here (parallel index into the flat `data` buffer).
    #[allow(clippy::needless_range_loop)]
    for token_idx in 0..seq_len {
        let mask = attention_mask[token_idx] as f32;
        total_weight += mask;
        let offset = token_idx * hidden_size;
        for dim in 0..hidden_size {
            pooled[dim] += data[offset + dim] * mask;
        }
    }

    if total_weight > 0.0 {
        for v in &mut pooled {
            *v /= total_weight;
        }
    }

    // L2 normalize.
    let norm: f32 = pooled.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for v in &mut pooled {
            *v /= norm;
        }
    }

    Ok(pooled)
}

pub fn is_model_downloaded(config: &MindConfig) -> bool {
    let model_dir = crate::config::models_dir().join(&config.model_name);
    model_dir.join("model.onnx").exists() && model_dir.join("tokenizer.json").exists()
}

/// Look up the pinned checksum for the default model's files (audit #6).
/// Custom models have no pin (returns None → download with a warning).
fn model_file_pin(model_name: &str, local_name: &str) -> Option<&'static str> {
    if model_name == "all-MiniLM-L6-v2" {
        match local_name {
            "model.onnx" => integrity::pin(integrity::MODEL_MINILM_ONNX),
            "tokenizer.json" => integrity::pin(integrity::MODEL_MINILM_TOKENIZER),
            _ => None,
        }
    } else {
        None
    }
}

pub async fn download_model(config: &MindConfig) -> Result<()> {
    let model_dir = crate::config::models_dir().join(&config.model_name);
    std::fs::create_dir_all(&model_dir)?;

    let base_url = format!(
        "https://huggingface.co/sentence-transformers/{}/resolve/main",
        config.model_name
    );

    let files = [
        ("onnx/model.onnx", "model.onnx"),
        ("tokenizer.json", "tokenizer.json"),
    ];

    for (remote_path, local_name) in &files {
        let url = format!("{base_url}/{remote_path}");
        let dest = model_dir.join(local_name);

        if dest.exists() {
            println!("  {local_name} already exists, skipping.");
            continue;
        }

        let pin = model_file_pin(&config.model_name, local_name);
        if pin.is_none() {
            println!(
                "  [warn] no pinned checksum for {local_name} (custom model) — integrity not verified"
            );
        }
        println!("  Downloading {local_name}...");
        crate::util::download_file(&url, &dest, pin).await?;
    }

    println!("  Model downloaded to {}", model_dir.display());
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
pub fn extract_member_tar_gz(archive: &Path, member: &str, dest: &Path) -> Result<()> {
    let file = std::fs::File::open(archive)?;
    let dec = flate2::read::GzDecoder::new(file);
    let mut tar = tar::Archive::new(dec);
    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_string_lossy().to_string();
        if path == member {
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
        println!("  ONNX Runtime already exists at {}", dest.display());
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
        (
            format!("onnxruntime-linux-aarch64-{ORT_VERSION}"),
            "tgz",
            format!("onnxruntime-linux-aarch64-{ORT_VERSION}/lib/libonnxruntime.so"),
            None,
        )
    } else {
        (
            format!("onnxruntime-linux-x64-{ORT_VERSION}"),
            "tgz",
            format!("onnxruntime-linux-x64-{ORT_VERSION}/lib/libonnxruntime.so"),
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
        println!(
            "  [warn] no pinned checksum for this platform's ONNX Runtime — integrity not verified"
        );
    }
    println!("  Downloading ONNX Runtime v{ORT_VERSION}...");
    crate::util::download_file(&url, &archive_path, expected).await?;

    println!("  Extracting...");
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
    println!("  ONNX Runtime installed to {}", dest.display());

    // SAFETY: called during `doctor --fix` before any ORT usage.
    unsafe {
        std::env::set_var("ORT_DYLIB_PATH", &dest);
    }

    let _ = std::fs::remove_dir_all(&tmp_dir);
    Ok(())
}
