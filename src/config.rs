use anyhow::{Context, Result};
use once_cell::sync::OnceCell;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

fn default_vector_size() -> u64 {
    384
}

fn default_pooling() -> String {
    // MiniLM uses mean pooling; XLM-R models (e.g. bge-m3) use CLS pooling.
    "mean".to_string()
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MindConfig {
    pub version: String,
    pub data_dir: PathBuf,
    pub model_name: String,
    pub qdrant_port: u16,
    /// Embedding dimension. Stored so a model swap can be detected (audit #11).
    #[serde(default = "default_vector_size")]
    pub vector_size: u64,
    /// Optional Qdrant API key. When set, the bundled server is started with it
    /// and the client authenticates (audit #7). `None` = local-only, no auth.
    #[serde(default)]
    pub qdrant_api_key: Option<String>,
    /// Pooling strategy for the embedding model: "mean" (MiniLM / sentence-
    /// transformers) or "cls" (XLM-R models like bge-m3). Audit #21.
    #[serde(default = "default_pooling")]
    pub pooling: String,
    /// Whether the ONNX model expects a `token_type_ids` input. True for BERT-
    /// family (MiniLM); false for XLM-R (e5 / bge-m3). Audit #21.
    #[serde(default = "default_true")]
    pub uses_token_type_ids: bool,
    /// Prefix prepended to search queries before embedding. e5 models require
    /// "query: "; MiniLM uses "" (no prefix). Audit #21.
    #[serde(default)]
    pub query_prefix: String,
    /// Prefix prepended to stored documents before embedding. e5 models require
    /// "passage: "; MiniLM uses "". Audit #21.
    #[serde(default)]
    pub passage_prefix: String,
    /// Enable cross-encoder reranking (audit #22). ON by default - `bge-reranker-base`
    /// is strong on English (the target audience) and improves precision there.
    /// (It does degrade Russian ranking; if RU mattered, use a stronger multilingual
    /// reranker or turn this off.)
    #[serde(default = "default_true")]
    pub rerank_enabled: bool,
    /// Reranker model name (dir under models/). Audit #22.
    #[serde(default = "default_rerank_model")]
    pub rerank_model: String,
    /// How many dense candidates to fetch and rerank before returning `limit`.
    #[serde(default = "default_rerank_top_k")]
    pub rerank_top_k: usize,
    /// v1.5 Phase 6: install profile selecting per-mode confidence-score
    /// anchors. Default `chat-only` matches the legacy single-user behaviour
    /// — existing configs that pre-date v1.5 deserialise unchanged.
    #[serde(default)]
    pub install_mode: crate::install_mode::InstallMode,
}

fn default_rerank_model() -> String {
    "bge-reranker-base".to_string()
}

fn default_rerank_top_k() -> usize {
    20
}

impl Default for MindConfig {
    fn default() -> Self {
        // Default to multilingual-e5-base (audit #21): far better RU/EN retrieval
        // than the English-only MiniLM, practical on CPU (768-dim, ~278M, runs
        // quantized). e5 needs mean pooling, no token_type_ids, and query/passage
        // prefixes. Existing MiniLM configs keep their own values via serde.
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            data_dir: mind_home(),
            model_name: "multilingual-e5-base".to_string(),
            qdrant_port: 6334,
            vector_size: 768,
            qdrant_api_key: None,
            pooling: "mean".to_string(),
            uses_token_type_ids: false,
            query_prefix: "query: ".to_string(),
            passage_prefix: "passage: ".to_string(),
            rerank_enabled: true,
            rerank_model: default_rerank_model(),
            rerank_top_k: default_rerank_top_k(),
            install_mode: crate::install_mode::InstallMode::default(),
        }
    }
}

impl MindConfig {
    pub fn load() -> Result<Self> {
        let path = config_path();
        if !path.exists() {
            anyhow::bail!("{}", crate::error::MindError::NotInitialized);
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read config at {}", path.display()))?;
        let config: MindConfig = serde_json::from_str(&content)?;
        Ok(config)
    }

    pub fn save(&self) -> Result<()> {
        let path = config_path();
        let content = serde_json::to_string_pretty(self)?;
        crate::util::atomic_write_str(&path, &content)
    }
}

/// Process-global config cache. The data dir / model / ports don't change within
/// a session, so re-reading and re-parsing `config.json` on every `run_*` tool
/// call is wasted disk work now that `mgimind mcp` is one long-lived process.
static CONFIG_CACHE: OnceCell<MindConfig> = OnceCell::new();

/// Load the config once per process and reuse it. Returns a cheap clone of the
/// cached value, so call sites keep owning a `MindConfig` exactly as before -
/// only the disk read + JSON parse is memoized. A fresh process (every CLI
/// invocation, and a restarted MCP server) re-reads, so edits still take effect.
pub fn load_cached() -> Result<MindConfig> {
    CONFIG_CACHE.get_or_try_init(MindConfig::load).cloned()
}

pub fn mind_home() -> PathBuf {
    // An explicit override lets power users relocate the data dir and lets tests
    // isolate it on every OS. This is the only portable way: on Windows
    // `dirs::home_dir()` resolves the real user profile and ignores $HOME, so a
    // $HOME override (which works on Unix) cannot redirect the data dir there.
    if let Some(dir) = std::env::var_os("MGIMIND_HOME") {
        return PathBuf::from(dir);
    }
    // Never panic here: in the single long-lived `mgimind mcp` process an
    // `.expect()` would unwind through the read loop and kill the whole session
    // (a silent loss of memory access for a non-technical user). Fall back to the
    // current directory if the home dir genuinely can't be resolved - combined
    // with the per-tool panic isolation in `mcp::serve`, the server stays alive.
    dirs::home_dir()
        .unwrap_or_else(|| {
            eprintln!(
                "mgimind: could not determine home directory; falling back to ./mgimind. \
                 Set MGIMIND_HOME to choose the data dir explicitly."
            );
            PathBuf::from(".")
        })
        .join("mgimind")
}

pub fn config_path() -> PathBuf {
    mind_home().join("config.json")
}

pub fn sessions_dir() -> PathBuf {
    mind_home().join("sessions")
}

pub fn models_dir() -> PathBuf {
    mind_home().join("models")
}

pub fn is_initialized() -> bool {
    config_path().exists()
}

#[cfg(test)]
mod tests {
    use super::MindConfig;

    #[test]
    fn old_config_without_vector_size_defaults_to_384() {
        // A v0.1 config has no vector_size / qdrant_api_key fields.
        let json = r#"{"version":"0.1.0","data_dir":"/tmp/x","model_name":"all-MiniLM-L6-v2","qdrant_port":6334}"#;
        let cfg: MindConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.vector_size, 384);
        assert!(cfg.qdrant_api_key.is_none());
    }

    #[test]
    fn config_roundtrips() {
        let cfg = MindConfig::default();
        let s = serde_json::to_string(&cfg).unwrap();
        let back: MindConfig = serde_json::from_str(&s).unwrap();
        assert_eq!(back.vector_size, cfg.vector_size);
        assert_eq!(back.model_name, cfg.model_name);
        assert_eq!(back.pooling, cfg.pooling);
        assert_eq!(back.rerank_enabled, cfg.rerank_enabled);
    }

    #[test]
    fn default_is_e5_base_xlmr_shaped() {
        let cfg = MindConfig::default();
        assert_eq!(cfg.model_name, "multilingual-e5-base");
        assert_eq!(cfg.vector_size, 768);
        assert_eq!(cfg.pooling, "mean");
        assert!(!cfg.uses_token_type_ids, "XLM-R/e5 has no token_type_ids");
        assert_eq!(cfg.query_prefix, "query: ");
        assert_eq!(cfg.passage_prefix, "passage: ");
    }

    #[test]
    fn legacy_minilm_config_keeps_its_shape() {
        // An old MiniLM config must still load as MiniLM (mean pool, type_ids,
        // no prefixes) rather than inheriting the new e5 defaults.
        let json = r#"{"version":"0.1.0","data_dir":"/tmp/x","model_name":"all-MiniLM-L6-v2","qdrant_port":6334}"#;
        let cfg: MindConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.model_name, "all-MiniLM-L6-v2");
        assert!(cfg.uses_token_type_ids);
        assert_eq!(cfg.query_prefix, "");
    }

    /// v1.5 Phase 6: pre-v1.5 configs with no `install_mode` field must
    /// deserialise to `InstallMode::ChatOnly` (the single-user-chat
    /// default that matches legacy behaviour). Catches accidental
    /// breaking changes to the serde default.
    #[test]
    fn pre_v15_config_defaults_to_chat_only() {
        use crate::install_mode::InstallMode;
        let json = r#"{"version":"0.1.0","data_dir":"/tmp/x","model_name":"all-MiniLM-L6-v2","qdrant_port":6334}"#;
        let cfg: MindConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.install_mode, InstallMode::ChatOnly);
    }
}
