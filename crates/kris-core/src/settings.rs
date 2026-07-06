use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub llama_url: String,
    pub model: String,
    pub temperature: f32,
    pub max_tokens: u32,
    pub max_tool_iterations: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            llama_url: "http://127.0.0.1:8080".to_string(),
            model: "qwen2.5-coder-3b-instruct".to_string(),
            temperature: 0.2,
            max_tokens: 2048,
            max_tool_iterations: 6,
        }
    }
}

impl Settings {
    pub fn config_path() -> Option<PathBuf> {
        let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;

        Some(PathBuf::from(home).join(".kris").join("config.toml"))
    }

    pub fn load() -> Self {
        Self::config_path()
            .and_then(|path| fs::read_to_string(path).ok())
            .and_then(|content| toml::from_str(&content).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::config_path()
            .ok_or_else(|| anyhow::anyhow!("could not resolve a home directory"))?;

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        fs::write(path, toml::to_string_pretty(self)?)?;

        Ok(())
    }
}
