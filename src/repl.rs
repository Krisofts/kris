use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use rustyline::completion::Completer;
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::{Hinter, HistoryHinter};
use rustyline::history::DefaultHistory;
use rustyline::validate::Validator;
use rustyline::{Context as RlContext, Editor, Helper};
use serde_json::Value;

use crate::agent::{heuristic_tokens, Agent, Project};
use crate::config::{Provider, Settings};
use crate::message::Message;
use crate::picker;
use crate::server;
use crate::style::{blue, bold, cyan, dim, green, red, yellow};
use crate::term::{terminal_width, truncate_to_width};
use crate::tools::{ToolRegistry, AWAITING_CONFIRMATION, COMMAND_RUNNING};

// Raised from 10/24/20: a multi-file task (scaffold a project, add a
// feature, verify with clippy/tests) easily needs more tool calls than
// that, especially now that append_file encourages building a long new
// file as several small chunks rather than one big write_file - each
// chunk is its own iteration. Confirmed on-device: a full-stack scaffold
// (frontend + backend + curl smoke tests) ran past 20 well before it had
// a final answer.
const DEFAULT_MAX_ITERATIONS: u32 = 40;
const FIX_MIN_ITERATIONS: u32 = 40;

const MODEL_PRESETS: &[(&str, &str)] = &[
    ("1.5b", "qwen2.5-coder-1.5b-instruct-q4_k_m.gguf"),
    ("3b", "qwen2.5-coder-3b-instruct-q4_k_m.gguf"),
    ("7b", "qwen2.5-coder-7b-instruct-q4_k_m.gguf"),
];

/// Every REPL command `dispatch` recognizes as the first word of a line -
/// kept as its own list (rather than derived from `dispatch`'s `match`)
/// so tab-completion has something to complete against without needing
/// to parse that function.
const KNOWN_COMMANDS: &[&str] = &[
    "help",
    "version",
    "clear",
    "health",
    "serve",
    "mode",
    "model",
    "project",
    "config",
    "fix",
    "init",
    "review",
    "security-review",
    "exit",
    "quit",
];

/// Gives the REPL's line editor two suggestion sources, both accepted
/// with Tab (rustyline's default binding for `Complete`) then Enter:
/// known command names for a bare word (`he` -> `health`/`help`), and
/// previous history entries matching the current prefix (rustyline's
/// built-in `HistoryHinter`, shown as inline ghost text) for anything
/// else - so a repeated or similar prompt doesn't have to be retyped in
/// full.
struct KrisHelper {
    history_hinter: HistoryHinter,
}

impl KrisHelper {
    fn new() -> Self {
        Self {
            history_hinter: HistoryHinter::new(),
        }
    }
}

impl Completer for KrisHelper {
    type Candidate = String;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &RlContext<'_>,
    ) -> rustyline::Result<(usize, Vec<String>)> {
        // Only offers command-name completions for the first word of the
        // line, and only while the cursor is still inside it - typing
        // `mode onl<Tab>` shouldn't try to complete "onl" against command
        // names, since it's an argument, not a command.
        let prefix = &line[..pos];
        if prefix.contains(' ') {
            return Ok((0, Vec::new()));
        }

        let matches: Vec<String> = KNOWN_COMMANDS
            .iter()
            .filter(|cmd| cmd.starts_with(prefix))
            .map(|cmd| cmd.to_string())
            .collect();

        Ok((0, matches))
    }
}

impl Hinter for KrisHelper {
    type Hint = String;

    fn hint(&self, line: &str, pos: usize, ctx: &RlContext<'_>) -> Option<String> {
        self.history_hinter.hint(line, pos, ctx)
    }
}

impl Highlighter for KrisHelper {}
impl Validator for KrisHelper {}
impl Helper for KrisHelper {}

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

    let mut editor = Editor::<KrisHelper, DefaultHistory>::new()?;
    editor.set_helper(Some(KrisHelper::new()));

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
            "projects: {}  |  project: {}  |  {mode}: {model}",
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

/// With no argument, launches the interactive picker over projects living
/// directly under the projects folder. `project <name>` skips the picker
/// and switches straight to that project. `project <path>` (anything
/// containing a `/` or starting with `~`, so it can't be mistaken for a
/// plain project name) instead changes the projects folder itself - this
/// used to be a separate `workspace` command, folded in here since the two
/// concepts confused more than they helped kept apart. Switching to a
/// project persists as the new `active_project`, so it's what KRIS opens
/// next time too - "picking a project" and "setting the default" are the
/// same action here.
fn handle_project(session: &mut Session, arg: &str) {
    let projects_dir = PathBuf::from(&session.settings.workspace);
    fs::create_dir_all(&projects_dir).ok();

    if arg.is_empty() {
        println!("Projects folder: {}", projects_dir.display());
        interactive_pick_project(session);
        return;
    }

    if looks_like_a_path(arg) {
        change_projects_folder(session, arg);
        return;
    }

    let path = projects_dir.join(arg);
    if !path.is_dir() {
        println!(
            "{}",
            red(&format!(
                "No project \"{arg}\" in {}",
                projects_dir.display()
            ))
        );
        return;
    }

    apply_project_switch(session, arg);
}

/// Distinguishes `project <path>` (change the projects folder itself)
/// from `project <name>` (switch to a project inside it) - a path-like
/// argument contains a `/` or starts with `~`, neither of which is valid
/// in a plain project (sub)folder name.
fn looks_like_a_path(arg: &str) -> bool {
    arg.contains('/') || arg.starts_with('~')
}

/// Changes which folder holds every project. Creates it if it doesn't
/// exist yet, since it's just a container.
fn change_projects_folder(session: &mut Session, arg: &str) {
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
        green(&format!("Projects folder set to {}", path.display()))
    );
    if !session.has_active_project() {
        println!(
            "{}",
            dim("Belum ada project - `project` untuk lihat daftar.")
        );
    }
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
    println!("  project [name|path]   pick a project with arrow keys, switch straight to <name>, or pass a <path> (e.g. ~/projects) to change the projects folder itself");
    println!("  config [set k v]      show or change settings (saved to config.toml)");
    println!("  clear                 clear conversation history and the screen");
    println!("  !<command>            run a raw shell command directly");
    println!("  help                  show this message");
    println!("  version               show the KRIS version");
    println!("  exit / quit           leave KRIS");
    println!();
    println!(
        "Press Tab to complete a command name or accept a suggestion from history, then Enter."
    );
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

    // If the failed request happens before any progress this turn, `run`
    // rolls its dangling user message back out of `session.history`, so
    // retrying with the exact same prompt is safe - it won't leave a
    // duplicated message behind. If it happens after some tool calls
    // already completed, `run` keeps them instead - recorded here so the
    // retry can tell which case it's in.
    let history_len_before = session.history.len();

    if let Err(err) = run_turn(session, prompt, max_iterations).await {
        if looks_like_connection_error(&err) {
            let is_local = session.settings.provider == Provider::Local;

            println!();
            println!(
                "{}",
                yellow(if is_local {
                    "Lost connection to llama-server - trying to restart it..."
                } else {
                    "Lost connection to the model - retrying..."
                })
            );

            // `ensure_running` manages a local llama-server; for an online
            // provider it's a no-op check that an API key is still
            // configured, since there's no local process to relaunch - a
            // dropped connection there is just a network blip worth one
            // retry.
            if server::ensure_running(&session.settings).await {
                // Progress survived the disconnect (history grew past its
                // pre-turn length) - nudge the model to pick up from there
                // instead of resending the whole original prompt, which
                // would otherwise read as a request to redo the task from
                // scratch even though most of it is already done.
                let resume_prompt = if session.history.len() > history_len_before {
                    "Koneksi ke model sempat putus. Lanjutkan dari yang sudah dikerjakan sejauh \
                     ini - jangan mengulang dari awal."
                } else {
                    prompt
                };
                if let Err(err) = run_turn(session, resume_prompt, max_iterations).await {
                    print_turn_error(session, &err);
                }
            } else {
                print_turn_error(session, &err);
            }
        } else {
            print_turn_error(session, &err);
        }
    }
}

fn looks_like_connection_error(err: &anyhow::Error) -> bool {
    err.downcast_ref::<reqwest::Error>()
        // `bytes_stream()` (what the SSE reader in client.rs polls) wraps
        // every per-chunk read failure as `is_decode()`, even one caused by
        // the connection dying partway through (reset, proxy hiccup) rather
        // than any actual malformed data - confirmed on-device: a dropped
        // OpenRouter stream surfaced this way without tripping
        // `is_connect()`/`is_timeout()`, so it fell through to a dead end
        // instead of being retried.
        .map(|err| err.is_connect() || err.is_timeout() || err.is_decode())
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
    let is_local = session.settings.provider == Provider::Local;
    let spinner = tokio::spawn(spin(spinner_waiting, spinner_counts, is_local));

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

                // Running "Read N files · Edited N files · Ran N commands"
                // recap, printed right below each tool call as it happens
                // (not just live in the spinner, which disappears once the
                // turn ends) - a persistent record of progress so far.
                if let Some(summary) = counts.snapshot().summary() {
                    println!("{}", dim(&summary));
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

/// Prints a command's full result in a closed box (╭─╮ / │ … │ / ╰─╯),
/// the same border style as the startup banner, instead of a single
/// truncated line - so multi-line output like build/test logs or a git
/// diff is actually readable rather than just showing its first line.
fn print_boxed_output(result: &str, is_error: bool) {
    let border = |s: &str| if is_error { red(s) } else { blue(s) };
    let content = |s: &str| if is_error { red(s) } else { dim(s) };

    // run_command's result always starts with "exit code: N" - pulled out
    // of the body and shown as its own row inside the box instead of just
    // another line of output, colored by whether it actually succeeded
    // (independent of `is_error`, which only reflects whether the tool
    // call itself errored - a command that ran but exited non-zero is
    // still an Ok result). git/git_commit's plain output has no such
    // line, so this is a no-op for those.
    let (exit_code, body) = split_exit_code(result);
    let exit_line = exit_code.map(|code| format!("exit code: {code}"));

    // A line long enough to wrap would otherwise continue past the right
    // border with nothing to close it - clamping to the terminal width
    // (same helper the spinner uses, for the same reason) keeps every
    // line, and the border around it, intact. "│ " + " │" eats 4 columns.
    let max_inner = terminal_width().saturating_sub(4).max(1);

    let lines: Vec<String> = body
        .lines()
        .map(|line| truncate_to_width(line, max_inner))
        .collect();

    // Sized to the longest actual line - like the banner sizes itself to
    // its title/subtitle - rather than always stretching to the full
    // terminal width, so a short result doesn't get lost in an oversized
    // box.
    let inner_width = lines
        .iter()
        .chain(exit_line.iter())
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0)
        .max(1);

    let rule = "─".repeat(inner_width + 2);
    println!("{}", border(&format!("╭{rule}╮")));
    for line in &lines {
        println!(
            "{} {} {}",
            border("│"),
            content(&format!("{line:<inner_width$}")),
            border("│")
        );
    }
    if let Some(line) = &exit_line {
        let padded = format!("{line:<inner_width$}");
        let colored = if exit_code == Some("0") {
            green(&padded)
        } else {
            red(&padded)
        };
        println!("{} {} {}", border("│"), colored, border("│"));
    }
    println!("{}", border(&format!("╰{rule}╯")));
}

/// Splits run_command's `"exit code: N\n<output>"` result shape into the
/// code and the rest of the body - `None` if the text doesn't start with
/// that exact prefix (e.g. git/git_commit's plain output, which has no
/// exit code line at all).
fn split_exit_code(result: &str) -> (Option<&str>, &str) {
    match result.split_once('\n') {
        Some((first, rest)) if first.starts_with("exit code: ") => {
            (Some(&first["exit code: ".len()..]), rest)
        }
        _ => (None, result),
    }
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

/// Human-friendly verb shown in the "● <label>(...)" tool-call header,
/// in place of the raw snake_case tool name a model actually calls -
/// consistent with the Read/Edited/Ran verbs the end-of-turn tally
/// already uses. Falls back to the raw name for anything not listed here
/// (a future tool that hasn't been given a label yet), so it never
/// prints something misleadingly blank.
fn friendly_tool_label(tool_name: &str) -> &str {
    match tool_name {
        "read_file" => "Read",
        "list_directory" => "List",
        "tree" => "Tree",
        "find_files" => "Find",
        "search_code" => "Search",
        "write_file" => "Write",
        "append_file" => "Append",
        "edit_file" => "Edit",
        "delete_file" | "delete_directory" => "Delete",
        "move_file" => "Move",
        "create_directory" => "Create",
        "run_command" => "Run",
        "git" => "Git",
        "git_commit" => "Commit",
        "outline_file" => "Outline",
        "ask_question" => "Ask",
        other => other,
    }
}

fn format_tool_call(tool_name: &str, args: &Value) -> String {
    let label = friendly_tool_label(tool_name);

    if tool_name == "move_file" {
        if let (Some(from), Some(to)) = (
            args.get("from").and_then(Value::as_str),
            args.get("to").and_then(Value::as_str),
        ) {
            return format!("{label}({from} -> {to})");
        }
    }

    let summary = [
        "command",
        "path",
        "pattern",
        "keyword",
        "subcommand",
        "question",
    ]
    .into_iter()
    .find_map(|key| args.get(key).and_then(Value::as_str));

    match summary {
        Some(summary) => format!("{label}({summary})"),
        None => format!("{label}()"),
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

/// Rotated through every few seconds for the spinner label, Claude Code-
/// style ("Thinking...", "Brewing...", ...) - purely cosmetic liveliness
/// for a wait that can otherwise run long with nothing else on screen
/// (this is also what covers a reasoning model's hidden "thinking" phase,
/// since that trace itself is never printed - see chat_stream_openai's
/// own comment on why).
const SPINNER_VERBS: [&str; 10] = [
    "Thinking",
    "Pondering",
    "Brewing",
    "Percolating",
    "Mulling",
    "Ruminating",
    "Cogitating",
    "Musing",
    "Noodling",
    "Contemplating",
];

fn spinner_verb(elapsed_secs: u64) -> &'static str {
    const SECONDS_PER_VERB: u64 = 8;
    let index = (elapsed_secs / SECONDS_PER_VERB) as usize % SPINNER_VERBS.len();
    SPINNER_VERBS[index]
}

/// Runs for the whole turn (stopped externally via `.abort()`), rather
/// than exiting the moment `waiting` first goes false - `run_turn` flips
/// it back to true after each tool call, so this needs to keep ticking
/// and simply stay quiet in between, ready to resume drawing for the next
/// wait instead of having already exited after the first one. `is_local`
/// picks which long-wait note applies (see below) - a reasoning model
/// routed through an online provider can easily spend over a minute on a
/// hidden "thinking" trace before its first visible token, which isn't
/// the same "may be stuck" situation a local llama-server sitting at
/// max_tokens for that long usually is.
async fn spin(waiting: Arc<AtomicBool>, counts: Arc<SharedTurnCounts>, is_local: bool) {
    const FRAMES: [&str; 10] = ["-", "\\", "|", "/", "-", "\\", "|", "/", "*", "+"];
    let mut i = 0;
    let started = Instant::now();

    loop {
        // A tool is blocked on a y/N confirmation right now - stay quiet
        // instead of redrawing over that prompt every 90ms, which would
        // hide it and make KRIS look stuck "thinking" forever while it's
        // actually just waiting on the user.
        if waiting.load(Ordering::SeqCst)
            && !AWAITING_CONFIRMATION.load(Ordering::SeqCst)
            && !COMMAND_RUNNING.load(Ordering::SeqCst)
        {
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
            let verb = spinner_verb(elapsed);
            let mut label = if elapsed < 60 {
                format!("{verb}... {elapsed}s")
            } else if is_local {
                format!(
                    "{verb}... {elapsed}s (taking a while - if the model seems stuck in a \
                     repetitive loop, check ~/llama-server.log, or try `config set max_tokens \
                     256` to bound it)"
                )
            } else {
                format!(
                    "{verb}... {elapsed}s (a reasoning model can take a while before its \
                     first visible token - this is expected, not a hang)"
                )
            };

            // Live "Read N files · Ran N commands" recap, like Claude Code
            // shows while a turn is still in progress - it disappears with
            // the rest of this line once the turn finishes, rather than
            // being printed again afterward.
            if let Some(summary) = counts.snapshot().summary() {
                label = format!("{label} · {summary}");
            }

            // Regression guard: `\r\x1b[K` only rewinds/clears the terminal's
            // *current* line - once the "unusually long" note and a growing
            // tally push this past the terminal width, it wraps onto
            // several lines, and every 90ms tick then only clears the last
            // of those, leaving the earlier wrapped lines behind to pile up
            // as the terminal scrolls (confirmed on-device: dozens of
            // near-duplicate lines flooding a ~40-column Termux window).
            // Truncating to fit the actual terminal width guarantees this
            // is always a single line, so the redraw stays in place.
            let frame = FRAMES[i % FRAMES.len()];
            let prefix_width = frame.chars().count() + 1; // frame + one space
            let max_label_width = terminal_width()
                .saturating_sub(1) // leave the cursor's own column free
                .saturating_sub(prefix_width);
            let label = truncate_to_width(&label, max_label_width);

            print!("\r\x1b[K{} {}", dim(frame), dim(&label));
            let _ = std::io::stdout().flush();
            i += 1;
        }
        tokio::time::sleep(Duration::from_millis(90)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustyline::history::DefaultHistory;
    use std::sync::Mutex;

    fn complete_at_end(prefix: &str) -> Vec<String> {
        let helper = KrisHelper::new();
        let history = DefaultHistory::new();
        let ctx = RlContext::new(&history);
        helper.complete(prefix, prefix.len(), &ctx).unwrap().1
    }

    #[test]
    fn completer_suggests_matching_command_names() {
        let matches = complete_at_end("he");
        assert!(matches.contains(&"help".to_string()));
        assert!(matches.contains(&"health".to_string()));
        assert_eq!(matches.len(), 2);
    }

    #[test]
    fn completer_matches_a_single_command_exactly() {
        assert_eq!(complete_at_end("mod"), vec!["mode", "model"]);
        assert_eq!(complete_at_end("fix"), vec!["fix"]);
    }

    #[test]
    fn completer_offers_nothing_once_past_the_command_word() {
        // `mode onl` is an argument to `mode`, not a command name itself -
        // completing it against KNOWN_COMMANDS would be wrong.
        assert!(complete_at_end("mode onl").is_empty());
    }

    #[test]
    fn completer_offers_nothing_for_an_unknown_prefix() {
        assert!(complete_at_end("zzz").is_empty());
    }

    #[test]
    fn friendly_tool_label_covers_every_built_in_tool() {
        assert_eq!(friendly_tool_label("read_file"), "Read");
        assert_eq!(friendly_tool_label("write_file"), "Write");
        assert_eq!(friendly_tool_label("append_file"), "Append");
        assert_eq!(friendly_tool_label("edit_file"), "Edit");
        assert_eq!(friendly_tool_label("delete_file"), "Delete");
        assert_eq!(friendly_tool_label("delete_directory"), "Delete");
        assert_eq!(friendly_tool_label("move_file"), "Move");
        assert_eq!(friendly_tool_label("create_directory"), "Create");
        assert_eq!(friendly_tool_label("run_command"), "Run");
        assert_eq!(friendly_tool_label("git"), "Git");
        assert_eq!(friendly_tool_label("git_commit"), "Commit");
        assert_eq!(friendly_tool_label("outline_file"), "Outline");
        assert_eq!(friendly_tool_label("ask_question"), "Ask");
    }

    #[test]
    fn friendly_tool_label_falls_back_to_the_raw_name_for_anything_unlisted() {
        assert_eq!(friendly_tool_label("some_future_tool"), "some_future_tool");
    }

    #[test]
    fn format_tool_call_uses_the_friendly_label_with_its_argument() {
        assert_eq!(
            format_tool_call("read_file", &serde_json::json!({ "path": "a.rs" })),
            "Read(a.rs)"
        );
        assert_eq!(
            format_tool_call(
                "run_command",
                &serde_json::json!({ "command": "cargo test" })
            ),
            "Run(cargo test)"
        );
        assert_eq!(
            format_tool_call("write_file", &serde_json::json!({})),
            "Write()"
        );
    }

    #[test]
    fn format_tool_call_shows_move_file_as_a_friendly_arrow() {
        assert_eq!(
            format_tool_call(
                "move_file",
                &serde_json::json!({ "from": "a.rs", "to": "b.rs" })
            ),
            "Move(a.rs -> b.rs)"
        );
    }

    #[test]
    fn split_exit_code_extracts_the_code_and_leaves_the_rest() {
        let (code, body) = split_exit_code("exit code: 0\nrunning 3 tests\nok");
        assert_eq!(code, Some("0"));
        assert_eq!(body, "running 3 tests\nok");
    }

    #[test]
    fn split_exit_code_is_none_for_output_with_no_exit_code_line() {
        // git/git_commit's plain output, e.g.
        let (code, body) = split_exit_code("[main 1234abc] a commit message\n1 file changed");
        assert_eq!(code, None);
        assert_eq!(body, "[main 1234abc] a commit message\n1 file changed");
    }

    #[test]
    fn spinner_verb_stays_put_within_its_window_then_rotates() {
        assert_eq!(spinner_verb(0), "Thinking");
        assert_eq!(spinner_verb(7), "Thinking");
        assert_eq!(spinner_verb(8), "Pondering");
        assert_eq!(spinner_verb(15), "Pondering");
        assert_eq!(spinner_verb(16), "Brewing");
    }

    #[test]
    fn spinner_verb_wraps_around_after_the_last_one() {
        let period = 8 * SPINNER_VERBS.len() as u64;
        assert_eq!(spinner_verb(0), spinner_verb(period));
    }

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

    #[tokio::test]
    async fn looks_like_connection_error_treats_a_truncated_body_as_retryable() {
        // Regression test: a stream that dies partway through (connection
        // reset, proxy hiccup) surfaces as a reqwest "decode" error - that's
        // just how `bytes_stream()` (what the SSE reader polls) labels every
        // body-read failure, not necessarily a decoding/parsing problem -
        // not a "connect" or "timeout" one. Confirmed on-device against
        // OpenRouter, where this used to fall through to a dead end (no
        // retry, no restart attempt) instead of being treated as the same
        // kind of transient failure a dropped connection to llama-server is.
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf).await;
            // Promises far more body than it actually sends, then
            // disappears - the same shape as an SSE stream that dies
            // mid-response.
            let response = "HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\nshort";
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.shutdown().await;
        });

        let err = reqwest::get(format!("http://{addr}"))
            .await
            .expect("connecting and reading headers should succeed")
            .bytes()
            .await
            .expect_err("a body shorter than its own Content-Length should fail to read");

        assert!(err.is_decode());
        assert!(looks_like_connection_error(&err.into()));
    }

    #[test]
    fn looks_like_a_path_recognizes_paths_but_not_plain_names() {
        assert!(looks_like_a_path("~/projects"));
        assert!(looks_like_a_path("../elsewhere"));
        assert!(looks_like_a_path("/data/projects"));
        assert!(looks_like_a_path("sub/dir"));

        assert!(!looks_like_a_path("tridjaya"));
        assert!(!looks_like_a_path("my-project"));
        assert!(!looks_like_a_path(""));
    }

    // `handle_project` saves settings to `$HOME/.config/kris/config.toml`
    // as a side effect - these two tests point `$HOME` at a scratch
    // tempdir for their duration so they can't ever touch a real config
    // file, and share a lock since env vars are process-global and tests
    // run concurrently by default.
    static HOME_ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_scratch_home<T>(f: impl FnOnce(&Path) -> T) -> T {
        let _guard = HOME_ENV_LOCK.lock().unwrap();
        let original_home = std::env::var("HOME").ok();
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", tmp.path());

        let result = f(tmp.path());

        match original_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
        result
    }

    #[test]
    fn project_command_with_a_bare_name_switches_within_the_projects_folder() {
        with_scratch_home(|home| {
            let projects_dir = home.join("projects");
            fs::create_dir_all(projects_dir.join("tridjaya")).unwrap();

            let settings = Settings {
                workspace: projects_dir.display().to_string(),
                ..Settings::default()
            };
            let mut session = Session::new(settings);

            handle_project(&mut session, "tridjaya");

            assert_eq!(session.settings.active_project, "tridjaya");
            assert_eq!(
                session.settings.workspace,
                projects_dir.display().to_string()
            );
        });
    }

    #[test]
    fn project_command_with_a_path_changes_the_projects_folder_instead() {
        with_scratch_home(|home| {
            let old_projects_dir = home.join("projects");
            fs::create_dir_all(&old_projects_dir).unwrap();
            let new_projects_dir = home.join("elsewhere");

            let settings = Settings {
                workspace: old_projects_dir.display().to_string(),
                active_project: "stale".to_string(),
                ..Settings::default()
            };
            let mut session = Session::new(settings);

            handle_project(&mut session, new_projects_dir.to_str().unwrap());

            assert_eq!(
                session.settings.workspace,
                new_projects_dir.display().to_string()
            );
            assert!(new_projects_dir.is_dir());
            assert!(session.settings.active_project.is_empty());
        });
    }
}
