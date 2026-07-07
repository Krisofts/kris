use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use serde_json::Value;

use crate::agent::{Agent, Project};
use crate::config::Settings;
use crate::message::Message;
use crate::server;
use crate::style::{bold, cyan, dim, green, red, yellow};
use crate::tools::ToolRegistry;

const DEFAULT_MAX_ITERATIONS: u32 = 10;
const FIX_MIN_ITERATIONS: u32 = 24;

const MODEL_PRESETS: &[(&str, &str)] = &[
    ("1.5b", "qwen2.5-coder-1.5b-instruct-q4_k_m.gguf"),
    ("3b", "qwen2.5-coder-3b-instruct-q4_k_m.gguf"),
    ("7b", "qwen2.5-coder-7b-instruct-q4_k_m.gguf"),
];

struct Session {
    settings: Settings,
    root: PathBuf,
    project_name: String,
    project_type_hint: String,
    history: Vec<Message>,
}

impl Session {
    fn new(settings: Settings) -> Self {
        let root = PathBuf::from(&settings.workspace);
        let (project_name, project_type_hint) = project_hint(&root);

        Self {
            settings,
            root,
            project_name,
            project_type_hint,
            history: Vec::new(),
        }
    }

    fn switch_workspace(&mut self, path: &Path) {
        self.root = path.to_path_buf();
        let (name, hint) = project_hint(&self.root);
        self.project_name = name;
        self.project_type_hint = hint;
        self.history.clear();
        self.settings.workspace = self.root.display().to_string();
    }

    fn agent(&self) -> Agent {
        Agent::new(
            server::client_for(&self.settings),
            ToolRegistry::with_defaults(),
            self.settings.temperature,
            self.settings.max_tokens,
            self.settings.context_size,
        )
    }
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
    let mut session = Session::new(settings);

    std::fs::create_dir_all(&session.root).ok();

    if !server::ensure_running(&session.settings).await {
        return Ok(());
    }

    ask(&mut session, prompt).await;

    Ok(())
}

pub async fn run_interactive(settings: Settings) -> Result<()> {
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

fn print_banner(session: &Session) {
    println!("{}", bold("KRIS - offline coding assistant"));
    println!(
        "{}",
        dim(&format!(
            "workspace: {}  |  model: {}",
            session.root.display(),
            if session.settings.model_path.is_empty() {
                "(not configured)".to_string()
            } else {
                session.settings.model_path.clone()
            }
        ))
    );
    println!("{}", dim("Type your request, or `help` for commands."));
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
        "model" => handle_model(session, rest),
        "workspace" => handle_workspace(session, rest),
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

fn handle_workspace(session: &mut Session, arg: &str) {
    if arg.is_empty() {
        println!("Current workspace: {}", session.root.display());
        return;
    }

    let path = PathBuf::from(shellexpand_home(arg));
    if !path.is_dir() {
        println!("{}", red(&format!("{} is not a directory", path.display())));
        return;
    }

    session.switch_workspace(&path);
    if let Err(err) = session.settings.save() {
        println!("{}", red(&format!("Failed to save config: {err}")));
    }
    println!(
        "{}",
        green(&format!("Switched workspace to {}", session.root.display()))
    );
}

fn shellexpand_home(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest).display().to_string();
        }
    }
    path.to_string()
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
    println!("  health                check whether llama-server is reachable");
    println!("  serve                 start llama-server in the background if needed");
    println!("  model [preset]        show/switch the Qwen2.5-Coder model (1.5b/3b/7b)");
    println!("  workspace [path]      show/switch the project KRIS is working in");
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
        if looks_like_connection_error(&err) {
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
    println!(
        "{}",
        dim(&format!(
            "Make sure llama-server is running at {}",
            session.settings.llama_url
        ))
    );
}

async fn run_turn(session: &mut Session, prompt: &str, max_iterations: u32) -> Result<()> {
    let agent = session.agent();
    let root = session.root.clone();
    let project_name = session.project_name.clone();
    let project_type_hint = session.project_type_hint.clone();

    let waiting = Arc::new(AtomicBool::new(true));
    let spinner_waiting = waiting.clone();
    let spinner = tokio::spawn(spin(spinner_waiting));

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
                println!("{} {}", dim("*"), bold(&format_tool_call(tool_name, args)));
                if let Some(err) = result.strip_prefix("Error: ") {
                    println!("  {} {}", red("x"), red(err));
                }
            },
        )
        .await;

    spinner.abort();
    clear_line();

    match result {
        Ok(answer) => {
            println!();
            if !answer.is_empty() {
                println!("{answer}");
            }
            Ok(())
        }
        Err(err) => Err(err),
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
    print!("\r{}\r", " ".repeat(40));
    let _ = std::io::stdout().flush();
}

async fn spin(waiting: Arc<AtomicBool>) {
    const FRAMES: [&str; 10] = ["-", "\\", "|", "/", "-", "\\", "|", "/", "*", "+"];
    let mut i = 0;

    while waiting.load(Ordering::SeqCst) {
        print!("\r{} {}", dim(FRAMES[i % FRAMES.len()]), dim("thinking..."));
        let _ = std::io::stdout().flush();
        i += 1;
        tokio::time::sleep(Duration::from_millis(90)).await;
    }
}
