use anyhow::{Context, Result};
use once_cell::sync::OnceCell;
use ort::session::Session;
use ort::value::Value;
use std::sync::Mutex;

use crate::config::MindConfig;

static SESSION: OnceCell<Mutex<Session>> = OnceCell::new();

fn get_model_path(config: &MindConfig) -> std::path::PathBuf {
    crate::config::models_dir().join(&config.model_name).join("model.onnx")
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

pub async fn embed(config: &MindConfig, text: &str) -> Result<Vec<f32>> {
    init_session(config)?;

    let session_lock = SESSION.get().unwrap();
    let mut session = session_lock.lock().map_err(|e| anyhow::anyhow!("Session lock poisoned: {e}"))?;

    let tokenizer_path = crate::config::models_dir()
        .join(&config.model_name)
        .join("tokenizer.json");

    let tokenizer = tokenizers::Tokenizer::from_file(&tokenizer_path)
        .map_err(|e| anyhow::anyhow!("Failed to load tokenizer: {e}"))?;

    let encoding = tokenizer
        .encode(text, true)
        .map_err(|e| anyhow::anyhow!("Tokenization failed: {e}"))?;

    let input_ids: Vec<i64> = encoding.get_ids().iter().map(|&id| id as i64).collect();
    let attention_mask: Vec<i64> = encoding.get_attention_mask().iter().map(|&m| m as i64).collect();
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

    let hidden_size = if shape.len() == 3 {
        shape[2] as usize
    } else {
        384
    };

    // Mean pooling over token dimension, masked by attention
    let mut pooled = vec![0.0f32; hidden_size];
    let mut total_weight = 0.0f32;

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

    // L2 normalize
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
        let file = local_name;
        let dest = model_dir.join(file);

        if dest.exists() {
            println!("  {file} already exists, skipping.");
            continue;
        }

        println!("  Downloading {file}...");
        let status = std::process::Command::new("curl")
            .args(["-sL", "-o", &dest.to_string_lossy(), &url])
            .status()
            .context(format!("Failed to download {file}"))?;

        if !status.success() {
            anyhow::bail!("Download failed for {file}");
        }
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
    // Place ORT library next to the executable
    let exe = std::env::current_exe().unwrap_or_default();
    exe.parent()
        .unwrap_or(std::path::Path::new("."))
        .join(ort_lib_name())
}

pub fn is_ort_available() -> bool {
    // Check if ORT_DYLIB_PATH is set, or if library exists next to exe
    if std::env::var("ORT_DYLIB_PATH").is_ok() {
        return true;
    }
    ort_lib_path().exists()
}

pub async fn download_ort_runtime() -> Result<()> {
    let dest = ort_lib_path();
    if dest.exists() {
        println!("  ONNX Runtime already exists at {}", dest.display());
        return Ok(());
    }

    let (os_name, archive_ext, lib_path_in_archive) = if cfg!(target_os = "windows") {
        if cfg!(target_arch = "x86_64") {
            (
                format!("onnxruntime-win-x64-{ORT_VERSION}"),
                "zip",
                format!("onnxruntime-win-x64-{ORT_VERSION}/lib/onnxruntime.dll"),
            )
        } else {
            (
                format!("onnxruntime-win-arm64-{ORT_VERSION}"),
                "zip",
                format!("onnxruntime-win-arm64-{ORT_VERSION}/lib/onnxruntime.dll"),
            )
        }
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            (
                format!("onnxruntime-osx-arm64-{ORT_VERSION}"),
                "tgz",
                format!("onnxruntime-osx-arm64-{ORT_VERSION}/lib/libonnxruntime.dylib"),
            )
        } else {
            (
                format!("onnxruntime-osx-x86_64-{ORT_VERSION}"),
                "tgz",
                format!("onnxruntime-osx-x86_64-{ORT_VERSION}/lib/libonnxruntime.dylib"),
            )
        }
    } else {
        // Linux
        if cfg!(target_arch = "aarch64") {
            (
                format!("onnxruntime-linux-aarch64-{ORT_VERSION}"),
                "tgz",
                format!("onnxruntime-linux-aarch64-{ORT_VERSION}/lib/libonnxruntime.so"),
            )
        } else {
            (
                format!("onnxruntime-linux-x64-{ORT_VERSION}"),
                "tgz",
                format!("onnxruntime-linux-x64-{ORT_VERSION}/lib/libonnxruntime.so"),
            )
        }
    };

    let url = format!(
        "https://github.com/microsoft/onnxruntime/releases/download/v{ORT_VERSION}/{os_name}.{archive_ext}"
    );

    let tmp_dir = std::env::temp_dir().join("mgimind_ort_download");
    std::fs::create_dir_all(&tmp_dir)?;
    let archive_path = tmp_dir.join(format!("ort.{archive_ext}"));

    println!("  Downloading ONNX Runtime v{ORT_VERSION}...");
    let status = std::process::Command::new("curl")
        .args(["-sL", "-o", &archive_path.to_string_lossy(), &url])
        .status()
        .context("Failed to download ONNX Runtime")?;

    if !status.success() {
        anyhow::bail!("ONNX Runtime download failed");
    }

    println!("  Extracting...");
    if archive_ext == "zip" {
        let status = std::process::Command::new("tar")
            .args([
                "-xf",
                &archive_path.to_string_lossy(),
                "-C",
                &tmp_dir.to_string_lossy(),
                &lib_path_in_archive,
            ])
            .status();

        // Fallback: try unzip if tar doesn't work with zip
        if status.is_err() || !status.unwrap().success() {
            let _ = std::process::Command::new("unzip")
                .args([
                    "-o",
                    &archive_path.to_string_lossy().to_string(),
                    &lib_path_in_archive,
                    "-d",
                    &tmp_dir.to_string_lossy().to_string(),
                ])
                .status()
                .context("Failed to extract ONNX Runtime")?;
        }
    } else {
        // tgz
        std::process::Command::new("tar")
            .args([
                "-xzf",
                &archive_path.to_string_lossy(),
                "-C",
                &tmp_dir.to_string_lossy(),
                &lib_path_in_archive,
            ])
            .status()
            .context("Failed to extract ONNX Runtime")?;
    }

    let extracted = tmp_dir.join(&lib_path_in_archive);
    if extracted.exists() {
        std::fs::copy(&extracted, &dest)?;
        println!("  ONNX Runtime installed to {}", dest.display());
    } else {
        anyhow::bail!(
            "Extraction succeeded but library not found at {}",
            extracted.display()
        );
    }

    // Set env var for current process
    // SAFETY: called during doctor --fix before any ORT usage
    unsafe { std::env::set_var("ORT_DYLIB_PATH", &dest); }

    // Cleanup
    let _ = std::fs::remove_dir_all(&tmp_dir);

    Ok(())
}
