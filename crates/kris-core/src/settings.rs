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
    /// Path to the llama-server binary, used by the `serve` command.
    pub llama_server_path: String,
    /// Path to the .gguf model file, used by the `serve` command.
    pub model_path: String,
    pub context_size: u32,
    /// Passed as `-t` if set; left to llama-server's own default otherwise.
    pub threads: Option<u32>,
    pub mlock: bool,
    /// Passed as `--flash-attn` if true. Speeds up attention with no quality
    /// loss, but the flag is only recognized by reasonably recent llama.cpp
    /// builds, so it's opt-in rather than default.
    pub flash_attn: bool,
    /// Passed as `--cache-type-k`/`--cache-type-v` if set (e.g. "q8_0").
    /// Quantizing the KV cache roughly halves its memory use for a small,
    /// usually unnoticeable quality cost - left unset to keep llama-server's
    /// f16 default otherwise.
    pub cache_type_k: Option<String>,
    pub cache_type_v: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        let llama_server_path = crate::home::home_dir()
            .map(|home| home.join("llama.cpp/build/bin/llama-server"))
            .map(|path| path.display().to_string())
            .unwrap_or_default();

        Self {
            llama_url: "http://127.0.0.1:8080".to_string(),
            model: "qwen2.5-coder-3b-instruct".to_string(),
            temperature: 0.2,
            // Kept modest by default: on CPU-only phone hardware, generation
            // speed is the bottleneck, so a lower cap keeps replies snappy.
            // Raise it with `config set max_tokens <n>` if answers get cut off.
            max_tokens: 1024,
            max_tool_iterations: 6,
            llama_server_path,
            model_path: String::new(),
            context_size: 4096,
            threads: None,
            mlock: false,
            flash_attn: false,
            cache_type_k: None,
            cache_type_v: None,
        }
    }
}

impl Settings {
    pub fn config_path() -> Option<PathBuf> {
        Some(crate::home::home_dir()?.join(".kris").join("config.toml"))
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
