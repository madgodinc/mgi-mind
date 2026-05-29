use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

fn default_vector_size() -> u64 {
    384
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
}

impl Default for MindConfig {
    fn default() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            data_dir: mind_home(),
            model_name: "all-MiniLM-L6-v2".to_string(),
            qdrant_port: 6334,
            vector_size: default_vector_size(),
            qdrant_api_key: None,
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

pub fn mind_home() -> PathBuf {
    dirs::home_dir()
        .expect("Could not determine home directory")
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
    }
}
