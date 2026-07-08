use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Persisted at `~/.config/kris/config.toml`. Every field has a sane
/// default so a first run with no config file at all still works, as long
/// as `model_path` gets set (via `config set model_path ...` or by
/// `scripts/setup-termux.sh`, which writes it directly).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub model_path: String,
    pub llama_server_path: String,
    pub llama_url: String,
    pub context_size: u32,
    pub temperature: f32,
    pub max_tokens: u32,
    pub threads: Option<u32>,
    pub mlock: bool,
    pub flash_attn: bool,
    pub cache_type_k: Option<String>,
    pub cache_type_v: Option<String>,
    /// Parent folder holding every project - what the `project` command
    /// lists and picks from. Every project lives as a direct subfolder of
    /// this one; there is no separate single-project directory anymore.
    pub workspace: String,
    /// Name of the subfolder of `workspace` that's currently active, or
    /// empty if none has been picked yet - in which case the agent
    /// operates directly on `workspace` itself (e.g. to scaffold the
    /// first project into it).
    pub active_project: String,
    /// When true, every tool that would normally ask for a y/N
    /// confirmation (filesystem edits, run_command) executes immediately
    /// instead - equivalent to having answered "always" at the start of
    /// every session. Off by default since it removes the only safety
    /// net against a model acting on the project unsupervised.
    pub bypass_permissions: bool,
}

impl Default for Settings {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));

        Self {
            model_path: String::new(),
            llama_server_path: home
                .join("llama.cpp/build/bin/llama-server")
                .display()
                .to_string(),
            llama_url: "http://127.0.0.1:8080".to_string(),
            context_size: 8192,
            temperature: 0.2,
            max_tokens: 4096,
            threads: None,
            mlock: false,
            flash_attn: true,
            cache_type_k: Some("q8_0".to_string()),
            cache_type_v: Some("q8_0".to_string()),
            workspace: home.join("workspace").display().to_string(),
            active_project: String::new(),
            bypass_permissions: false,
        }
    }
}

fn config_path() -> Result<PathBuf> {
    let base = dirs::home_dir().context("could not determine home directory")?;
    Ok(base.join(".config").join("kris").join("config.toml"))
}

impl Settings {
    pub fn load() -> Result<Self> {
        let path = config_path()?;

        if !path.is_file() {
            return Ok(Self::default());
        }

        let raw =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;

        toml_parse(&raw)
    }

    pub fn save(&self) -> Result<()> {
        let path = config_path()?;

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        fs::write(&path, toml_render(self))
            .with_context(|| format!("writing {}", path.display()))?;

        Ok(())
    }

    /// Sets a field by name from a raw string value, used by the `config
    /// set <key> <value>` REPL command. Kept as a manual match instead of
    /// reflection so unknown keys give a clear error.
    pub fn set_field(&mut self, key: &str, value: &str) -> Result<()> {
        match key {
            "model_path" => self.model_path = value.to_string(),
            "llama_server_path" => self.llama_server_path = value.to_string(),
            "llama_url" => self.llama_url = value.to_string(),
            "context_size" => {
                let parsed: u32 = value.parse().context("expected an integer")?;
                if parsed == 0 {
                    anyhow::bail!("context_size must be greater than 0");
                }
                self.context_size = parsed;
            }
            "temperature" => {
                let parsed: f32 = value.parse().context("expected a number")?;
                if !(0.0..=2.0).contains(&parsed) {
                    anyhow::bail!("temperature must be between 0.0 and 2.0");
                }
                self.temperature = parsed;
            }
            "max_tokens" => {
                let parsed: u32 = value.parse().context("expected an integer")?;
                if parsed == 0 {
                    anyhow::bail!("max_tokens must be greater than 0");
                }
                self.max_tokens = parsed;
            }
            "threads" => {
                let parsed: u32 = value.parse().context("expected an integer")?;
                if parsed == 0 {
                    anyhow::bail!("threads must be greater than 0 (unset it instead to use the default)");
                }
                self.threads = Some(parsed);
            }
            "mlock" => self.mlock = value.parse().context("expected true or false")?,
            "flash_attn" => self.flash_attn = value.parse().context("expected true or false")?,
            "cache_type_k" => self.cache_type_k = Some(value.to_string()),
            "cache_type_v" => self.cache_type_v = Some(value.to_string()),
            "workspace" => self.workspace = value.to_string(),
            "active_project" => self.active_project = value.to_string(),
            // Legacy key from before this file's fields were renamed -
            // silently ignored (not rejected, so an old config.toml still
            // loads) rather than mapped onto `workspace`. It used to
            // default to the whole home directory for anyone who hadn't
            // customized it, which is far too broad to adopt as the new
            // workspace root; the old `workspace` key (the single active
            // project's own directory) usually already sits inside the
            // right parent folder and is a much safer value to keep.
            "projects_root" => {}
            "bypass_permissions" => {
                self.bypass_permissions = value.parse().context("expected true or false")?
            }
            other => anyhow::bail!("unknown config key \"{other}\""),
        }

        Ok(())
    }

    pub fn describe(&self) -> String {
        toml_render(self)
    }

    /// Soft warnings about combinations that parse fine individually but
    /// don't make sense together - printed at startup, not enforced,
    /// since llama-server would otherwise just fail confusingly deep
    /// into a request instead of up front.
    pub fn sanity_warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();

        if self.max_tokens >= self.context_size {
            warnings.push(format!(
                "max_tokens ({}) is >= context_size ({}) - there'd be no room left for the \
                 conversation itself. Consider lowering max_tokens or raising context_size.",
                self.max_tokens, self.context_size
            ));
        }

        warnings
    }
}

/// Tiny hand-rolled TOML reader/writer covering exactly the flat
/// string/int/float/bool/option shape `Settings` uses, so the crate
/// doesn't need a full `toml` dependency just to persist a dozen scalar
/// fields (keeps the dependency tree, and thus Termux build time, down).
fn toml_render(settings: &Settings) -> String {
    let mut out = String::new();
    out.push_str(&format!("model_path = {:?}\n", settings.model_path));
    out.push_str(&format!(
        "llama_server_path = {:?}\n",
        settings.llama_server_path
    ));
    out.push_str(&format!("llama_url = {:?}\n", settings.llama_url));
    out.push_str(&format!("context_size = {}\n", settings.context_size));
    out.push_str(&format!("temperature = {}\n", settings.temperature));
    out.push_str(&format!("max_tokens = {}\n", settings.max_tokens));
    if let Some(threads) = settings.threads {
        out.push_str(&format!("threads = {threads}\n"));
    }
    out.push_str(&format!("mlock = {}\n", settings.mlock));
    out.push_str(&format!("flash_attn = {}\n", settings.flash_attn));
    if let Some(v) = &settings.cache_type_k {
        out.push_str(&format!("cache_type_k = {v:?}\n"));
    }
    if let Some(v) = &settings.cache_type_v {
        out.push_str(&format!("cache_type_v = {v:?}\n"));
    }
    out.push_str(&format!("workspace = {:?}\n", settings.workspace));
    out.push_str(&format!("active_project = {:?}\n", settings.active_project));
    out.push_str(&format!(
        "bypass_permissions = {}\n",
        settings.bypass_permissions
    ));
    out
}

fn toml_parse(raw: &str) -> Result<Settings> {
    let mut settings = Settings::default();

    for (lineno, line) in raw.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let (key, value) = line
            .split_once('=')
            .with_context(|| format!("config.toml line {}: expected `key = value`", lineno + 1))?;

        let key = key.trim();
        let value = value.trim();
        let unquoted = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .unwrap_or(value);

        settings.set_field(key, unquoted)?;
    }

    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_render_and_parse() {
        let settings = Settings {
            model_path: "/data/model.gguf".to_string(),
            context_size: 4096,
            threads: Some(4),
            ..Settings::default()
        };

        let rendered = toml_render(&settings);
        let parsed = toml_parse(&rendered).unwrap();

        assert_eq!(parsed.model_path, "/data/model.gguf");
        assert_eq!(parsed.context_size, 4096);
        assert_eq!(parsed.threads, Some(4));
    }

    #[test]
    fn legacy_projects_root_key_does_not_overwrite_workspace() {
        // Simulates an old config.toml: "workspace" was the single active
        // project's own directory (a sane value to keep as the new
        // "parent folder" meaning too, since it usually already contains
        // whatever project the user had), while "projects_root" was a
        // separate, often-untouched field defaulting to the whole home
        // directory - far too broad to inherit as the new workspace root.
        let raw = "workspace = \"/home/user/project\"\nprojects_root = \"/home/user\"\n";
        let parsed = toml_parse(raw).unwrap();

        assert_eq!(parsed.workspace, "/home/user/project");
    }

    #[test]
    fn set_field_rejects_unknown_key() {
        let mut settings = Settings::default();
        assert!(settings.set_field("does_not_exist", "x").is_err());
    }
}
