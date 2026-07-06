use std::io::{self, Write};
use std::path::Path;

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

pub struct RunCommandTool;

impl Tool for RunCommandTool {
    fn name(&self) -> &'static str {
        "run_command"
    }

    fn description(&self) -> &'static str {
        "Run a shell command inside the project root (e.g. `cargo build`, `cargo test`, \
         `npm install`). Asks the user for a y/n confirmation before executing anything. \
         Output is captured and truncated if long."
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

        // Wipe any leftover spinner text so the prompt below isn't garbled.
        print!("\r{}\r", " ".repeat(60));
        println!("\n┌─ KRIS wants to run a command in {}:", root.display());
        println!("│  {command}");
        print!("└─ Run it? [y/N]: ");
        let _ = io::stdout().flush();

        let mut input = String::new();
        if io::stdin().read_line(&mut input).is_err() {
            return Ok("Command not executed (could not read confirmation).".to_string());
        }

        if !matches!(input.trim().to_lowercase().as_str(), "y" | "yes") {
            return Ok("Command not executed (user declined).".to_string());
        }

        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(root)
            .output()
            .map_err(|err| ToolError::Tool(format!("failed to run command: {err}")))?;

        let mut combined = String::new();
        combined.push_str(&String::from_utf8_lossy(&output.stdout));
        combined.push_str(&String::from_utf8_lossy(&output.stderr));

        if combined.len() > MAX_OUTPUT {
            combined.truncate(MAX_OUTPUT);
            combined.push_str("\n...(output truncated)");
        }

        let status = output
            .status
            .code()
            .map(|code| code.to_string())
            .unwrap_or_else(|| "unknown".to_string());

        Ok(format!("exit code: {status}\n{combined}"))
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
