use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize)]
pub struct MindConfig {
    pub version: String,
    pub data_dir: PathBuf,
    pub model_name: String,
    pub qdrant_port: u16,
}

impl Default for MindConfig {
    fn default() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            data_dir: mind_home(),
            model_name: "all-MiniLM-L6-v2".to_string(),
            qdrant_port: 6334,
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
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, content)?;
        Ok(())
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
