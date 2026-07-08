use std::cell::Cell;
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::Ordering;

use serde_json::{json, Value};

use super::{Tool, ToolError, AWAITING_CONFIRMATION};

const MAX_OUTPUT: usize = 4000;
const ALLOWED: [&str; 5] = ["status", "diff", "log", "show", "branch"];

pub struct GitTool;

impl Tool for GitTool {
    fn name(&self) -> &'static str {
        "git"
    }

    fn description(&self) -> &'static str {
        "Run a read-only git inspection command (status, diff, log, show, or branch) \
         inside the project. Never modifies anything, so unlike run_command this needs \
         no confirmation - for commits, pushes, resets, etc. use run_command instead."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "subcommand": { "type": "string", "description": "One of: status, diff, log, show, branch" },
                "args": { "type": "string", "description": "Extra arguments, e.g. a file path for diff, or a commit hash for show" }
            },
            "required": ["subcommand"]
        })
    }

    fn execute(&self, root: &Path, args: &Value) -> Result<String, ToolError> {
        let subcommand = args
            .get("subcommand")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::InvalidArgs("subcommand".to_string()))?;

        if !ALLOWED.contains(&subcommand) {
            return Err(ToolError::InvalidArgs(format!(
                "subcommand must be one of: {}",
                ALLOWED.join(", ")
            )));
        }

        let extra = args.get("args").and_then(Value::as_str).unwrap_or("");

        let mut command = std::process::Command::new("git");
        command.arg(subcommand).current_dir(root);

        match subcommand {
            "log" => {
                command.args(["--oneline", "-n", "20"]);
            }
            "diff" => {
                command.arg("--no-color");
            }
            "status" => {
                command.arg("--short");
            }
            _ => {}
        }

        for token in extra.split_whitespace() {
            command.arg(token);
        }

        let output = command
            .output()
            .map_err(|err| ToolError::Tool(format!("failed to run git: {err}")))?;

        let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
        combined.push_str(&String::from_utf8_lossy(&output.stderr));

        if combined.len() > MAX_OUTPUT {
            combined.truncate(MAX_OUTPUT);
            combined.push_str("\n...(output truncated)");
        }

        if combined.trim().is_empty() {
            combined = "(no output)".to_string();
        }

        Ok(combined)
    }
}

/// Stages and commits changes - the one git write operation KRIS gets as
/// a dedicated tool rather than routing through run_command, since a
/// multi-line or quote-containing commit message is fragile to pass
/// through `sh -c` (run_command still works for push/reset/etc., which
/// are rarer and don't have this quoting problem).
pub struct GitCommitTool {
    auto_approve: Cell<bool>,
}

impl GitCommitTool {
    pub fn new(auto_approve: bool) -> Self {
        Self {
            auto_approve: Cell::new(auto_approve),
        }
    }
}

impl Tool for GitCommitTool {
    fn name(&self) -> &'static str {
        "git_commit"
    }

    fn description(&self) -> &'static str {
        "Stages and commits changes to git with a message, after a y/N confirmation. Only \
         use when the user explicitly asks for a commit."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "message": { "type": "string", "description": "Commit message" },
                "paths": {
                    "type": "string",
                    "description": "Space-separated paths to stage (default: every change, like `git add -A`)"
                }
            },
            "required": ["message"]
        })
    }

    fn execute(&self, root: &Path, args: &Value) -> Result<String, ToolError> {
        let message = args
            .get("message")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::InvalidArgs("message".to_string()))?;
        let paths = args.get("paths").and_then(Value::as_str).unwrap_or("");

        let status = run_git(root, &["status", "--short"])?;
        if status.trim().is_empty() {
            return Ok("Nothing to commit (working tree clean).".to_string());
        }

        if !self.auto_approve.get() {
            AWAITING_CONFIRMATION.store(true, Ordering::SeqCst);

            print!("\r{}\r", " ".repeat(60));
            println!("\n┌─ KRIS wants to commit in {}:", root.display());
            println!("│  message: {message}");
            for line in status.lines() {
                println!("│  {line}");
            }
            print!("└─ Commit this? [y/N, or a = always for this session]: ");
            let _ = io::stdout().flush();

            let mut input = String::new();
            let read = io::stdin().read_line(&mut input);

            AWAITING_CONFIRMATION.store(false, Ordering::SeqCst);

            if read.is_err() {
                return Ok("Not committed (could not read confirmation).".to_string());
            }

            match input.trim().to_lowercase().as_str() {
                "y" | "yes" => {}
                "a" | "always" => self.auto_approve.set(true),
                _ => return Ok("Not committed (user declined).".to_string()),
            }
        }

        let mut add_args = vec!["add"];
        if paths.trim().is_empty() {
            add_args.push("-A");
        } else {
            add_args.extend(paths.split_whitespace());
        }
        run_git(root, &add_args)?;

        run_git(root, &["commit", "-m", message])
    }
}

fn run_git(root: &Path, args: &[&str]) -> Result<String, ToolError> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|err| ToolError::Tool(format!("failed to run git {}: {err}", args.join(" "))))?;

    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));

    if !output.status.success() {
        return Err(ToolError::Tool(format!(
            "git {} failed: {combined}",
            args.join(" ")
        )));
    }

    Ok(combined)
}
