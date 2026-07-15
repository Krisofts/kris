use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
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
use crate::export;
use crate::kris_md;
use crate::message::Message;
use crate::picker;
use crate::server;
use crate::session_store;
use crate::style::{blue, bold, cyan, dim, green, red, yellow};
use crate::term::{terminal_width, truncate_to_width};
use crate::tools::{
    tool_category, SharedTurnCounts, ToolCategory, ToolRegistry, AWAITING_CONFIRMATION,
    COMMAND_RUNNING,
};

// Raised from 10/24/20: a multi-file task (scaffold a project, add a
// feature, verify with clippy/tests) easily needs more tool calls than
// that, especially now that append_file encourages building a long new
// file as several small chunks rather than one big write_file - each
// chunk is its own iteration. Confirmed on-device: a full-stack scaffold
// (frontend + backend + curl smoke tests) ran past 20 well before it had
// a final answer.
const DEFAULT_MAX_ITERATIONS: u32 = 40;
const FIX_MIN_ITERATIONS: u32 = 40;

/// Every REPL command `dispatch` recognizes as the first word of a line -
/// kept as its own list (rather than derived from `dispatch`'s `match`)
/// so tab-completion has something to complete against without needing
/// to parse that function.
const KNOWN_COMMANDS: &[&str] = &[
    "help",
    "version",
    "clear",
    "health",
    "mode",
    "project",
    "resume",
    "export",
    "compact",
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
        let history = session_store::load(&root);
        kris_md::seed_new_project(&root);

        Self {
            settings,
            root,
            project_name,
            project_type_hint,
            history,
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

    /// Switches to whatever `root` now resolves to and loads *that*
    /// project's own persisted session (empty if it's never had one) -
    /// rather than always starting blank, so flipping between two
    /// projects resumes each one's own last conversation instead of
    /// losing it the moment you switch away.
    fn refresh_root(&mut self) {
        self.root = resolve_root(&self.settings.workspace, &self.settings.active_project);
        let (name, hint) = project_hint(&self.root);
        self.project_name = name;
        self.project_type_hint = hint;
        self.history = session_store::load(&self.root);
        kris_md::seed_new_project(&self.root);
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

    /// Switches straight to an arbitrary project root, wherever it lives -
    /// used by `resume`, since a saved session's project isn't necessarily
    /// a subfolder of the *current* workspace the way `switch_project`
    /// assumes. Sets `workspace` to `root`'s parent and `active_project` to
    /// its own folder name, so it behaves exactly like having switched
    /// workspace and picked that project in one step.
    fn switch_to_root(&mut self, root: &Path) {
        if let Some(parent) = root.parent() {
            self.settings.workspace = parent.display().to_string();
        }
        self.settings.active_project = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
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
    let subtitle = "online coding assistant";
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
            session_store::clear(&session.root);
            print!("\x1b[2J\x1b[H");
            let _ = std::io::stdout().flush();
            println!("{}", dim("Conversation history cleared."));
        }
        "health" => {
            server::ensure_running(&session.settings).await;
        }
        "mode" => handle_mode(session, rest),
        "project" => handle_project(session, rest),
        "resume" => handle_resume(session),
        "export" => handle_export(session, rest),
        "compact" => handle_compact(session, rest).await,
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
        Provider::Gemini => ("gemini", settings.gemini_model.clone()),
        Provider::Claude => ("claude", settings.claude_model.clone()),
        Provider::OpenRouter => ("openrouter", settings.openrouter_model.clone()),
        Provider::Opper => ("opper", settings.opper_model.clone()),
        Provider::Opencode => ("opencode", settings.opencode_model.clone()),
    }
}

/// Switches between the five online providers (Gemini, Claude, OpenRouter,
/// Opper, or OpenCode Zen) at runtime. Clears the conversation, since
/// providers don't share context and a history built against one model is
/// best restarted on another. Accepts `gemini`, `claude`/`anthropic`,
/// `openrouter`/`or`, `opper`, and `opencode`/`zen`.
fn handle_mode(session: &mut Session, arg: &str) {
    if arg.is_empty() {
        let (mode, model) = describe_mode(&session.settings);
        println!("Current mode: {mode} ({model})");
        println!("Usage: mode gemini     use the Gemini API");
        println!("       mode claude     use the Claude API");
        println!("       mode openrouter use the OpenRouter API");
        println!("       mode opper      use the Opper API");
        println!("       mode opencode   use the OpenCode Zen API");
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
        Provider::Opper => {
            println!(
                "{}",
                green(&format!(
                    "Switched to Opper mode ({}).",
                    session.settings.opper_model
                ))
            );
            if session.settings.resolved_api_key().is_none() {
                println!(
                    "{}",
                    yellow(
                        "No API key set - export OPPER_API_KEY, or `config set opper_api_key <key>`."
                    )
                );
            }
        }
        Provider::Opencode => {
            println!(
                "{}",
                green(&format!(
                    "Switched to OpenCode Zen mode ({}).",
                    session.settings.opencode_model
                ))
            );
            if session.settings.resolved_api_key().is_none() {
                println!(
                    "{}",
                    yellow(
                        "No API key set - export OPENCODE_API_KEY, or `config set opencode_api_key <key>`."
                    )
                );
            }
        }
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

/// KRIS's counterpart to Claude Code's own `/resume`: shows a picker of
/// every project with a saved conversation (most recently used first,
/// across the whole machine rather than scoped to the current workspace -
/// KRIS has no notion of "worktree" to scope by) and switches straight to
/// whichever one is chosen, loading its session in the process.
fn handle_resume(session: &mut Session) {
    let sessions = session_store::list_sessions();

    if sessions.is_empty() {
        println!("{}", yellow("Belum ada sesi tersimpan untuk dilanjutkan."));
        return;
    }

    let labels: Vec<String> = sessions
        .iter()
        .map(|s| {
            let name = s
                .root
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| s.root.display().to_string());
            let ago = format_time_ago(s.modified.elapsed().unwrap_or_default());
            format!(
                "{name} ({}) - {} pesan - {ago}",
                s.root.display(),
                s.message_count
            )
        })
        .collect();

    let prompt =
        "Pilih sesi untuk dilanjutkan (\u{2191}/\u{2193} pilih, Enter konfirmasi, Esc batal):";

    match picker::pick(prompt, &labels, None) {
        picker::PickOutcome::Chosen(label) => {
            let index = labels.iter().position(|l| *l == label).unwrap();
            let root = sessions[index].root.clone();
            session.switch_to_root(&root);
            if let Err(err) = session.settings.save() {
                println!("{}", red(&format!("Failed to save config: {err}")));
            }
            println!(
                "{}",
                green(&format!(
                    "Melanjutkan sesi di {} ({} pesan)",
                    session.root.display(),
                    session.history.len()
                ))
            );
        }
        picker::PickOutcome::Cancelled => {}
        picker::PickOutcome::Unavailable => {
            println!("Sesi tersimpan:");
            for (i, label) in labels.iter().enumerate() {
                println!("  {}) {label}", i + 1);
            }
            println!("{}", dim("Terminal ini tidak mendukung picker interaktif."));
        }
    }
}

/// Rough "N ago" label for the `resume` picker - bucketed coarsely (not
/// exact calendar math) since it only needs to convey "recent" vs "a while
/// back", the same spirit as `format_elapsed`'s own rough rounding.
fn format_time_ago(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    if secs < 60 {
        "baru saja".to_string()
    } else if secs < 3600 {
        format!("{} menit lalu", secs / 60)
    } else if secs < 86400 {
        format!("{} jam lalu", secs / 3600)
    } else {
        format!("{} hari lalu", secs / 86400)
    }
}

/// KRIS's counterpart to Claude Code's own `/export`: writes the current
/// conversation out as readable Markdown (unlike the JSON `session_store`
/// persists purely for KRIS itself to reload) so it can be read, shared,
/// or handed off to someone else. `arg` is an optional filename (relative
/// to the project root); defaults to a name that won't collide with a
/// previous export in the same project.
fn handle_export(session: &Session, arg: &str) {
    if session.history.is_empty() {
        println!(
            "{}",
            yellow("Belum ada percakapan untuk diekspor di project ini.")
        );
        return;
    }

    let filename = if arg.is_empty() {
        default_export_filename()
    } else {
        arg.to_string()
    };
    let path = session.root.join(&filename);
    let content = export::render_export(&session.history);

    match fs::write(&path, content) {
        Ok(()) => println!(
            "{}",
            green(&format!("Percakapan diekspor ke {}", path.display()))
        ),
        Err(err) => println!(
            "{}",
            red(&format!("Gagal menulis {}: {err}", path.display()))
        ),
    }
}

fn default_export_filename() -> String {
    let epoch_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("kris-export-{epoch_secs}.md")
}

/// KRIS's counterpart to Claude Code's own `/compact`: asks the model to
/// summarize the conversation so far, then replaces `history` with just
/// that recap instead of KRIS's usual context-budget trimming
/// (`enforce_context_budget`), which only ever drops old turns outright.
/// Leaves the original conversation untouched if summarizing fails, so a
/// network hiccup never destroys history in exchange for nothing.
async fn handle_compact(session: &mut Session, extra: &str) {
    if session.history.is_empty() {
        println!("{}", yellow("Belum ada percakapan untuk diringkas."));
        return;
    }

    if !server::check_health(&session.settings).await
        && !server::ensure_running(&session.settings).await
    {
        return;
    }

    let agent = session.agent();
    let system_prompt = agent.system_prompt(&session.project_name, &session.project_type_hint);

    println!();
    println!("{}", dim("Meringkas percakapan..."));
    println!();

    let result = agent
        .summarize(&session.history, extra, |delta: &str| {
            print!("{delta}");
            let _ = std::io::stdout().flush();
        })
        .await;

    let summary = match result {
        Ok(summary) if !summary.trim().is_empty() => summary,
        Ok(_) => {
            println!(
                "{}",
                red("Model tidak memberikan ringkasan - percakapan tidak diubah.")
            );
            return;
        }
        Err(err) => {
            println!();
            println!("{} {err}", red("Gagal meringkas percakapan:"));
            return;
        }
    };

    println!();
    println!();

    session.history = build_compacted_history(system_prompt, &summary);
    if let Err(err) = session_store::save(&session.root, &session.history) {
        println!(
            "{}",
            yellow(&format!("(couldn't save session to disk: {err})"))
        );
    }
    println!(
        "{}",
        green("Percakapan diringkas - sesi dilanjutkan dari ringkasan ini.")
    );
}

/// Builds the replacement history after a `compact`: just the system
/// prompt (so `Agent::run`'s `history.is_empty()` check never wrongly
/// inserts a second one on the next turn) followed by the summary itself,
/// presented as an assistant turn recapping progress so far.
fn build_compacted_history(system_prompt: String, summary: &str) -> Vec<Message> {
    vec![
        Message::system(system_prompt),
        Message::assistant_text(format!(
            "(Ringkasan percakapan sebelumnya, dibuat oleh `compact`)\n\n{summary}"
        )),
    ]
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
        "  mode [gemini|claude|openrouter|opper|opencode] show/switch between Gemini, Claude, OpenRouter, Opper, and OpenCode Zen"
    );
    println!("  health                check whether the active provider has an API key configured");
    println!("  project [name|path]   pick a project with arrow keys, switch straight to <name>, or pass a <path> (e.g. ~/projects) to change the projects folder itself");
    println!("  resume                pick a saved session (any project) with arrow keys and switch straight to it");
    println!("  export [filename]     save the current conversation as readable Markdown");
    println!("  compact [instructions] summarize the conversation so far and continue from that recap instead of the full history");
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

/// Bounds how many times a single turn will retry after a connection-
/// looking failure (2 retries on top of the first attempt) - enough to
/// ride out a flaky mobile connection or a free-tier provider's brief
/// hiccup without retrying forever against something actually broken.
const MAX_CONNECTION_RETRIES: u32 = 2;

/// Runs one turn, retrying (via `server::ensure_running`) up to
/// `MAX_CONNECTION_RETRIES` times if the request fails with what looks
/// like a connection error - covers a flaky connection to the active
/// provider.
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
    // already completed, `run` keeps them instead - recorded here so each
    // retry can tell which case it's in.
    let history_len_before = session.history.len();
    let mut current_prompt = prompt.to_string();

    for attempt in 0..=MAX_CONNECTION_RETRIES {
        let err = match run_turn(session, &current_prompt, max_iterations).await {
            Ok(()) => return,
            Err(err) => err,
        };

        if !looks_like_connection_error(&err) || attempt == MAX_CONNECTION_RETRIES {
            print_turn_error(session, &err);
            return;
        }

        let message = format!(
            "Lost connection to the model - retrying ({}/{MAX_CONNECTION_RETRIES})...",
            attempt + 1
        );
        println!();
        println!("{}", yellow(&message));

        if !server::ensure_running(&session.settings).await {
            print_turn_error(session, &err);
            return;
        }

        // A short, growing backoff instead of retrying again immediately,
        // mirroring the client's own retry pacing for the initial connect
        // (client.rs's MAX_ATTEMPTS loop).
        tokio::time::sleep(Duration::from_secs(2 * (attempt as u64 + 1))).await;

        // Progress survived the disconnect (history grew past its pre-turn
        // length) - nudge the model to pick up from there instead of
        // resending the whole original prompt, which would otherwise read
        // as a request to redo the task from scratch even though most of
        // it is already done.
        current_prompt = if session.history.len() > history_len_before {
            "Koneksi ke model sempat putus. Lanjutkan dari yang sudah dikerjakan sejauh ini - \
             jangan mengulang dari awal."
                .to_string()
        } else {
            prompt.to_string()
        };
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
        Provider::Opper => format!(
            "Check your network connection and that OPPER_API_KEY is valid (model {}).",
            session.settings.opper_model
        ),
        Provider::Opencode => format!(
            "Check your network connection and that OPENCODE_API_KEY is valid (model {}).",
            session.settings.opencode_model
        ),
    };
    println!("{}", dim(&hint));
}

/// Heuristic token count for whatever's been added to `history` since
/// `turn_start`, clamped so a `turn_start` captured before the turn began
/// can't index past the end of `history` if it's since gotten *shorter*
/// than that - `enforce_context_budget` can drain old turns from the
/// front mid-turn on a long enough one, shifting every later index down.
/// Confirmed on-device: without the clamp this panicked ("range start
/// index out of range") and killed the whole process; clamping just
/// under-reports this turn's token count as 0 in that specific case
/// instead - a cosmetic footer, not worth losing the session over.
fn tokens_since(history: &[Message], turn_start: usize) -> usize {
    heuristic_tokens(&history[turn_start.min(history.len())..])
}

async fn run_turn(session: &mut Session, prompt: &str, max_iterations: u32) -> Result<()> {
    let agent = session.agent();
    let root = session.root.clone();
    let project_name = session.project_name.clone();
    let project_type_hint = session.project_type_hint.clone();

    let waiting = Arc::new(AtomicBool::new(true));
    let spinner_waiting = waiting.clone();
    // The same shared tally `run_command`'s own live status line reads
    // from (see `ToolRegistry::turn_counts`) - not a separate instance -
    // so a slow command in progress shows this turn's real "Read N ·
    // Edited N · Ran N" recap instead of going blank the whole time.
    let counts = agent.turn_counts();
    let spinner_counts = counts.clone();
    let activity: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let spinner_activity = activity.clone();
    // Set once a reasoning model's hidden "thinking" trace actually shows
    // up on the wire (never its content, just the fact that it started) -
    // real proof the model is working, so the spinner can switch out of
    // its plain elapsed-time display into something livelier. Reset back
    // to false every time `waiting` is re-armed for the next iteration's
    // wait, so a later iteration doesn't inherit an earlier one's state.
    let reasoning_started = Arc::new(AtomicBool::new(false));
    let spinner_reasoning_started = reasoning_started.clone();
    let spinner = tokio::spawn(spin(
        spinner_waiting,
        spinner_counts,
        spinner_activity,
        spinner_reasoning_started,
    ));

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
                println!(
                    "{} {}",
                    tool_bullet(tool_name),
                    bold(&format_tool_call(tool_name, args))
                );
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
                        println!(
                            "{}",
                            if is_error {
                                red(&line)
                            } else {
                                colorize_diff_stat(&line)
                            }
                        );
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
                // Next iteration's wait starts fresh in "no reasoning yet"
                // territory - it shouldn't inherit this iteration's own.
                reasoning_started.store(false, Ordering::SeqCst);
                // This tool call is done, so it's no longer "being
                // prepared" - clear it rather than let it linger and
                // wrongly describe whatever the next iteration turns out
                // to be.
                if let Ok(mut current) = activity.lock() {
                    current.clear();
                }
            },
            |tool_name: &str| {
                let label = friendly_tool_label(tool_name);
                if let Ok(mut current) = activity.lock() {
                    *current = label.to_string();
                }
            },
            || {
                reasoning_started.store(true, Ordering::SeqCst);
            },
        )
        .await;

    spinner.abort();
    clear_line();

    // Persisted regardless of Ok/Err - agent.run() keeps whatever tool
    // calls/results already completed even when the turn itself ends in
    // an error (a dropped connection mid-task shouldn't throw away
    // progress), so the session file should reflect that too rather than
    // only ever saving on a clean finish.
    if let Err(save_err) = session_store::save(&session.root, &session.history) {
        println!(
            "{}",
            yellow(&format!("(couldn't save session to disk: {save_err})"))
        );
    }

    match result {
        // `answer` isn't reprinted here - agent.run() has already streamed
        // it in full via the `on_delta` callback above (live as tokens
        // arrived, or flushed in one piece for a held-back/synthetic
        // message), so doing it again would just duplicate the output.
        Ok(_answer) => {
            let elapsed = started.elapsed();
            let tokens = tokens_since(&session.history, history_len_before);
            // Whole conversation so far, not just this turn's share of it -
            // a rough heuristic (same chars/4 estimate used elsewhere as a
            // fallback), but good enough to warn before the "older
            // conversation history trimmed" notice shows up out of nowhere.
            let context_used = heuristic_tokens(&session.history);
            let context_budget = session.settings.effective_context_size();
            println!();
            // The "Read N files · Ran N commands" tally lived only in the
            // spinner label above while the turn was in progress - like
            // Claude Code, it isn't repeated here once things are done.
            println!(
                "{}",
                dim(&format!(
                    "{} · {} · {}",
                    format_elapsed(elapsed),
                    format_tokens(tokens),
                    format_context_usage(context_used, context_budget)
                ))
            );
            Ok(())
        }
        Err(err) => Err(err),
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

/// Rough percentage of the active provider's context budget the whole
/// conversation is currently using - shown so a trim (or a provider
/// rejecting an oversized request) is never a surprise. `budget` of 0
/// would divide by zero; treated as "unknown" instead of panicking, even
/// though `Settings::set_field` already refuses to save a zero context
/// size in practice.
fn format_context_usage(used: usize, budget: u32) -> String {
    if budget == 0 {
        return "context: unknown".to_string();
    }
    let pct = (used as f64 / budget as f64 * 100.0).round() as u64;
    format!("{pct}% context")
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

/// Colors the leading "●" by which of Claude Code's three broad action
/// groups the tool belongs to - blue for a read, green for an edit, yellow
/// for a command - so a scroll of tool calls sorts itself out at a glance
/// on a narrow Termux screen instead of every line starting with the same
/// undifferentiated cyan dot regardless of what actually happened.
fn tool_bullet(tool_name: &str) -> String {
    match tool_category(tool_name) {
        ToolCategory::Read => blue("●"),
        ToolCategory::Edit => green("●"),
        ToolCategory::Command => yellow("●"),
        ToolCategory::Other => cyan("●"),
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

/// Recolors a trailing "(+N -M)" diff-stat suffix (as emitted by
/// write_file/append_file/edit_file's result strings) green/red within an
/// otherwise dim tool-result summary line - insertions green, removals red,
/// matching how the full diff view (`render_unified_diff`) already colors
/// its own +/- lines, instead of the stat blending into the single dim
/// gray the rest of the line uses. Falls back to plain `dim` for any line
/// that doesn't actually end in that shape (most tool results don't).
fn colorize_diff_stat(line: &str) -> String {
    if let Some(open) = line.rfind("(+") {
        let rest = &line[open..];
        if let Some(close_rel) = rest.find(')') {
            let inner = &rest[1..close_rel];
            if let Some((add_part, rem_part)) = inner.split_once(' ') {
                if let (Some(add_digits), Some(rem_digits)) =
                    (add_part.strip_prefix('+'), rem_part.strip_prefix('-'))
                {
                    let digits_ok =
                        |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
                    if digits_ok(add_digits) && digits_ok(rem_digits) {
                        let before = &line[..open];
                        let after = &rest[close_rel + 1..];
                        return format!(
                            "{}({} {}){}",
                            dim(before),
                            green(&format!("+{add_digits}")),
                            red(&format!("-{rem_digits}")),
                            dim(after)
                        );
                    }
                }
            }
        }
    }
    dim(line)
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

/// "5s" below a minute, "1m 05s" past it - a plain seconds count keeps
/// growing into an unreadable number on a genuinely long wait, so once
/// there's more than a minute of it, showing minutes and seconds separately
/// stays easy to read at a glance.
fn format_wait_elapsed(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m {:02}s", secs / 60, secs % 60)
    }
}

/// A small animated ellipsis for the reasoning-phase spinner label, cycling
/// every tick - purely cosmetic liveliness distinct from the plain elapsed
/// counter used before reasoning shows up.
fn reasoning_dots(tick: usize) -> &'static str {
    const DOTS: [&str; 4] = ["", ".", "..", "..."];
    DOTS[tick % DOTS.len()]
}

/// Runs for the whole turn (stopped externally via `.abort()`), rather
/// than exiting the moment `waiting` first goes false - `run_turn` flips
/// it back to true after each tool call, so this needs to keep ticking
/// and simply stay quiet in between, ready to resume drawing for the next
/// wait instead of having already exited after the first one. `activity`
/// is set by `Agent::run`'s `on_activity` callback to the tool the model
/// is currently in the middle of calling (cleared once it's actually
/// executed or a fresh iteration starts) - shown once known instead of
/// leaving the generic rotating verb as the only signal the whole time.
/// `reasoning_started` is set by `Agent::run`'s `on_reasoning` callback the
/// moment a reasoning model's hidden trace actually shows up on the wire -
/// real proof of life, distinct from simply still waiting on a response.
async fn spin(
    waiting: Arc<AtomicBool>,
    counts: Arc<SharedTurnCounts>,
    activity: Arc<Mutex<String>>,
    reasoning_started: Arc<AtomicBool>,
) {
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
            let is_reasoning = reasoning_started.load(Ordering::SeqCst);

            let mut label = if is_reasoning {
                // Nothing left to prove at this point - the wire itself
                // confirmed the model is actively reasoning, so this drops
                // the elapsed counter in favor of a livelier, more colorful
                // indicator instead of repeating the same reassurance.
                format!("Reasoning{}", reasoning_dots(i))
            } else {
                // A visible running clock, not just a spinning glyph, so a
                // genuinely long wait (a slow tool call, or a reasoning
                // model whose trace hasn't shown up yet) reads as "still
                // working, N seconds so far" instead of looking identically
                // frozen at 5s and at 5 minutes. Past a minute, that's long
                // enough it's worth a nudge that this is expected rather
                // than letting it keep spinning silently.
                let verb = spinner_verb(elapsed);
                let wait = format_wait_elapsed(elapsed);
                if elapsed < 60 {
                    format!("{verb}... {wait}")
                } else {
                    format!(
                        "{verb}... {wait} (a reasoning model can take a while before its \
                         first visible token - this is expected, not a hang)"
                    )
                }
            };

            // What's actually happening behind the scenes right now, once
            // known, instead of just the decorative rotating verb - the
            // model may be several seconds into streaming a tool call's
            // arguments well before that tool is actually run.
            if let Ok(current) = activity.lock() {
                if !current.is_empty() {
                    label = format!("{label} · preparing {}", *current);
                }
            }

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

            // Brighter cyan instead of the usual dim gray once reasoning is
            // confirmed underway - a splash of color reads as more "alive"
            // than the same muted tone used for the plain "still waiting"
            // phase.
            if is_reasoning {
                print!("\r\x1b[K{} {}", cyan(frame), cyan(&label));
            } else {
                print!("\r\x1b[K{} {}", dim(frame), dim(&label));
            }
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
        assert_eq!(complete_at_end("mod"), vec!["mode"]);
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
    fn tool_bullet_colors_by_category() {
        assert_eq!(tool_bullet("read_file"), blue("●"));
        assert_eq!(tool_bullet("edit_file"), green("●"));
        assert_eq!(tool_bullet("run_command"), yellow("●"));
        assert_eq!(tool_bullet("ask_question"), cyan("●"));
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
    fn colorize_diff_stat_recolors_insertions_green_and_removals_red() {
        let out = colorize_diff_stat("  ⎿ Wrote 123 bytes to a.rs (+6 -8)");
        assert!(out.contains(&green("+6")));
        assert!(out.contains(&red("-8")));
    }

    #[test]
    fn colorize_diff_stat_falls_back_to_dim_without_a_stat_suffix() {
        let out = colorize_diff_stat("  ⎿ some other tool output");
        assert_eq!(out, dim("  ⎿ some other tool output"));
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
    fn format_context_usage_rounds_to_a_percentage() {
        assert_eq!(format_context_usage(0, 8192), "0% context");
        assert_eq!(format_context_usage(4096, 8192), "50% context");
        assert_eq!(format_context_usage(8192, 8192), "100% context");
        // Over budget (a request the next call would trim) still reports
        // honestly rather than clamping at 100%.
        assert_eq!(format_context_usage(9000, 8192), "110% context");
    }

    #[test]
    fn format_context_usage_handles_a_zero_budget_without_dividing_by_it() {
        assert_eq!(format_context_usage(100, 0), "context: unknown");
    }

    #[test]
    fn tokens_since_clamps_a_stale_turn_start_instead_of_panicking() {
        // Regression test: mid-turn context trimming (enforce_context_budget)
        // drains old turns from the *front* of history, shifting every
        // later index down - a `turn_start` captured before that trim can
        // end up pointing past the end of the now-shorter history, which
        // used to panic with "range start index out of range" and take
        // the whole process down with it.
        let history = vec![Message::user("hi")];
        assert_eq!(tokens_since(&history, 5), 0);
    }

    #[test]
    fn tokens_since_counts_normally_when_turn_start_is_in_range() {
        let history = vec![
            Message::user("hi"),
            Message::assistant_text("hello there, how can I help?".to_string()),
        ];
        assert_eq!(tokens_since(&history, 1), heuristic_tokens(&history[1..]));
        assert!(tokens_since(&history, 1) > 0);
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
        // kind of transient failure any other dropped connection is.
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
    // (and now also loads/saves session files under `$HOME/.config/kris/
    // sessions/`) as a side effect - these tests point `$HOME` at a
    // scratch tempdir for their duration so they can't ever touch a real
    // config file. Shares `crate::test_support::HOME_ENV_LOCK` with every
    // other module's `with_scratch_home`, not a lock of its own - env vars
    // are process-global, so a per-module lock alone doesn't stop this
    // module's test from racing a *different* module's test over the same
    // `$HOME` when both run concurrently on separate threads (confirmed:
    // this was an actual flaky failure once session_store.rs grew its own
    // separate lock).
    fn with_scratch_home<T>(f: impl FnOnce(&Path) -> T) -> T {
        let _guard = crate::test_support::HOME_ENV_LOCK.lock().unwrap();
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
    fn switching_projects_resumes_each_projects_own_persisted_session() {
        // Regression coverage for session persistence: switching away from
        // a project used to just clear `history` outright (`refresh_root`
        // called `self.history.clear()`), so a KRIS restart or a `project`
        // switch and back lost the conversation entirely. It should now
        // resume whatever was last saved for that specific project root.
        with_scratch_home(|home| {
            let projects_dir = home.join("projects");
            fs::create_dir_all(projects_dir.join("alpha")).unwrap();
            fs::create_dir_all(projects_dir.join("beta")).unwrap();

            let settings = Settings {
                workspace: projects_dir.display().to_string(),
                ..Settings::default()
            };
            let mut session = Session::new(settings);

            handle_project(&mut session, "alpha");
            session.history.push(Message::user("working on alpha"));
            session_store::save(&session.root, &session.history).unwrap();

            handle_project(&mut session, "beta");
            assert!(
                session.history.is_empty(),
                "beta has never been saved, so it should start empty"
            );
            session.history.push(Message::user("working on beta"));
            session_store::save(&session.root, &session.history).unwrap();

            handle_project(&mut session, "alpha");
            assert_eq!(
                session.history.last().and_then(|m| m.content.as_deref()),
                Some("working on alpha")
            );

            handle_project(&mut session, "beta");
            assert_eq!(
                session.history.last().and_then(|m| m.content.as_deref()),
                Some("working on beta")
            );
        });
    }

    #[test]
    fn switch_to_root_updates_workspace_and_active_project_and_loads_its_history() {
        // `resume` can land on a project that isn't under the *current*
        // workspace at all (a session saved while a different workspace
        // folder was active) - switch_to_root has to repoint `workspace`
        // itself, not just `active_project`, and pick up that project's
        // own persisted session in the process.
        with_scratch_home(|home| {
            let other_workspace = home.join("elsewhere");
            let root = other_workspace.join("tridjaya");
            fs::create_dir_all(&root).unwrap();
            session_store::save(&root, &[Message::user("resumed from elsewhere")]).unwrap();

            let settings = Settings {
                workspace: home.join("projects").display().to_string(),
                ..Settings::default()
            };
            let mut session = Session::new(settings);

            session.switch_to_root(&root);

            assert_eq!(
                session.settings.workspace,
                other_workspace.display().to_string()
            );
            assert_eq!(session.settings.active_project, "tridjaya");
            assert_eq!(session.root, root);
            assert_eq!(
                session.history.last().and_then(|m| m.content.as_deref()),
                Some("resumed from elsewhere")
            );
        });
    }

    #[test]
    fn handle_export_writes_readable_markdown_to_the_project_root() {
        with_scratch_home(|home| {
            let projects_dir = home.join("projects");
            fs::create_dir_all(&projects_dir).unwrap();
            let settings = Settings {
                workspace: projects_dir.display().to_string(),
                ..Settings::default()
            };
            let mut session = Session::new(settings);
            session.history.push(Message::user("halo"));
            session.history.push(Message::assistant_text("halo juga!"));

            handle_export(&session, "export-test.md");

            let content = fs::read_to_string(projects_dir.join("export-test.md")).unwrap();
            assert!(content.contains("## You"));
            assert!(content.contains("halo"));
            assert!(content.contains("## KRIS"));
            assert!(content.contains("halo juga!"));
        });
    }

    #[test]
    fn handle_export_writes_nothing_when_history_is_empty() {
        with_scratch_home(|home| {
            let projects_dir = home.join("projects");
            fs::create_dir_all(&projects_dir).unwrap();
            let settings = Settings {
                workspace: projects_dir.display().to_string(),
                ..Settings::default()
            };
            let session = Session::new(settings);

            handle_export(&session, "should-not-exist.md");

            assert!(!projects_dir.join("should-not-exist.md").exists());
        });
    }

    #[test]
    fn format_time_ago_buckets_by_rough_scale() {
        assert_eq!(format_time_ago(Duration::from_secs(5)), "baru saja");
        assert_eq!(format_time_ago(Duration::from_secs(120)), "2 menit lalu");
        assert_eq!(format_time_ago(Duration::from_secs(3 * 3600)), "3 jam lalu");
        assert_eq!(
            format_time_ago(Duration::from_secs(2 * 86400)),
            "2 hari lalu"
        );
    }

    #[test]
    fn build_compacted_history_keeps_the_system_prompt_first() {
        // Regression coverage: history.is_empty() is what makes Agent::run
        // insert a fresh system prompt - if a compacted history didn't
        // still start with one, the very next real turn would either go
        // out with no system prompt at all, or (if the check were somehow
        // bypassed) end up with two. Either way this is the one invariant
        // that must hold for a compacted session to behave like a normal
        // one afterward.
        let history = build_compacted_history("be helpful".to_string(), "did X, next is Y");

        assert_eq!(history.len(), 2);
        assert!(!history.is_empty(), "must never look like a fresh session");
        assert_eq!(history[0].role, crate::message::Role::System);
        assert_eq!(history[0].content.as_deref(), Some("be helpful"));
        assert_eq!(history[1].role, crate::message::Role::Assistant);
        assert!(history[1]
            .content
            .as_deref()
            .unwrap()
            .contains("did X, next is Y"));
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
