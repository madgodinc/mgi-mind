//! Cross-encoder reranker (audit #22). Dense retrieval (bi-encoder cosine) is
//! fast but coarse; a cross-encoder scores each (query, passage) pair *jointly*
//! and re-orders the top-K far more accurately. Default model is
//! `bge-reranker-base` (XLM-R, multilingual incl. RU; quantized ONNX, CPU-ok).
//!
//! Best-effort: any failure (model missing, tokenize/inference error) returns an
//! error that `search` swallows, leaving the dense order untouched - reranking is
//! a quality boost, never a hard dependency.

use anyhow::{Result, anyhow};
use once_cell::sync::OnceCell;
use ort::session::Session;
use ort::value::Value;
use std::sync::Mutex;

use crate::config::MindConfig;

static RERANK_SESSION: OnceCell<Mutex<Session>> = OnceCell::new();
static RERANK_TOKENIZER: OnceCell<tokenizers::Tokenizer> = OnceCell::new();

/// XLM-RoBERTa pad token id (bge-reranker-base is XLM-R based).
const XLMR_PAD_ID: i64 = 1;

fn model_dir(config: &MindConfig) -> std::path::PathBuf {
    crate::config::models_dir().join(&config.rerank_model)
}

pub fn is_model_downloaded(config: &MindConfig) -> bool {
    let d = model_dir(config);
    d.join("model.onnx").exists() && d.join("tokenizer.json").exists()
}

fn init_session(config: &MindConfig) -> Result<()> {
    if RERANK_SESSION.get().is_some() {
        return Ok(());
    }
    let model_path = model_dir(config).join("model.onnx");
    if !model_path.exists() {
        anyhow::bail!(
            "Reranker model not found at {}. Run `mgimind doctor --fix`.",
            model_path.display()
        );
    }
    let session = Session::builder()
        .map_err(|e| anyhow!("rerank session builder: {e}"))?
        .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)
        .map_err(|e| anyhow!("rerank optimization level: {e}"))?
        .commit_from_file(&model_path)
        .map_err(|e| anyhow!("load reranker ONNX: {e}"))?;
    let _ = RERANK_SESSION.set(Mutex::new(session));
    Ok(())
}

fn get_tokenizer(config: &MindConfig) -> Result<&'static tokenizers::Tokenizer> {
    RERANK_TOKENIZER.get_or_try_init(|| {
        let path = model_dir(config).join("tokenizer.json");
        tokenizers::Tokenizer::from_file(&path).map_err(|e| anyhow!("rerank tokenizer: {e}"))
    })
}

/// Relevance score for each (query, passage) pair - higher = more relevant.
/// All pairs run in a single padded batch (one ONNX pass) so reranking K
/// candidates stays cheap on CPU.
pub async fn scores(config: &MindConfig, query: &str, passages: &[String]) -> Result<Vec<f32>> {
    if passages.is_empty() {
        return Ok(Vec::new());
    }
    init_session(config)?;
    let tokenizer = get_tokenizer(config)?;

    // Encode (query, passage) pairs → [CLS] query [SEP] passage [SEP].
    let mut encodings = Vec::with_capacity(passages.len());
    for p in passages {
        let enc = tokenizer
            .encode((query, p.as_str()), true)
            .map_err(|e| anyhow!("rerank tokenize: {e}"))?;
        encodings.push(enc);
    }

    let n = encodings.len();
    // Cap at the model's 512-position limit (longer pairs crash the ONNX Expand).
    let max_len = encodings
        .iter()
        .map(|e| e.get_ids().len())
        .max()
        .unwrap_or(0)
        .clamp(1, 512);

    // Right-pad every pair to max_len (pad id + mask 0) for a rectangular batch.
    let mut ids = vec![XLMR_PAD_ID; n * max_len];
    let mut mask = vec![0i64; n * max_len];
    for (i, enc) in encodings.iter().enumerate() {
        let eids = enc.get_ids();
        let emask = enc.get_attention_mask();
        for (j, (&id, &m)) in eids.iter().zip(emask.iter()).take(max_len).enumerate() {
            ids[i * max_len + j] = id as i64;
            mask[i * max_len + j] = m as i64;
        }
    }

    let session_lock = RERANK_SESSION.get().unwrap();
    let mut session = session_lock
        .lock()
        .map_err(|e| anyhow!("rerank lock poisoned: {e}"))?;

    let ids_value =
        Value::from_array(([n, max_len], ids)).map_err(|e| anyhow!("rerank ids tensor: {e}"))?;
    let mask_value =
        Value::from_array(([n, max_len], mask)).map_err(|e| anyhow!("rerank mask tensor: {e}"))?;

    use ort::session::SessionInputValue;
    let inputs: Vec<(std::borrow::Cow<'_, str>, SessionInputValue<'_>)> = vec![
        (std::borrow::Cow::from("input_ids"), ids_value.into()),
        (std::borrow::Cow::from("attention_mask"), mask_value.into()),
    ];

    let outputs = session
        .run(inputs)
        .map_err(|e| anyhow!("rerank inference: {e}"))?;
    let (shape, data) = outputs[0]
        .try_extract_tensor::<f32>()
        .map_err(|e| anyhow!("rerank output: {e}"))?;

    // Output is [n, labels] (bge-reranker: labels=1) - one relevance logit per pair.
    let labels = if shape.len() == 2 {
        (shape[1] as usize).max(1)
    } else {
        1
    };
    let out = (0..n).map(|i| data[i * labels]).collect();
    Ok(out)
}

/// Download the reranker ONNX (quantized) + tokenizer from the Xenova mirror.
pub async fn download_model(config: &MindConfig) -> Result<()> {
    let dir = model_dir(config);
    std::fs::create_dir_all(&dir)?;
    let base = match config.rerank_model.as_str() {
        "bge-reranker-base" => "https://huggingface.co/Xenova/bge-reranker-base/resolve/main",
        other => anyhow::bail!(
            "Unknown reranker model '{other}'. Place model.onnx + tokenizer.json in {} manually.",
            dir.display()
        ),
    };
    for (remote, local) in [
        ("onnx/model_quantized.onnx", "model.onnx"),
        ("tokenizer.json", "tokenizer.json"),
    ] {
        let dest = dir.join(local);
        if dest.exists() {
            println!("  {local} already exists, skipping.");
            continue;
        }
        println!("  Downloading reranker {local}...");
        crate::util::download_file(&format!("{base}/{remote}"), &dest, None).await?;
    }
    println!("  Reranker downloaded to {}", dir.display());
    Ok(())
}
