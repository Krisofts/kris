use std::cell::Cell;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::Stdio;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use ignore::WalkBuilder;
use regex::Regex;
use serde_json::{json, Value};

use crate::error::ToolError;
use crate::file::{cat::cat, find::find, list::list_directory, tree::tree, write::write_file};

use super::Tool;

pub struct ReadFileTool;

impl Tool for ReadFileTool {
    fn name(&self) -> &'static str {
        "read_file"
    }

    fn description(&self) -> &'static str {
        "Read the contents of a file, given a path relative to the project root."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path relative to the project root" }
            },
            "required": ["path"]
        })
    }

    fn execute(&self, root: &Path, args: &Value) -> Result<String, ToolError> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::InvalidArgs("path".to_string()))?;

        cat(root.join(path))
    }
}

pub struct ListDirectoryTool;

impl Tool for ListDirectoryTool {
    fn name(&self) -> &'static str {
        "list_directory"
    }

    fn description(&self) -> &'static str {
        "List files and folders inside a directory relative to the project root."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Directory path relative to the project root, use \".\" for the root" }
            },
            "required": []
        })
    }

    fn execute(&self, root: &Path, args: &Value) -> Result<String, ToolError> {
        let path = args.get("path").and_then(Value::as_str).unwrap_or(".");

        let entries = list_directory(root.join(path))?;

        Ok(entries.join("\n"))
    }
}

pub struct FindFilesTool;

impl Tool for FindFilesTool {
    fn name(&self) -> &'static str {
        "find_files"
    }

    fn description(&self) -> &'static str {
        "Recursively search the project for files whose name contains a keyword."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "keyword": { "type": "string", "description": "Substring to search for in file names" }
            },
            "required": ["keyword"]
        })
    }

    fn execute(&self, root: &Path, args: &Value) -> Result<String, ToolError> {
        let keyword = args
            .get("keyword")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::InvalidArgs("keyword".to_string()))?;

        let results = find(root, keyword).map_err(|err| ToolError::Tool(err.to_string()))?;

        if results.is_empty() {
            Ok("No files found.".to_string())
        } else {
            Ok(results.join("\n"))
        }
    }
}

pub struct TreeTool;

impl Tool for TreeTool {
    fn name(&self) -> &'static str {
        "tree"
    }

    fn description(&self) -> &'static str {
        "Show the directory tree of the whole project."
    }

    fn parameters_schema(&self) -> Value {
        json!({ "type": "object", "properties": {}, "required": [] })
    }

    fn execute(&self, root: &Path, _args: &Value) -> Result<String, ToolError> {
        let lines = tree(root).map_err(|err| ToolError::Tool(err.to_string()))?;

        Ok(lines.join("\n"))
    }
}

pub struct WriteFileTool;

impl Tool for WriteFileTool {
    fn name(&self) -> &'static str {
        "write_file"
    }

    fn description(&self) -> &'static str {
        "Create a file, or overwrite it if it already exists, with the given content. Path is relative to the project root."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path relative to the project root" },
                "content": { "type": "string", "description": "Full contents to write to the file" }
            },
            "required": ["path", "content"]
        })
    }

    fn execute(&self, root: &Path, args: &Value) -> Result<String, ToolError> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::InvalidArgs("path".to_string()))?;

        let content = args
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::InvalidArgs("content".to_string()))?;

        write_file(root.join(path), content)?;

        Ok(format!("Wrote {} bytes to {path}", content.len()))
    }
}

pub struct EditFileTool;

impl Tool for EditFileTool {
    fn name(&self) -> &'static str {
        "edit_file"
    }

    fn description(&self) -> &'static str {
        "Replace an exact occurrence of old_string with new_string in an existing file. \
         Prefer this over write_file for small changes to existing files. old_string must \
         match exactly once, unless replace_all is true."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path relative to the project root" },
                "old_string": { "type": "string", "description": "Exact text to find, including whitespace" },
                "new_string": { "type": "string", "description": "Text to replace it with" },
                "replace_all": { "type": "boolean", "description": "Replace every occurrence instead of requiring exactly one match (default false)" }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }

    fn execute(&self, root: &Path, args: &Value) -> Result<String, ToolError> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::InvalidArgs("path".to_string()))?;

        let old_string = args
            .get("old_string")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::InvalidArgs("old_string".to_string()))?;

        let new_string = args
            .get("new_string")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::InvalidArgs("new_string".to_string()))?;

        let replace_all = args
            .get("replace_all")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let full_path = root.join(path);
        let content = cat(&full_path)?;

        let occurrences = content.matches(old_string).count();

        if occurrences == 0 {
            return Err(ToolError::Tool(format!("old_string not found in {path}")));
        }

        if occurrences > 1 && !replace_all {
            return Err(ToolError::Tool(format!(
                "old_string matches {occurrences} times in {path}; make it unique or set replace_all: true"
            )));
        }

        let updated = if replace_all {
            content.replace(old_string, new_string)
        } else {
            content.replacen(old_string, new_string, 1)
        };

        write_file(&full_path, &updated)?;

        let count = if replace_all { occurrences } else { 1 };

        Ok(format!("Replaced {count} occurrence(s) in {path}"))
    }
}

pub struct SearchCodeTool;

impl Tool for SearchCodeTool {
    fn name(&self) -> &'static str {
        "search_code"
    }

    fn description(&self) -> &'static str {
        "Search file contents for a regex pattern across the project (like grep -rn), \
         skipping .git and anything ignored by .gitignore. Returns matching \
         path:line: text, capped to the first 200 matches."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Regex pattern to search for" }
            },
            "required": ["pattern"]
        })
    }

    fn execute(&self, root: &Path, args: &Value) -> Result<String, ToolError> {
        const MAX_MATCHES: usize = 200;

        let pattern = args
            .get("pattern")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::InvalidArgs("pattern".to_string()))?;

        let regex =
            Regex::new(pattern).map_err(|err| ToolError::Tool(format!("invalid regex: {err}")))?;

        let mut matches = Vec::new();

        'walk: for entry in WalkBuilder::new(root).build().filter_map(Result::ok) {
            let path = entry.path();

            if !path.is_file() {
                continue;
            }

            let Ok(content) = std::fs::read_to_string(path) else {
                continue;
            };

            for (i, line) in content.lines().enumerate() {
                if regex.is_match(line) {
                    let rel = path.strip_prefix(root).unwrap_or(path);
                    matches.push(format!("{}:{}: {}", rel.display(), i + 1, line.trim()));

                    if matches.len() >= MAX_MATCHES {
                        break 'walk;
                    }
                }
            }
        }

        if matches.is_empty() {
            Ok("No matches found.".to_string())
        } else {
            Ok(matches.join("\n"))
        }
    }
}

pub struct RunCommandTool {
    auto_approve: Cell<bool>,
}

impl Default for RunCommandTool {
    fn default() -> Self {
        Self::new()
    }
}

impl RunCommandTool {
    pub fn new() -> Self {
        Self {
            auto_approve: Cell::new(false),
        }
    }
}

impl Tool for RunCommandTool {
    fn name(&self) -> &'static str {
        "run_command"
    }

    fn description(&self) -> &'static str {
        "Run a shell command inside the project root (e.g. `cargo build`, `cargo test`, \
         `npm install`). Asks the user for a y/n confirmation before executing anything. \
         Output is captured and truncated if long. Killed after 2 minutes if it hasn't \
         finished - for a process that should keep running (a dev server), background \
         it yourself instead, e.g. `tmux new-session -d -s preview 'npm run dev'`, which \
         returns immediately."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "The shell command to run, e.g. \"cargo test\"" }
            },
            "required": ["command"]
        })
    }

    fn execute(&self, root: &Path, args: &Value) -> Result<String, ToolError> {
        const MAX_OUTPUT: usize = 4000;

        let command = args
            .get("command")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::InvalidArgs("command".to_string()))?;

        if !self.auto_approve.get() {
            // Wipe any leftover spinner text so the prompt below isn't garbled.
            print!("\r{}\r", " ".repeat(60));
            println!("\n┌─ KRIS wants to run a command in {}:", root.display());
            println!("│  {command}");
            print!("└─ Run it? [y/N, or a = always for this session]: ");
            let _ = io::stdout().flush();

            let mut input = String::new();
            if io::stdin().read_line(&mut input).is_err() {
                return Ok("Command not executed (could not read confirmation).".to_string());
            }

            match input.trim().to_lowercase().as_str() {
                "y" | "yes" => {}
                "a" | "always" => self.auto_approve.set(true),
                _ => return Ok("Command not executed (user declined).".to_string()),
            }
        }

        const TIMEOUT: Duration = Duration::from_secs(120);

        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| ToolError::Tool(format!("failed to run command: {err}")))?;

        // Drain stdout/stderr on their own threads while polling try_wait()
        // below - otherwise a command with enough output to fill the pipe
        // buffer would deadlock (it blocks writing, we'd block waiting).
        let stdout_rx = spawn_reader(child.stdout.take());
        let stderr_rx = spawn_reader(child.stderr.take());

        let start = Instant::now();
        let mut timed_out = false;

        let status_code = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status.code(),
                Ok(None) => {
                    if start.elapsed() > TIMEOUT {
                        let _ = child.kill();
                        let _ = child.wait();
                        timed_out = true;
                        break None;
                    }
                    thread::sleep(Duration::from_millis(150));
                }
                Err(_) => break None,
            }
        };

        let mut combined = stdout_rx.recv().unwrap_or_default();
        combined.push_str(&stderr_rx.recv().unwrap_or_default());

        if combined.len() > MAX_OUTPUT {
            combined.truncate(MAX_OUTPUT);
            combined.push_str("\n...(output truncated)");
        }

        if timed_out {
            combined.push_str("\n(killed after 120s timeout)");
        }

        let status = status_code
            .map(|code| code.to_string())
            .unwrap_or_else(|| "unknown".to_string());

        Ok(format!("exit code: {status}\n{combined}"))
    }
}

fn spawn_reader<R>(pipe: Option<R>) -> mpsc::Receiver<String>
where
    R: Read + Send + 'static,
{
    let (tx, rx) = mpsc::channel();

    thread::spawn(move || {
        let mut buf = Vec::new();

        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut buf);
        }

        let _ = tx.send(String::from_utf8_lossy(&buf).into_owned());
    });

    rx
}

pub struct CreateDirectoryTool;

impl Tool for CreateDirectoryTool {
    fn name(&self) -> &'static str {
        "create_directory"
    }

    fn description(&self) -> &'static str {
        "Create a directory, including any missing parent directories, given a path \
         relative to the project root."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Directory path relative to the project root" }
            },
            "required": ["path"]
        })
    }

    fn execute(&self, root: &Path, args: &Value) -> Result<String, ToolError> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::InvalidArgs("path".to_string()))?;

        std::fs::create_dir_all(root.join(path))?;

        Ok(format!("Created directory {path}"))
    }
}

pub struct DeleteFileTool;

impl Tool for DeleteFileTool {
    fn name(&self) -> &'static str {
        "delete_file"
    }

    fn description(&self) -> &'static str {
        "Delete a single file (not a directory) inside the project, given a path relative \
         to the project root."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path relative to the project root" }
            },
            "required": ["path"]
        })
    }

    fn execute(&self, root: &Path, args: &Value) -> Result<String, ToolError> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::InvalidArgs("path".to_string()))?;

        let full_path = root.join(path);

        if !full_path.is_file() {
            return Err(ToolError::Tool(format!(
                "{path} is not a file (or doesn't exist)"
            )));
        }

        std::fs::remove_file(&full_path)?;

        Ok(format!("Deleted {path}"))
    }
}

pub struct DeleteDirectoryTool;

impl Tool for DeleteDirectoryTool {
    fn name(&self) -> &'static str {
        "delete_directory"
    }

    fn description(&self) -> &'static str {
        "Delete a directory and everything inside it, given a path relative to the project \
         root. Refuses to delete the project root itself."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Directory path relative to the project root" }
            },
            "required": ["path"]
        })
    }

    fn execute(&self, root: &Path, args: &Value) -> Result<String, ToolError> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::InvalidArgs("path".to_string()))?;

        let full_path = root.join(path);

        if !full_path.is_dir() {
            return Err(ToolError::Tool(format!(
                "{path} is not a directory (or doesn't exist)"
            )));
        }

        let canonical_root = root
            .canonicalize()
            .map_err(|err| ToolError::Tool(format!("failed to resolve project root: {err}")))?;
        let canonical_target = full_path
            .canonicalize()
            .map_err(|err| ToolError::Tool(format!("failed to resolve {path}: {err}")))?;

        if canonical_target == canonical_root {
            return Err(ToolError::Tool(
                "Refusing to delete the project root itself".to_string(),
            ));
        }

        std::fs::remove_dir_all(&full_path)?;

        Ok(format!("Deleted directory {path}"))
    }
}

pub struct MoveFileTool;

impl Tool for MoveFileTool {
    fn name(&self) -> &'static str {
        "move_file"
    }

    fn description(&self) -> &'static str {
        "Move or rename a file or directory inside the project. Both paths are relative \
         to the project root."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "from": { "type": "string", "description": "Current path, relative to the project root" },
                "to": { "type": "string", "description": "New path, relative to the project root" }
            },
            "required": ["from", "to"]
        })
    }

    fn execute(&self, root: &Path, args: &Value) -> Result<String, ToolError> {
        let from = args
            .get("from")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::InvalidArgs("from".to_string()))?;

        let to = args
            .get("to")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::InvalidArgs("to".to_string()))?;

        let from_path = root.join(from);
        let to_path = root.join(to);

        if !from_path.exists() {
            return Err(ToolError::Tool(format!("{from} does not exist")));
        }

        if let Some(parent) = to_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        std::fs::rename(&from_path, &to_path)?;

        Ok(format!("Moved {from} to {to}"))
    }
}

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
        const MAX_OUTPUT: usize = 4000;
        const ALLOWED: [&str; 5] = ["status", "diff", "log", "show", "branch"];

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

        // Naive whitespace split: fine for the simple path/hash/branch-name
        // arguments this tool is meant for.
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

pub struct OutlineFileTool;

impl Tool for OutlineFileTool {
    fn name(&self) -> &'static str {
        "outline_file"
    }

    fn description(&self) -> &'static str {
        "Show a quick outline of a file's top-level functions/classes/structs/types (by \
         simple pattern matching, not full parsing) without reading the whole file. \
         Useful for orienting in a large file before deciding what to read_file or \
         edit_file."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path relative to the project root" }
            },
            "required": ["path"]
        })
    }

    fn execute(&self, root: &Path, args: &Value) -> Result<String, ToolError> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::InvalidArgs("path".to_string()))?;

        let content = cat(root.join(path))?;

        let regex = Regex::new(
            r"^\s*(pub(\s*\([^)]*\))?\s+)?(export\s+(default\s+)?)?(async\s+)?(fn|struct|enum|trait|impl|class|interface|def|function)\s+\w",
        )
        .map_err(|err| ToolError::Tool(format!("invalid outline regex: {err}")))?;

        let lines: Vec<String> = content
            .lines()
            .enumerate()
            .filter(|(_, line)| regex.is_match(line))
            .map(|(i, line)| format!("{}: {}", i + 1, line.trim()))
            .collect();

        if lines.is_empty() {
            Ok("No recognizable top-level definitions found (or unsupported language)."
                .to_string())
        } else {
            Ok(lines.join("\n"))
        }
    }
}
