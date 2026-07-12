use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Which backend serves the model: a local `llama-server` (fully offline),
/// an online OpenAI-compatible API (Gemini's compatibility endpoint, or
/// OpenRouter's), or Claude's native Messages API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Local,
    Gemini,
    Claude,
    OpenRouter,
}

impl Provider {
    fn as_str(self) -> &'static str {
        match self {
            Provider::Local => "local",
            Provider::Gemini => "gemini",
            Provider::Claude => "claude",
            Provider::OpenRouter => "openrouter",
        }
    }

    /// Accepts the internal names plus the friendlier "offline"/"online"
    /// aliases the `mode` command speaks, so `config set provider online`
    /// and `mode online` land on the same value. "online" stays mapped to
    /// Gemini specifically (its long-standing meaning here) - Claude and
    /// OpenRouter are only ever selected by their own name(s), not the
    /// generic alias.
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "local" | "offline" | "llama" => Some(Provider::Local),
            "gemini" | "online" => Some(Provider::Gemini),
            "claude" | "anthropic" => Some(Provider::Claude),
            "openrouter" | "or" => Some(Provider::OpenRouter),
            _ => None,
        }
    }
}

/// Persisted at `~/.config/kris/config.toml`. Every field has a sane
/// default so a first run with no config file at all still works, as long
/// as `model_path` gets set (via `config set model_path ...` or by
/// `scripts/setup-termux.sh`, which writes it directly).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Selects offline (`local` llama-server) vs online (`gemini`) at
    /// runtime, so the same install can switch between the two without
    /// re-editing anything but this one value.
    pub provider: Provider,
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
    /// OpenAI-compatible base URL for the online provider (Gemini's compat
    /// endpoint by default). The client appends `/chat/completions`.
    pub gemini_url: String,
    /// Model id sent in online requests, e.g. `gemini-2.5-flash`.
    pub gemini_model: String,
    /// API key for the online provider. Left empty by default: the
    /// `GEMINI_API_KEY` environment variable is preferred and checked
    /// first, so the key need not be written to disk in plain text at all.
    pub gemini_api_key: String,
    /// Context-window budget used for history trimming in online mode,
    /// kept separate from `context_size` (which doubles as llama-server's
    /// `-c` allocation) so a large online window doesn't make the local
    /// server try to reserve gigabytes of KV cache.
    pub gemini_context_size: u32,
    /// Base URL for Claude's native Messages API.
    pub claude_url: String,
    /// Model id sent in Claude requests, e.g. `claude-sonnet-5`.
    pub claude_model: String,
    /// API key for Claude. Left empty by default: the `ANTHROPIC_API_KEY`
    /// environment variable is preferred and checked first, so the key
    /// need not be written to disk in plain text at all. Never set this
    /// from a hardcoded value in source - only from the environment or a
    /// value the user types in themselves.
    pub claude_api_key: String,
    /// Context-window budget for Claude, tracked separately for the same
    /// reason as `gemini_context_size`.
    pub claude_context_size: u32,
    /// OpenAI-compatible base URL for OpenRouter, which fronts many
    /// different model providers behind one API and key.
    pub openrouter_url: String,
    /// Model id sent in OpenRouter requests, e.g.
    /// `anthropic/claude-sonnet-5` or `openai/gpt-5`.
    pub openrouter_model: String,
    /// API key for OpenRouter. Left empty by default: the
    /// `OPENROUTER_API_KEY` environment variable is preferred and checked
    /// first, so the key need not be written to disk in plain text at all.
    pub openrouter_api_key: String,
    /// Context-window budget for OpenRouter, tracked separately for the
    /// same reason as `gemini_context_size` - it varies a lot by whichever
    /// model is selected behind it.
    pub openrouter_context_size: u32,
    /// OpenRouter's `reasoning.effort` override: one of `"none"`,
    /// `"minimal"`, `"low"`, `"medium"`, `"high"`, or empty to omit the
    /// field entirely (provider/model default). Reasoning models routed
    /// through OpenRouter (e.g. Tencent's Hy3) can otherwise spend their
    /// whole `max_tokens` budget on a hidden "thinking" trace and never
    /// reach a visible answer or tool call - capping effort here leaves
    /// more of that budget for the actual response. Empty by default since
    /// not every model on OpenRouter supports or wants this field.
    pub openrouter_reasoning_effort: String,
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
    /// Narrower sibling of `bypass_permissions`: auto-approves only the
    /// file-editing tools (write_file, edit_file, delete_file,
    /// delete_directory, move_file, create_directory), leaving
    /// run_command and git_commit still asking for confirmation. Lets
    /// someone skip the diff prompt for routine edits while keeping a
    /// manual gate on anything that runs a shell command. Has no effect
    /// when `bypass_permissions` is already true.
    pub auto_approve_edits: bool,
}

impl Default for Settings {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));

        Self {
            provider: Provider::Local,
            model_path: String::new(),
            llama_server_path: home
                .join("llama.cpp/build/bin/llama-server")
                .display()
                .to_string(),
            llama_url: "http://127.0.0.1:8080".to_string(),
            context_size: 8192,
            temperature: 0.2,
            // Bounds how long a single reply can run if the model ever
            // latches onto a repetitive loop and never emits a stop
            // token - at phone-CPU decode speeds, 4096 tokens of that
            // could mean tens of minutes stuck "thinking" before the cap
            // even kicks in. Raise via `config set max_tokens <n>` for a
            // task that genuinely needs longer replies.
            max_tokens: 1024,
            threads: None,
            mlock: false,
            flash_attn: true,
            cache_type_k: Some("q8_0".to_string()),
            cache_type_v: Some("q8_0".to_string()),
            gemini_url: "https://generativelanguage.googleapis.com/v1beta/openai".to_string(),
            gemini_model: "gemini-2.5-flash".to_string(),
            gemini_api_key: String::new(),
            gemini_context_size: 128_000,
            claude_url: "https://api.anthropic.com".to_string(),
            claude_model: "claude-sonnet-5".to_string(),
            claude_api_key: String::new(),
            claude_context_size: 200_000,
            openrouter_url: "https://openrouter.ai/api/v1".to_string(),
            openrouter_model: "openai/gpt-5".to_string(),
            openrouter_api_key: String::new(),
            openrouter_context_size: 128_000,
            openrouter_reasoning_effort: String::new(),
            workspace: home.join("projects").display().to_string(),
            active_project: String::new(),
            bypass_permissions: false,
            auto_approve_edits: false,
        }
    }
}

fn config_path() -> Result<PathBuf> {
    let base = dirs::home_dir().context("could not determine home directory")?;
    Ok(base.join(".config").join("kris").join("config.toml"))
}

/// Resolves a workspace path to an absolute one, expanding a leading `~`
/// and anchoring any relative path at the home directory - deliberately
/// never at the process's current working directory. Without this, a
/// relative `workspace` (from an old config, or a bare name typed at the
/// `workspace`/`config set workspace` prompt) would resolve against
/// wherever the `kris` binary happened to be launched from - e.g. from
/// inside its own cloned source repo - silently writing every generated
/// project file into that repo instead of a real workspace folder.
fn normalize_workspace_path(value: &str) -> String {
    let home = dirs::home_dir();

    let expanded = if value == "~" {
        home.clone().unwrap_or_else(|| PathBuf::from(value))
    } else if let Some(rest) = value.strip_prefix("~/") {
        home.as_ref()
            .map(|h| h.join(rest))
            .unwrap_or_else(|| PathBuf::from(value))
    } else {
        PathBuf::from(value)
    };

    if expanded.is_absolute() {
        return expanded.display().to_string();
    }

    match home {
        Some(h) => h.join(expanded).display().to_string(),
        None => expanded.display().to_string(),
    }
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
            "provider" => {
                self.provider = Provider::parse(value).with_context(|| {
                    format!(
                        "expected local/offline, gemini/online, claude, or openrouter, got \"{value}\""
                    )
                })?;
            }
            "gemini_url" => self.gemini_url = value.to_string(),
            "gemini_model" => self.gemini_model = value.to_string(),
            "gemini_api_key" => self.gemini_api_key = value.to_string(),
            "gemini_context_size" => {
                let parsed: u32 = value.parse().context("expected an integer")?;
                if parsed == 0 {
                    anyhow::bail!("gemini_context_size must be greater than 0");
                }
                self.gemini_context_size = parsed;
            }
            "claude_url" => self.claude_url = value.to_string(),
            "claude_model" => self.claude_model = value.to_string(),
            "claude_api_key" => self.claude_api_key = value.to_string(),
            "claude_context_size" => {
                let parsed: u32 = value.parse().context("expected an integer")?;
                if parsed == 0 {
                    anyhow::bail!("claude_context_size must be greater than 0");
                }
                self.claude_context_size = parsed;
            }
            "openrouter_url" => self.openrouter_url = value.to_string(),
            "openrouter_model" => self.openrouter_model = value.to_string(),
            "openrouter_api_key" => self.openrouter_api_key = value.to_string(),
            "openrouter_context_size" => {
                let parsed: u32 = value.parse().context("expected an integer")?;
                if parsed == 0 {
                    anyhow::bail!("openrouter_context_size must be greater than 0");
                }
                self.openrouter_context_size = parsed;
            }
            "openrouter_reasoning_effort" => {
                const ALLOWED: &[&str] = &["", "none", "minimal", "low", "medium", "high"];
                let normalized = value.trim().to_ascii_lowercase();
                if !ALLOWED.contains(&normalized.as_str()) {
                    anyhow::bail!(
                        "openrouter_reasoning_effort must be one of: none, minimal, low, \
                         medium, high, or empty to unset - got \"{value}\""
                    );
                }
                self.openrouter_reasoning_effort = normalized;
            }
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
                    anyhow::bail!(
                        "threads must be greater than 0 (unset it instead to use the default)"
                    );
                }
                self.threads = Some(parsed);
            }
            "mlock" => self.mlock = value.parse().context("expected true or false")?,
            "flash_attn" => self.flash_attn = value.parse().context("expected true or false")?,
            "cache_type_k" => self.cache_type_k = Some(value.to_string()),
            "cache_type_v" => self.cache_type_v = Some(value.to_string()),
            "workspace" => self.workspace = normalize_workspace_path(value),
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
            "auto_approve_edits" => {
                self.auto_approve_edits = value.parse().context("expected true or false")?
            }
            other => anyhow::bail!("unknown config key \"{other}\""),
        }

        Ok(())
    }

    pub fn describe(&self) -> String {
        // Redacted so `config` doesn't print the API key to the terminal;
        // `save` still writes the real value via toml_render directly.
        toml_render_inner(self, true)
    }

    /// Resolves the active online provider's API key, preferring its
    /// environment variable (`GEMINI_API_KEY` / `ANTHROPIC_API_KEY` /
    /// `OPENROUTER_API_KEY`) over the persisted config value so the key
    /// need never be written to disk at all. Returns `None` for the local
    /// provider, which needs no key.
    pub fn resolved_api_key(&self) -> Option<String> {
        let (env_var, configured) = match self.provider {
            Provider::Local => return None,
            Provider::Gemini => ("GEMINI_API_KEY", &self.gemini_api_key),
            Provider::Claude => ("ANTHROPIC_API_KEY", &self.claude_api_key),
            Provider::OpenRouter => ("OPENROUTER_API_KEY", &self.openrouter_api_key),
        };

        if let Ok(key) = std::env::var(env_var) {
            if !key.trim().is_empty() {
                return Some(key);
            }
        }
        let key = configured.trim();
        (!key.is_empty()).then(|| key.to_string())
    }

    /// Context-window budget the history-trimmer should respect for the
    /// active provider - each online window is tracked separately from the
    /// local llama-server allocation (and from each other).
    pub fn effective_context_size(&self) -> u32 {
        match self.provider {
            Provider::Local => self.context_size,
            Provider::Gemini => self.gemini_context_size,
            Provider::Claude => self.claude_context_size,
            Provider::OpenRouter => self.openrouter_context_size,
        }
    }

    /// Soft warnings about combinations that parse fine individually but
    /// don't make sense together - printed at startup, not enforced,
    /// since llama-server would otherwise just fail confusingly deep
    /// into a request instead of up front.
    pub fn sanity_warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();

        if self.max_tokens >= self.effective_context_size() {
            warnings.push(format!(
                "max_tokens ({}) is >= the context window ({}) - there'd be no room left for \
                 the conversation itself. Consider lowering max_tokens or raising the context \
                 size.",
                self.max_tokens,
                self.effective_context_size()
            ));
        }

        if self.provider == Provider::Gemini && self.resolved_api_key().is_none() {
            warnings.push(
                "online mode (provider = gemini) is selected but no API key is set - export \
                 GEMINI_API_KEY, or run `config set gemini_api_key <key>`."
                    .to_string(),
            );
        }

        if self.provider == Provider::Claude && self.resolved_api_key().is_none() {
            warnings.push(
                "Claude mode (provider = claude) is selected but no API key is set - export \
                 ANTHROPIC_API_KEY, or run `config set claude_api_key <key>`."
                    .to_string(),
            );
        }

        if self.provider == Provider::OpenRouter && self.resolved_api_key().is_none() {
            warnings.push(
                "OpenRouter mode (provider = openrouter) is selected but no API key is set - \
                 export OPENROUTER_API_KEY, or run `config set openrouter_api_key <key>`."
                    .to_string(),
            );
        }

        warnings
    }
}

/// Tiny hand-rolled TOML reader/writer covering exactly the flat
/// string/int/float/bool/option shape `Settings` uses, so the crate
/// doesn't need a full `toml` dependency just to persist a dozen scalar
/// fields (keeps the dependency tree, and thus Termux build time, down).
fn toml_render(settings: &Settings) -> String {
    toml_render_inner(settings, false)
}

fn toml_render_inner(settings: &Settings, redact: bool) -> String {
    let mut out = String::new();
    out.push_str(&format!("provider = {:?}\n", settings.provider.as_str()));
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
    out.push_str(&format!("gemini_url = {:?}\n", settings.gemini_url));
    out.push_str(&format!("gemini_model = {:?}\n", settings.gemini_model));
    let api_key = if redact && !settings.gemini_api_key.is_empty() {
        "***".to_string()
    } else {
        settings.gemini_api_key.clone()
    };
    out.push_str(&format!("gemini_api_key = {api_key:?}\n"));
    out.push_str(&format!(
        "gemini_context_size = {}\n",
        settings.gemini_context_size
    ));
    out.push_str(&format!("claude_url = {:?}\n", settings.claude_url));
    out.push_str(&format!("claude_model = {:?}\n", settings.claude_model));
    let claude_api_key = if redact && !settings.claude_api_key.is_empty() {
        "***".to_string()
    } else {
        settings.claude_api_key.clone()
    };
    out.push_str(&format!("claude_api_key = {claude_api_key:?}\n"));
    out.push_str(&format!(
        "claude_context_size = {}\n",
        settings.claude_context_size
    ));
    out.push_str(&format!("openrouter_url = {:?}\n", settings.openrouter_url));
    out.push_str(&format!(
        "openrouter_model = {:?}\n",
        settings.openrouter_model
    ));
    let openrouter_api_key = if redact && !settings.openrouter_api_key.is_empty() {
        "***".to_string()
    } else {
        settings.openrouter_api_key.clone()
    };
    out.push_str(&format!("openrouter_api_key = {openrouter_api_key:?}\n"));
    out.push_str(&format!(
        "openrouter_context_size = {}\n",
        settings.openrouter_context_size
    ));
    out.push_str(&format!(
        "openrouter_reasoning_effort = {:?}\n",
        settings.openrouter_reasoning_effort
    ));
    out.push_str(&format!("workspace = {:?}\n", settings.workspace));
    out.push_str(&format!("active_project = {:?}\n", settings.active_project));
    out.push_str(&format!(
        "bypass_permissions = {}\n",
        settings.bypass_permissions
    ));
    out.push_str(&format!(
        "auto_approve_edits = {}\n",
        settings.auto_approve_edits
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

    #[test]
    fn provider_accepts_friendly_aliases() {
        let mut settings = Settings::default();
        assert_eq!(settings.provider, Provider::Local);

        settings.set_field("provider", "online").unwrap();
        assert_eq!(settings.provider, Provider::Gemini);
        settings.set_field("provider", "offline").unwrap();
        assert_eq!(settings.provider, Provider::Local);
        settings.set_field("provider", "gemini").unwrap();
        assert_eq!(settings.provider, Provider::Gemini);
        settings.set_field("provider", "claude").unwrap();
        assert_eq!(settings.provider, Provider::Claude);
        settings.set_field("provider", "anthropic").unwrap();
        assert_eq!(settings.provider, Provider::Claude);
        settings.set_field("provider", "openrouter").unwrap();
        assert_eq!(settings.provider, Provider::OpenRouter);
        settings.set_field("provider", "or").unwrap();
        assert_eq!(settings.provider, Provider::OpenRouter);

        assert!(settings.set_field("provider", "nonsense").is_err());
    }

    #[test]
    fn provider_and_online_fields_round_trip() {
        let settings = Settings {
            provider: Provider::Gemini,
            gemini_model: "gemini-2.5-pro".to_string(),
            gemini_context_size: 200_000,
            ..Settings::default()
        };

        let parsed = toml_parse(&toml_render(&settings)).unwrap();
        assert_eq!(parsed.provider, Provider::Gemini);
        assert_eq!(parsed.gemini_model, "gemini-2.5-pro");
        assert_eq!(parsed.gemini_context_size, 200_000);
    }

    #[test]
    fn effective_context_size_follows_provider() {
        let mut settings = Settings {
            context_size: 8192,
            gemini_context_size: 128_000,
            claude_context_size: 200_000,
            openrouter_context_size: 64_000,
            ..Settings::default()
        };
        assert_eq!(settings.effective_context_size(), 8192);
        settings.provider = Provider::Gemini;
        assert_eq!(settings.effective_context_size(), 128_000);
        settings.provider = Provider::Claude;
        assert_eq!(settings.effective_context_size(), 200_000);
        settings.provider = Provider::OpenRouter;
        assert_eq!(settings.effective_context_size(), 64_000);
    }

    #[test]
    fn workspace_relative_path_anchors_at_home_not_cwd() {
        // Regression test: a relative workspace used to resolve against
        // whatever directory the `kris` process happened to be launched
        // from - e.g. its own cloned source repo - instead of always
        // landing under the home directory.
        let mut settings = Settings::default();
        settings.set_field("workspace", "myproject").unwrap();

        let home = dirs::home_dir().unwrap();
        assert_eq!(
            settings.workspace,
            home.join("myproject").display().to_string()
        );
    }

    #[test]
    fn workspace_tilde_expands_to_home() {
        let mut settings = Settings::default();
        settings.set_field("workspace", "~/projects").unwrap();

        let home = dirs::home_dir().unwrap();
        assert_eq!(
            settings.workspace,
            home.join("projects").display().to_string()
        );
    }

    #[test]
    fn workspace_absolute_path_is_left_unchanged() {
        let mut settings = Settings::default();
        settings.set_field("workspace", "/data/workspace").unwrap();
        assert_eq!(settings.workspace, "/data/workspace");
    }

    #[test]
    fn auto_approve_edits_round_trips_and_defaults_off() {
        let mut settings = Settings::default();
        assert!(!settings.auto_approve_edits);

        settings.set_field("auto_approve_edits", "true").unwrap();
        assert!(settings.auto_approve_edits);

        let parsed = toml_parse(&toml_render(&settings)).unwrap();
        assert!(parsed.auto_approve_edits);
    }

    #[test]
    fn claude_fields_round_trip() {
        let settings = Settings {
            provider: Provider::Claude,
            claude_model: "claude-opus-4-8".to_string(),
            claude_context_size: 200_000,
            ..Settings::default()
        };

        let parsed = toml_parse(&toml_render(&settings)).unwrap();
        assert_eq!(parsed.provider, Provider::Claude);
        assert_eq!(parsed.claude_model, "claude-opus-4-8");
        assert_eq!(parsed.claude_context_size, 200_000);
    }

    #[test]
    fn openrouter_fields_round_trip() {
        let settings = Settings {
            provider: Provider::OpenRouter,
            openrouter_model: "anthropic/claude-sonnet-5".to_string(),
            openrouter_context_size: 100_000,
            ..Settings::default()
        };

        let parsed = toml_parse(&toml_render(&settings)).unwrap();
        assert_eq!(parsed.provider, Provider::OpenRouter);
        assert_eq!(parsed.openrouter_model, "anthropic/claude-sonnet-5");
        assert_eq!(parsed.openrouter_context_size, 100_000);
    }

    #[test]
    fn openrouter_reasoning_effort_round_trips_and_validates() {
        let mut settings = Settings::default();
        assert_eq!(settings.openrouter_reasoning_effort, "");

        settings
            .set_field("openrouter_reasoning_effort", "low")
            .unwrap();
        assert_eq!(settings.openrouter_reasoning_effort, "low");

        let parsed = toml_parse(&toml_render(&settings)).unwrap();
        assert_eq!(parsed.openrouter_reasoning_effort, "low");

        // Case-insensitive, and empty clears it back to "no override".
        settings
            .set_field("openrouter_reasoning_effort", "NONE")
            .unwrap();
        assert_eq!(settings.openrouter_reasoning_effort, "none");
        settings
            .set_field("openrouter_reasoning_effort", "")
            .unwrap();
        assert_eq!(settings.openrouter_reasoning_effort, "");

        assert!(settings
            .set_field("openrouter_reasoning_effort", "extreme")
            .is_err());
    }

    #[test]
    fn describe_redacts_api_key_but_save_keeps_it() {
        let settings = Settings {
            gemini_api_key: "secret-key-value".to_string(),
            claude_api_key: "another-secret".to_string(),
            openrouter_api_key: "yet-another-secret".to_string(),
            ..Settings::default()
        };

        let shown = settings.describe();
        assert!(shown.contains("gemini_api_key = \"***\""));
        assert!(shown.contains("claude_api_key = \"***\""));
        assert!(shown.contains("openrouter_api_key = \"***\""));
        assert!(!shown.contains("secret-key-value"));
        assert!(!shown.contains("another-secret"));
        assert!(!shown.contains("yet-another-secret"));

        // The on-disk form (what save writes) must keep the real value.
        assert!(toml_render(&settings).contains("secret-key-value"));
        assert!(toml_render(&settings).contains("another-secret"));
        assert!(toml_render(&settings).contains("yet-another-secret"));
    }

    #[test]
    fn resolved_api_key_is_none_for_local_provider() {
        let settings = Settings {
            provider: Provider::Local,
            claude_api_key: "should-be-ignored".to_string(),
            ..Settings::default()
        };
        assert_eq!(settings.resolved_api_key(), None);
    }
}
