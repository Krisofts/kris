use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use serde_json::Value;

use crate::agent::{heuristic_tokens, Agent, Project};
use crate::config::{Provider, Settings};
use crate::message::Message;
use crate::picker;
use crate::server;
use crate::style::{bold, cyan, dim, green, red, yellow};
use crate::tools::{ToolRegistry, AWAITING_CONFIRMATION};

// Raised from 10/24: a multi-file task (scaffold a project, add a feature,
// verify with clippy/tests) easily needs more tool calls than that,
// especially now that append_file encourages building a long new file as
// several small chunks rather than one big write_file - each chunk is its
// own iteration.
const DEFAULT_MAX_ITERATIONS: u32 = 20;
const FIX_MIN_ITERATIONS: u32 = 40;

const MODEL_PRESETS: &[(&str, &str)] = &[
    ("1.5b", "qwen2.5-coder-1.5b-instruct-q4_k_m.gguf"),
    ("3b", "qwen2.5-coder-3b-instruct-q4_k_m.gguf"),
    ("7b", "qwen2.5-coder-7b-instruct-q4_k_m.gguf"),
];

struct Session {
    settings: Settings,
    /// Resolved working directory: `workspace/active_project` if
    /// `active_project` names a real subfolder, otherwise `workspace`
    /// itself (e.g. so a brand-new project can be scaffolded into it
    /// before anything has been picked).
    root: PathBuf,
    project_name: String,
    project_type_hint: String,
    history: Vec<Message>,
}

impl Session {
    fn new(settings: Settings) -> Self {
        let root = resolve_root(&settings.workspace, &settings.active_project);
        let (project_name, project_type_hint) = project_hint(&root);

        Self {
            settings,
            root,
            project_name,
            project_type_hint,
            history: Vec::new(),
        }
    }

    /// True once `active_project` names a subfolder of `workspace` that
    /// actually exists - false for a fresh install, or right after
    /// switching to a workspace folder that hasn't had a project picked
    /// yet.
    fn has_active_project(&self) -> bool {
        !self.settings.active_project.is_empty()
            && PathBuf::from(&self.settings.workspace)
                .join(&self.settings.active_project)
                .is_dir()
    }

    fn refresh_root(&mut self) {
        self.root = resolve_root(&self.settings.workspace, &self.settings.active_project);
        let (name, hint) = project_hint(&self.root);
        self.project_name = name;
        self.project_type_hint = hint;
        self.history.clear();
    }

    /// Switches which folder acts as the workspace (the parent holding
    /// every project) - distinct from `switch_project`, which picks a
    /// project inside the current workspace. Clears `active_project`
    /// since it almost certainly doesn't exist under the new workspace.
    fn switch_workspace(&mut self, path: &Path) {
        self.settings.workspace = path.display().to_string();
        self.settings.active_project.clear();
        self.refresh_root();
    }

    /// Switches the active project to `name`, a subfolder of the current
    /// workspace - the caller is expected to have already checked it
    /// exists.
    fn switch_project(&mut self, name: &str) {
        self.settings.active_project = name.to_string();
        self.refresh_root();
    }

    fn agent(&self) -> Agent {
        Agent::new(
            server::client_for(&self.settings),
            ToolRegistry::with_defaults(
                self.settings.bypass_permissions,
                self.settings.auto_approve_edits,
            ),
            self.settings.temperature,
            self.settings.max_tokens,
            self.settings.effective_context_size(),
        )
    }
}

fn resolve_root(workspace: &str, active_project: &str) -> PathBuf {
    let workspace = PathBuf::from(workspace);

    if !active_project.is_empty() {
        let candidate = workspace.join(active_project);
        if candidate.is_dir() {
            return candidate;
        }
    }

    workspace
}

/// Guesses a one-line project-type hint from marker files, so the system
/// prompt can say e.g. "This is a Rust (Cargo) project" without spending a
/// tool call on it.
fn project_hint(root: &Path) -> (String, String) {
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.display().to_string());

    let markers: &[(&str, &str)] = &[
        ("Cargo.toml", "This is a Rust project (Cargo)."),
        (
            "package.json",
            "This is a JavaScript/TypeScript project (npm).",
        ),
        ("pyproject.toml", "This is a Python project."),
        ("go.mod", "This is a Go project."),
        ("Gemfile", "This is a Ruby project (Bundler)."),
    ];

    for (marker, hint) in markers {
        if root.join(marker).is_file() {
            return (name, hint.to_string());
        }
    }

    (name, String::new())
}

pub async fn run_once(settings: Settings, prompt: &str) -> Result<()> {
    print_sanity_warnings(&settings);
    let mut session = Session::new(settings);

    std::fs::create_dir_all(&session.root).ok();

    if !server::ensure_running(&session.settings).await {
        return Ok(());
    }

    ask(&mut session, prompt).await;

    Ok(())
}

pub async fn run_interactive(settings: Settings) -> Result<()> {
    print_sanity_warnings(&settings);
    let mut session = Session::new(settings);
    std::fs::create_dir_all(&session.root).ok();

    print_banner(&session);

    let mut editor = DefaultEditor::new()?;

    loop {
        let prompt = format!("{} ", cyan("kris>"));
        match editor.readline(&prompt) {
            Ok(line) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let _ = editor.add_history_entry(line);

                if !dispatch(&mut session, line).await {
                    break;
                }
            }
            Err(ReadlineError::Interrupted) => continue,
            Err(ReadlineError::Eof) => break,
            Err(err) => {
                println!("{}", red(&format!("Readline error: {err}")));
                break;
            }
        }
    }

    println!("{}", dim("Goodbye."));
    Ok(())
}

fn print_sanity_warnings(settings: &Settings) {
    for warning in settings.sanity_warnings() {
        println!("{}", yellow(&format!("Warning: {warning}")));
    }
}

/// Draws a small boxed header (title + subtitle) sized to its own text
/// rather than the terminal width - safe on the narrow terminals Termux
/// phones often have, and simple enough to need no extra crate (padding
/// is computed on the plain text, then the ANSI color codes are wrapped
/// around the padded string so they don't throw off the width math).
fn print_banner(session: &Session) {
    let title = format!("KRIS v{}", env!("CARGO_PKG_VERSION"));
    let subtitle = "local & online coding assistant";
    let width = title.len().max(subtitle.len());

    println!("{}", cyan(&format!("╭{}╮", "─".repeat(width + 2))));
    println!(
        "{} {} {}",
        cyan("│"),
        bold(&format!("{title:<width$}")),
        cyan("│")
    );
    println!(
        "{} {} {}",
        cyan("│"),
        dim(&format!("{subtitle:<width$}")),
        cyan("│")
    );
    println!("{}", cyan(&format!("╰{}╯", "─".repeat(width + 2))));
    println!();

    let (mode, model) = describe_mode(&session.settings);
    println!(
        "{}",
        dim(&format!(
            "workspace: {}  |  project: {}  |  {mode}: {model}",
            session.settings.workspace,
            if session.has_active_project() {
                session.project_name.clone()
            } else {
                "(belum ada project)".to_string()
            },
        ))
    );
    println!("{}", dim("Type your request, or `help` for commands."));
    if !session.has_active_project() {
        println!(
            "{}",
            dim("Belum ada project - `project` untuk lihat daftar, atau langsung minta KRIS membuatkan satu.")
        );
    }
    println!();
}

/// Returns false when the REPL should exit.
async fn dispatch(session: &mut Session, line: &str) -> bool {
    if let Some(command) = line.strip_prefix('!') {
        run_raw_shell(session, command);
        return true;
    }

    let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
    let rest = rest.trim();

    match word {
        "exit" | "quit" => return false,
        "help" => print_help(),
        "version" => println!("kris {}", env!("CARGO_PKG_VERSION")),
        "clear" => {
            session.history.clear();
            print!("\x1b[2J\x1b[H");
            let _ = std::io::stdout().flush();
            println!("{}", dim("Conversation history cleared."));
        }
        "health" => {
            if server::check_health(&session.settings).await {
                println!(
                    "{}",
                    green(&format!(
                        "llama-server is up at {}",
                        session.settings.llama_url
                    ))
                );
            } else {
                println!(
                    "{}",
                    red(&format!(
                        "llama-server is not reachable at {}",
                        session.settings.llama_url
                    ))
                );
                println!("Run `serve` to start it.");
            }
        }
        "serve" => {
            server::ensure_running(&session.settings).await;
        }
        "mode" => handle_mode(session, rest),
        "model" => handle_model(session, rest),
        "workspace" => handle_workspace(session, rest),
        "project" => handle_project(session, rest),
        "config" => handle_config(session, rest),
        "fix" => {
            let prompt = format!(
                "Build this project and fix any compile errors or failing tests, one at a \
                 time, rebuilding after each fix, until the build succeeds cleanly (and \
                 tests pass if there are any). Use run_command to build/test, and \
                 read_file / search_code to investigate errors before editing.{}",
                if rest.is_empty() {
                    String::new()
                } else {
                    format!(" Additional context from the user: {rest}")
                }
            );
            ask_with_iterations(session, &prompt, FIX_MIN_ITERATIONS).await;
        }
        "init" => {
            let prompt = "Explore this project (file tree, key source files, config, README \
                 if any) and write a concise KRIS.md at the project root summarizing: what \
                 the project is and its language/framework, the folder structure, how to \
                 build/test/run it, and any conventions or gotchas a coding assistant should \
                 know before making changes. Keep it information-dense, not padded prose - a \
                 few hundred lines at most. If KRIS.md already exists, read it first and \
                 update it rather than starting over.";
            ask_with_iterations(session, prompt, DEFAULT_MAX_ITERATIONS).await;
        }
        "review" => {
            let prompt = format!(
                "Review the currently pending changes (git diff against HEAD) for correctness \
                 bugs and clear simplification opportunities. Use the git tool (diff) to see \
                 what changed, and read any files you need more context on. Report findings \
                 as a short list: file, what's wrong, and a concrete failure scenario for each \
                 bug. If there are no pending changes, say so instead of reviewing something \
                 else.{}",
                if rest.is_empty() {
                    String::new()
                } else {
                    format!(" Additional context from the user: {rest}")
                }
            );
            ask_with_iterations(session, &prompt, DEFAULT_MAX_ITERATIONS).await;
        }
        "security-review" => {
            let prompt = format!(
                "Do a security review of the currently pending changes (git diff against \
                 HEAD) - not a general audit of the whole codebase. Use the git tool (diff) \
                 to see what changed, then look for vulnerability classes introduced or \
                 worsened by this diff specifically: injection, path traversal, secrets \
                 committed in plain text, unsafe deserialization, missing auth/permission \
                 checks, and similar. Report findings as a short list: file, the \
                 vulnerability, and a concrete exploit scenario for each. If there are no \
                 pending changes, say so instead of reviewing something else.{}",
                if rest.is_empty() {
                    String::new()
                } else {
                    format!(" Additional context from the user: {rest}")
                }
            );
            ask_with_iterations(session, &prompt, DEFAULT_MAX_ITERATIONS).await;
        }
        _ => ask(session, line).await,
    }

    true
}

fn run_raw_shell(session: &Session, command: &str) {
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(&session.root)
        .output();

    match output {
        Ok(output) => {
            print!("{}", String::from_utf8_lossy(&output.stdout));
            eprint!("{}", String::from_utf8_lossy(&output.stderr));
        }
        Err(err) => println!("{}", red(&format!("failed to run command: {err}"))),
    }
}

/// Returns a `(mode_label, model_label)` pair describing the active
/// provider, for the banner and the `mode` command.
fn describe_mode(settings: &Settings) -> (&'static str, String) {
    match settings.provider {
        Provider::Local => (
            "offline",
            if settings.model_path.is_empty() {
                "(not configured)".to_string()
            } else {
                settings.model_path.clone()
            },
        ),
        Provider::Gemini => ("online", settings.gemini_model.clone()),
        Provider::Claude => ("claude", settings.claude_model.clone()),
        Provider::OpenRouter => ("openrouter", settings.openrouter_model.clone()),
    }
}

/// Switches between offline (local llama-server) and an online provider
/// (Gemini, Claude, or OpenRouter) at runtime. Clears the conversation,
/// since backends don't share a KV cache and a history built against one
/// model is best restarted on another. Accepts `offline`/`local`,
/// `online`/`gemini`, `claude`/`anthropic`, and `openrouter`/`or`.
fn handle_mode(session: &mut Session, arg: &str) {
    if arg.is_empty() {
        let (mode, model) = describe_mode(&session.settings);
        println!("Current mode: {mode} ({model})");
        println!("Usage: mode offline    use the local llama.cpp server");
        println!("       mode online     use the Gemini API");
        println!("       mode claude     use the Claude API");
        println!("       mode openrouter use the OpenRouter API");
        return;
    }

    if let Err(err) = session.settings.set_field("provider", arg) {
        println!("{}", red(&format!("{err}")));
        return;
    }

    session.history.clear();
    if let Err(err) = session.settings.save() {
        println!("{}", red(&format!("Failed to save config: {err}")));
        return;
    }

    match session.settings.provider {
        Provider::Local => {
            println!("{}", green("Switched to offline mode (local llama.cpp)."));
            if session.settings.model_path.is_empty() {
                println!(
                    "{}",
                    dim("No model_path set yet - pick one with `model 3b` or `config set model_path <gguf>`.")
                );
            } else {
                println!(
                    "{}",
                    dim("Run `serve` to start llama-server if it isn't up.")
                );
            }
        }
        Provider::Gemini => {
            println!(
                "{}",
                green(&format!(
                    "Switched to online mode ({} via Gemini).",
                    session.settings.gemini_model
                ))
            );
            if session.settings.resolved_api_key().is_none() {
                println!(
                    "{}",
                    yellow("No API key set - export GEMINI_API_KEY, or `config set gemini_api_key <key>`.")
                );
            }
        }
        Provider::Claude => {
            println!(
                "{}",
                green(&format!(
                    "Switched to Claude mode ({}).",
                    session.settings.claude_model
                ))
            );
            if session.settings.resolved_api_key().is_none() {
                println!(
                    "{}",
                    yellow("No API key set - export ANTHROPIC_API_KEY, or `config set claude_api_key <key>`.")
                );
            }
        }
        Provider::OpenRouter => {
            println!(
                "{}",
                green(&format!(
                    "Switched to OpenRouter mode ({}).",
                    session.settings.openrouter_model
                ))
            );
            if session.settings.resolved_api_key().is_none() {
                println!(
                    "{}",
                    yellow("No API key set - export OPENROUTER_API_KEY, or `config set openrouter_api_key <key>`.")
                );
            }
        }
    }
}

fn handle_model(session: &mut Session, arg: &str) {
    if arg.is_empty() {
        println!("Current model_path: {}", session.settings.model_path);
        println!(
            "Presets: {}",
            MODEL_PRESETS
                .iter()
                .map(|(k, _)| *k)
                .collect::<Vec<_>>()
                .join(", ")
        );
        println!("Usage: model <preset>  (or `config set model_path <path>` for a custom GGUF)");
        return;
    }

    match MODEL_PRESETS.iter().find(|(key, _)| *key == arg) {
        Some((_, filename)) => {
            let Some(home) = dirs::home_dir() else {
                println!("{}", red("Could not determine home directory."));
                return;
            };
            session.settings.model_path = home.join(filename).display().to_string();
            if let Err(err) = session.settings.save() {
                println!("{}", red(&format!("Failed to save config: {err}")));
                return;
            }
            println!(
                "{}",
                green(&format!(
                    "model_path set to {}",
                    session.settings.model_path
                ))
            );
            println!("{}", dim("If llama-server is already running, stop it and run `serve` again to load the new model."));
        }
        None => println!(
            "{}",
            red(&format!(
                "Unknown preset \"{arg}\". Try: {}",
                MODEL_PRESETS
                    .iter()
                    .map(|(k, _)| *k)
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        ),
    }
}

/// Switches which folder acts as the workspace - the parent that holds
/// every project - as opposed to `project <name>`, which picks a project
/// inside it. Creates the folder if it doesn't exist yet, since it's
/// just a container. With no argument, launches the interactive project
/// picker for the (possibly just-created) workspace folder.
fn handle_workspace(session: &mut Session, arg: &str) {
    if arg.is_empty() {
        println!("Current workspace: {}", session.settings.workspace);
        interactive_pick_project(session);
        return;
    }

    // Routed through `set_field` so relative input is anchored at the home
    // directory rather than wherever the `kris` process happens to be
    // running from (e.g. inside its own cloned source repo) - see
    // `normalize_workspace_path` in config.rs.
    let _ = session.settings.set_field("workspace", arg);
    let path = PathBuf::from(&session.settings.workspace);
    if let Err(err) = fs::create_dir_all(&path) {
        println!(
            "{}",
            red(&format!("Could not create {}: {err}", path.display()))
        );
        return;
    }

    session.switch_workspace(&path);
    if let Err(err) = session.settings.save() {
        println!("{}", red(&format!("Failed to save config: {err}")));
    }
    println!(
        "{}",
        green(&format!("Switched workspace to {}", path.display()))
    );
    if !session.has_active_project() {
        println!(
            "{}",
            dim("Belum ada project - `project` untuk lihat daftar.")
        );
    }
}

/// With no argument, launches the interactive picker over projects
/// living directly under the workspace folder. `project <name>` skips
/// the picker and switches straight to that project, as opposed to
/// `workspace <path>`, which changes the workspace folder itself.
/// Switching (either way) persists as the new `active_project`, so it's
/// what KRIS opens next time too - "picking a project" and "setting the
/// default" are the same action here.
fn handle_project(session: &mut Session, arg: &str) {
    let workspace = PathBuf::from(&session.settings.workspace);
    fs::create_dir_all(&workspace).ok();

    if arg.is_empty() {
        interactive_pick_project(session);
        return;
    }

    let path = workspace.join(arg);
    if !path.is_dir() {
        println!(
            "{}",
            red(&format!("No project \"{arg}\" in {}", workspace.display()))
        );
        return;
    }

    apply_project_switch(session, arg);
}

/// Shows the arrow-key project picker over the current workspace folder
/// and applies whatever the user chooses. Falls back to a plain numbered-
/// free text listing if an interactive picker isn't possible at all (no
/// real TTY) rather than doing nothing.
fn interactive_pick_project(session: &mut Session) {
    let workspace = PathBuf::from(&session.settings.workspace);
    let names = list_project_names(&workspace);

    if names.is_empty() {
        println!(
            "{}",
            yellow(&format!("Belum ada project di {}.", workspace.display()))
        );
        println!(
            "{}",
            dim("Minta KRIS membuatkan satu, atau taruh folder project di sana lalu jalankan `project <nama>`.")
        );
        return;
    }

    let active_index = names
        .iter()
        .position(|name| *name == session.settings.active_project);
    let prompt = format!(
        "Pilih project di {} (\u{2191}/\u{2193} pilih, Enter konfirmasi, Esc batal):",
        workspace.display()
    );

    match picker::pick(&prompt, &names, active_index) {
        picker::PickOutcome::Chosen(name) => apply_project_switch(session, &name),
        picker::PickOutcome::Cancelled => {}
        picker::PickOutcome::Unavailable => {
            println!("Projects in {}:", workspace.display());
            for name in &names {
                let marker = if session.settings.active_project == *name {
                    "* "
                } else {
                    "  "
                };
                println!("{marker}{name}");
            }
            println!("{}", dim("Usage: project <name>"));
        }
    }
}

fn list_project_names(workspace: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(workspace) else {
        return Vec::new();
    };

    let mut names: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| !name.starts_with('.'))
        .collect();
    names.sort();
    names
}

fn apply_project_switch(session: &mut Session, name: &str) {
    session.switch_project(name);
    if let Err(err) = session.settings.save() {
        println!("{}", red(&format!("Failed to save config: {err}")));
    }
    println!(
        "{}",
        green(&format!(
            "Switched to project {name} ({})",
            session.root.display()
        ))
    );
}

fn handle_config(session: &mut Session, rest: &str) {
    if rest.is_empty() {
        println!("{}", session.settings.describe());
        return;
    }

    let mut parts = rest.splitn(3, ' ');
    match parts.next() {
        Some("set") => {
            let (Some(key), Some(value)) = (parts.next(), parts.next()) else {
                println!("Usage: config set <key> <value>");
                return;
            };
            match session.settings.set_field(key, value) {
                Ok(()) => match session.settings.save() {
                    Ok(()) => println!("{}", green(&format!("{key} = {value}"))),
                    Err(err) => println!("{}", red(&format!("Failed to save config: {err}"))),
                },
                Err(err) => println!("{}", red(&format!("{err}"))),
            }
        }
        _ => {
            println!("Usage: config          (show all settings)\n       config set <key> <value>")
        }
    }
}

fn print_help() {
    println!("Commands:");
    println!("  <anything else>       ask KRIS about this project");
    println!("  fix [notes]           build and iteratively fix errors until it's clean");
    println!("  init                  explore the project and write/update KRIS.md with a summary for future turns");
    println!("  review [notes]        review pending changes (git diff) for correctness bugs and simplification");
    println!("  security-review [notes] review pending changes (git diff) for security issues");
    println!(
        "  mode [offline|online|claude|openrouter] show/switch between local llama.cpp, Gemini, Claude, and OpenRouter"
    );
    println!("  health                check whether the active backend is reachable");
    println!(
        "  serve                 start llama-server in the background if needed (offline mode)"
    );
    println!("  model [preset]        show/switch the local Qwen2.5-Coder model (1.5b/3b/7b)");
    println!("  workspace [path]      show workspace / pick a project with arrow keys, or switch to a different workspace folder");
    println!("  project [name]        pick a project with arrow keys, or switch straight to <name> - also becomes the new default");
    println!("  config [set k v]      show or change settings (saved to config.toml)");
    println!("  clear                 clear conversation history and the screen");
    println!("  !<command>            run a raw shell command directly");
    println!("  help                  show this message");
    println!("  version               show the KRIS version");
    println!("  exit / quit           leave KRIS");
}

async fn ask(session: &mut Session, prompt: &str) {
    ask_with_iterations(session, prompt, DEFAULT_MAX_ITERATIONS).await;
}

/// Runs one turn, retrying once via `server::ensure_running` if the request
/// fails with what looks like a connection error - covers the case where
/// llama-server got killed (backgrounded process reaped, phone low on
/// memory, etc.) while KRIS was idle between turns.
async fn ask_with_iterations(session: &mut Session, prompt: &str, max_iterations: u32) {
    if !server::check_health(&session.settings).await
        && !server::ensure_running(&session.settings).await
    {
        return;
    }

    println!();

    // `run_turn` rolls a failed turn back out of `session.history` before
    // returning its error, so retrying with the exact same prompt here is
    // safe - it won't leave a duplicated or dangling user message behind.
    if let Err(err) = run_turn(session, prompt, max_iterations).await {
        // Restarting only makes sense for the local server; an online
        // provider that drops a connection is a network/API issue a restart
        // can't fix, so fall straight through to reporting it.
        if looks_like_connection_error(&err) && session.settings.provider == Provider::Local {
            println!();
            println!(
                "{}",
                yellow("Lost connection to llama-server - trying to restart it...")
            );
            if server::ensure_running(&session.settings).await {
                if let Err(err) = run_turn(session, prompt, max_iterations).await {
                    print_turn_error(session, &err);
                }
            }
        } else {
            print_turn_error(session, &err);
        }
    }
}

fn looks_like_connection_error(err: &anyhow::Error) -> bool {
    err.downcast_ref::<reqwest::Error>()
        .map(|err| err.is_connect() || err.is_timeout())
        .unwrap_or(false)
}

fn print_turn_error(session: &Session, err: &anyhow::Error) {
    println!();
    println!("{} {err}", red("Error talking to the model:"));
    let hint = match session.settings.provider {
        Provider::Local => format!(
            "Make sure llama-server is running at {}",
            session.settings.llama_url
        ),
        Provider::Gemini => format!(
            "Check your network connection and that GEMINI_API_KEY is valid (model {} at {}).",
            session.settings.gemini_model, session.settings.gemini_url
        ),
        Provider::Claude => format!(
            "Check your network connection and that ANTHROPIC_API_KEY is valid (model {}).",
            session.settings.claude_model
        ),
        Provider::OpenRouter => format!(
            "Check your network connection and that OPENROUTER_API_KEY is valid (model {}).",
            session.settings.openrouter_model
        ),
    };
    println!("{}", dim(&hint));
}

async fn run_turn(session: &mut Session, prompt: &str, max_iterations: u32) -> Result<()> {
    let agent = session.agent();
    let root = session.root.clone();
    let project_name = session.project_name.clone();
    let project_type_hint = session.project_type_hint.clone();

    let waiting = Arc::new(AtomicBool::new(true));
    let spinner_waiting = waiting.clone();
    let counts = Arc::new(SharedTurnCounts::default());
    let spinner_counts = counts.clone();
    let spinner = tokio::spawn(spin(spinner_waiting, spinner_counts));

    let history_len_before = session.history.len();
    let started = Instant::now();

    let result = agent
        .run(
            &mut session.history,
            Project {
                root: &root,
                name: &project_name,
                type_hint: &project_type_hint,
            },
            prompt,
            max_iterations,
            |delta: &str| {
                if waiting.swap(false, Ordering::SeqCst) {
                    clear_line();
                }
                print!("{delta}");
                let _ = std::io::stdout().flush();
            },
            |tool_name: &str, args: &Value, result: &str| {
                if waiting.swap(false, Ordering::SeqCst) {
                    clear_line();
                }
                println!("{} {}", cyan("●"), bold(&format_tool_call(tool_name, args)));
                counts.record(tool_name);

                let is_error = result.starts_with("Error: ");

                // A command's real output (build/test logs, diffs, ...) is
                // worth seeing in full rather than just its first line - a
                // boxed, line-prefixed block reads far easier than a wall
                // of text, and matches the ┌─/│/└─ style the confirmation
                // prompts already use elsewhere.
                if tool_category(tool_name) == ToolCategory::Command && result.lines().count() > 1 {
                    print_boxed_output(result, is_error);
                } else {
                    let summary = result.lines().next().unwrap_or("").trim();
                    if !summary.is_empty() {
                        let truncated: String = summary.chars().take(100).collect();
                        let ellipsis = if summary.chars().count() > 100 {
                            "…"
                        } else {
                            ""
                        };
                        let line = format!("  ⎿ {truncated}{ellipsis}");
                        println!("{}", if is_error { red(&line) } else { dim(&line) });
                    }
                }

                // Re-arms the spinner for the wait before the next
                // iteration's model response - a multi-step turn commonly
                // runs several tool calls in a row, each separated by its
                // own round trip, and each of those waits deserves the
                // same "still working" feedback the first one got.
                waiting.store(true, Ordering::SeqCst);
            },
        )
        .await;

    spinner.abort();
    clear_line();

    match result {
        // `answer` isn't reprinted here - agent.run() has already streamed
        // it in full via the `on_delta` callback above (live as tokens
        // arrived, or flushed in one piece for a held-back/synthetic
        // message), so doing it again would just duplicate the output.
        Ok(_answer) => {
            let elapsed = started.elapsed();
            let tokens = heuristic_tokens(&session.history[history_len_before..]);
            println!();
            // The "Read N files · Ran N commands" tally lived only in the
            // spinner label above while the turn was in progress - like
            // Claude Code, it isn't repeated here once things are done.
            println!(
                "{}",
                dim(&format!(
                    "{} · {}",
                    format_elapsed(elapsed),
                    format_tokens(tokens)
                ))
            );
            Ok(())
        }
        Err(err) => Err(err),
    }
}

/// Which of Claude Code's three broad action groups a tool call belongs
/// to, for the end-of-turn "Read N files · Edited N files · Ran N
/// commands" recap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolCategory {
    Read,
    Edit,
    Command,
    Other,
}

fn tool_category(name: &str) -> ToolCategory {
    match name {
        "read_file" | "list_directory" | "tree" | "find_files" | "search_code" | "outline_file"
        | "git" => ToolCategory::Read,
        "write_file" | "append_file" | "edit_file" | "delete_file" | "delete_directory"
        | "move_file" | "create_directory" => ToolCategory::Edit,
        "run_command" | "git_commit" => ToolCategory::Command,
        _ => ToolCategory::Other,
    }
}

#[derive(Default)]
struct TurnCounts {
    read: usize,
    edited: usize,
    commands: usize,
}

impl TurnCounts {
    fn summary(&self) -> Option<String> {
        let plural = |n: usize, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });

        let parts: Vec<String> = [
            (self.read, "Read", "file"),
            (self.edited, "Edited", "file"),
            (self.commands, "Ran", "command"),
        ]
        .into_iter()
        .filter(|(n, ..)| *n > 0)
        .map(|(n, verb, word)| format!("{verb} {}", plural(n, word)))
        .collect();

        (!parts.is_empty()).then(|| parts.join(" · "))
    }
}

/// Thread-safe tally shared between the closure recording tool calls (in
/// `run_turn`, on the main task) and the spinner (its own tokio task) that
/// displays a running "Read N files · Ran N commands" recap live, next to
/// the "thinking..." label, while the turn is still in progress - unlike
/// `TurnCounts`, which is a plain snapshot, not something updated from two
/// tasks at once.
#[derive(Default)]
struct SharedTurnCounts {
    read: AtomicUsize,
    edited: AtomicUsize,
    commands: AtomicUsize,
}

impl SharedTurnCounts {
    fn record(&self, tool_name: &str) {
        let counter = match tool_category(tool_name) {
            ToolCategory::Read => &self.read,
            ToolCategory::Edit => &self.edited,
            ToolCategory::Command => &self.commands,
            ToolCategory::Other => return,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    fn snapshot(&self) -> TurnCounts {
        TurnCounts {
            read: self.read.load(Ordering::Relaxed),
            edited: self.edited.load(Ordering::Relaxed),
            commands: self.commands.load(Ordering::Relaxed),
        }
    }
}

/// Prints a command's full result in a bordered, line-prefixed block
/// (┌─ / │ / └─) instead of a single truncated line, so multi-line output
/// like build/test logs or a git diff is actually readable rather than
/// just showing its first line.
fn print_boxed_output(result: &str, is_error: bool) {
    let border = |s: &str| if is_error { red(s) } else { cyan(s) };
    let content = |s: &str| if is_error { red(s) } else { dim(s) };

    println!("{}", border("┌─"));
    for line in result.lines() {
        println!("{} {}", border("│"), content(line));
    }
    println!("{}", border("└─"));
}

fn format_elapsed(elapsed: Duration) -> String {
    if elapsed.as_secs() >= 1 {
        format!("{:.1}s", elapsed.as_secs_f64())
    } else {
        format!("{}ms", elapsed.as_millis())
    }
}

fn format_tokens(tokens: usize) -> String {
    if tokens >= 1000 {
        format!("~{:.1}k tokens", tokens as f64 / 1000.0)
    } else {
        format!("~{tokens} tokens")
    }
}

fn format_tool_call(tool_name: &str, args: &Value) -> String {
    if tool_name == "move_file" {
        if let (Some(from), Some(to)) = (
            args.get("from").and_then(Value::as_str),
            args.get("to").and_then(Value::as_str),
        ) {
            return format!("{tool_name}({from} -> {to})");
        }
    }

    let summary = ["command", "path", "pattern", "keyword", "subcommand"]
        .into_iter()
        .find_map(|key| args.get(key).and_then(Value::as_str));

    match summary {
        Some(summary) => format!("{tool_name}({summary})"),
        None => format!("{tool_name}()"),
    }
}

fn clear_line() {
    // Clear-to-end-of-line rather than padding a fixed width - the
    // spinner's label length varies (it grows once the elapsed-time
    // warning kicks in), so a fixed-width blank could leave remnants of a
    // longer line behind.
    print!("\r\x1b[K");
    let _ = std::io::stdout().flush();
}

/// Runs for the whole turn (stopped externally via `.abort()`), rather
/// than exiting the moment `waiting` first goes false - `run_turn` flips
/// it back to true after each tool call, so this needs to keep ticking
/// and simply stay quiet in between, ready to resume drawing for the next
/// wait instead of having already exited after the first one.
async fn spin(waiting: Arc<AtomicBool>, counts: Arc<SharedTurnCounts>) {
    const FRAMES: [&str; 10] = ["-", "\\", "|", "/", "-", "\\", "|", "/", "*", "+"];
    let mut i = 0;
    let started = Instant::now();

    loop {
        // A tool is blocked on a y/N confirmation right now - stay quiet
        // instead of redrawing over that prompt every 90ms, which would
        // hide it and make KRIS look stuck "thinking" forever while it's
        // actually just waiting on the user.
        if waiting.load(Ordering::SeqCst) && !AWAITING_CONFIRMATION.load(Ordering::SeqCst) {
            let elapsed = started.elapsed().as_secs();

            // A visible running clock, not just a spinning glyph, so a
            // genuinely long wait (a local model stuck generating a
            // repetitive reply, or a slow tool call) reads as "still
            // working, N seconds so far" instead of looking identically
            // frozen at 5s and at 5 minutes. Past a minute, that's long
            // enough for a plain chat reply that it's worth a nudge toward
            // what to check, rather than just letting it keep spinning
            // silently - `\x1b[K` (clear to end of line) instead of
            // padding with spaces, since this label's length changes as
            // the elapsed count grows.
            let mut label = if elapsed >= 60 {
                format!(
                    "thinking... {elapsed}s (unusually long - if this is local/offline mode, \
                     the model may be stuck in a repetitive generation loop; check \
                     ~/llama-server.log, or try `config set max_tokens 256` to bound it)"
                )
            } else {
                format!("thinking... {elapsed}s")
            };

            // Live "Read N files · Ran N commands" recap, like Claude Code
            // shows while a turn is still in progress - it disappears with
            // the rest of this line once the turn finishes, rather than
            // being printed again afterward.
            if let Some(summary) = counts.snapshot().summary() {
                label = format!("{label} · {summary}");
            }

            print!("\r\x1b[K{} {}", dim(FRAMES[i % FRAMES.len()]), dim(&label));
            let _ = std::io::stdout().flush();
            i += 1;
        }
        tokio::time::sleep(Duration::from_millis(90)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_category_classifies_every_built_in_tool() {
        assert_eq!(tool_category("read_file"), ToolCategory::Read);
        assert_eq!(tool_category("search_code"), ToolCategory::Read);
        assert_eq!(tool_category("git"), ToolCategory::Read);
        assert_eq!(tool_category("write_file"), ToolCategory::Edit);
        assert_eq!(tool_category("append_file"), ToolCategory::Edit);
        assert_eq!(tool_category("run_command"), ToolCategory::Command);
        assert_eq!(tool_category("git_commit"), ToolCategory::Command);
        assert_eq!(tool_category("something_unknown"), ToolCategory::Other);
    }

    #[test]
    fn turn_counts_summary_omits_zero_categories_and_pluralizes() {
        let counts = SharedTurnCounts::default();
        assert_eq!(counts.snapshot().summary(), None);

        counts.record("read_file");
        assert_eq!(counts.snapshot().summary().as_deref(), Some("Read 1 file"));

        counts.record("edit_file");
        assert_eq!(
            counts.snapshot().summary().as_deref(),
            Some("Read 1 file · Edited 1 file")
        );

        counts.record("run_command");
        counts.record("run_command");
        assert_eq!(
            counts.snapshot().summary().as_deref(),
            Some("Read 1 file · Edited 1 file · Ran 2 commands")
        );
    }

    #[test]
    fn turn_counts_ignores_uncategorized_tools() {
        let counts = SharedTurnCounts::default();
        counts.record("outline_file");
        counts.record("something_unknown");
        assert_eq!(counts.snapshot().summary().as_deref(), Some("Read 1 file"));
    }
}
