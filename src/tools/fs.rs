use std::fs;
use std::path::Path;

use ignore::WalkBuilder;
use regex::Regex;
use serde_json::{json, Value};

use super::{Tool, ToolError};

/// Above this many lines, `read_file` without an explicit range truncates
/// and tells the model to use `offset`/`limit` or `outline_file` instead -
/// on an 8k-context phone model, one big source file can otherwise eat the
/// entire budget in a single tool call.
const SOFT_LINE_CAP: usize = 500;

pub struct ReadFileTool;

impl Tool for ReadFileTool {
    fn name(&self) -> &'static str {
        "read_file"
    }

    fn description(&self) -> &'static str {
        "Read a file's contents, given a path relative to the project root. Files over \
         500 lines are truncated by default - pass offset/limit (1-indexed line numbers) \
         to read a specific range, or use outline_file first to find the range you need."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path relative to the project root" },
                "offset": { "type": "integer", "description": "1-indexed line number to start from (default 1)" },
                "limit": { "type": "integer", "description": "Maximum number of lines to return" }
            },
            "required": ["path"]
        })
    }

    fn execute(&self, root: &Path, args: &Value) -> Result<String, ToolError> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::InvalidArgs("path".to_string()))?;

        let offset = args
            .get("offset")
            .and_then(Value::as_u64)
            .unwrap_or(1)
            .max(1) as usize;
        let explicit_limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .map(|v| v as usize);

        let content = fs::read_to_string(root.join(path))?;
        let total_lines = content.lines().count();

        let limit = explicit_limit.unwrap_or(SOFT_LINE_CAP);
        let selected: Vec<&str> = content.lines().skip(offset - 1).take(limit).collect();

        let shown_end = offset - 1 + selected.len();
        let mut out = selected.join("\n");

        if shown_end < total_lines {
            out.push_str(&format!(
                "\n... ({total_lines} lines total, showing {offset}-{shown_end}; pass offset/limit for more)"
            ));
        }

        Ok(out)
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

        let mut entries = Vec::new();
        for entry in fs::read_dir(root.join(path))? {
            entries.push(entry?.file_name().to_string_lossy().to_string());
        }
        entries.sort();

        Ok(entries.join("\n"))
    }
}

pub struct TreeTool;

impl Tool for TreeTool {
    fn name(&self) -> &'static str {
        "tree"
    }

    fn description(&self) -> &'static str {
        "Show the project's directory tree, skipping .git and anything ignored by \
         .gitignore (so build output, node_modules, target/, etc. don't clutter it)."
    }

    fn parameters_schema(&self) -> Value {
        json!({ "type": "object", "properties": {}, "required": [] })
    }

    fn execute(&self, root: &Path, _args: &Value) -> Result<String, ToolError> {
        let mut lines = vec![".".to_string()];

        // `.hidden(false)` deliberately surfaces other dotfiles/dotdirs
        // that are actually useful to see (.github/, .eslintrc, ...), but
        // that also un-hides `.git` itself, which isn't covered by
        // .gitignore rules (a repo's own .gitignore doesn't need to
        // mention .git) - without `filter_entry` explicitly skipping it,
        // this walked straight into .git's internal object database,
        // confirmed on-device to explode a single `tree` call into
        // hundreds of thousands of tokens on an ordinary repo and blow
        // straight through the context budget on the very first turn.
        let mut entries: Vec<_> = WalkBuilder::new(root)
            .hidden(false)
            .filter_entry(|entry| entry.file_name() != ".git")
            .min_depth(Some(1))
            .build()
            .filter_map(Result::ok)
            .collect();
        entries.sort_by(|a, b| a.path().cmp(b.path()));

        for entry in entries {
            let depth = entry.depth();
            let indent = "│   ".repeat(depth.saturating_sub(1));
            let name = entry.file_name().to_string_lossy();
            lines.push(format!("{indent}├── {name}"));
        }

        Ok(lines.join("\n"))
    }
}

pub struct FindFilesTool;

impl Tool for FindFilesTool {
    fn name(&self) -> &'static str {
        "find_files"
    }

    fn description(&self) -> &'static str {
        "Recursively search the project for files whose name contains a keyword, \
         skipping .git and anything ignored by .gitignore."
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

        let mut results = Vec::new();
        for entry in WalkBuilder::new(root).build().filter_map(Result::ok) {
            let path = entry.path();
            if let Some(name) = path.file_name() {
                if name.to_string_lossy().contains(keyword) {
                    let rel = path.strip_prefix(root).unwrap_or(path);
                    results.push(rel.display().to_string());
                }
            }
        }
        results.sort();

        if results.is_empty() {
            Ok("No files found.".to_string())
        } else {
            Ok(results.join("\n"))
        }
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

            let Ok(content) = fs::read_to_string(path) else {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn tree_shows_other_dotfiles_but_never_descends_into_git() {
        // Regression test: TreeTool passes `.hidden(false)` to show useful
        // dotfiles like `.github/`, but a repo's own `.gitignore` never
        // mentions `.git` itself, so without an explicit filter this also
        // walked straight into .git's internal object database - on-device,
        // that exploded a single `tree` call into hundreds of thousands of
        // tokens on an ordinary repo, blowing through the context budget
        // on the very first turn of a session.
        let dir = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir.path())
            .status()
            .unwrap();
        fs::write(dir.path().join("a.rs"), "fn main() {}\n").unwrap();
        fs::write(dir.path().join(".env"), "SECRET=1\n").unwrap();
        std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(dir.path())
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args([
                "-c",
                "user.email=a@a.com",
                "-c",
                "user.name=a",
                "commit",
                "-q",
                "-m",
                "init",
            ])
            .current_dir(dir.path())
            .status()
            .unwrap();

        let tool = TreeTool;
        let out = tool.execute(dir.path(), &json!({})).unwrap();

        assert!(out.contains("a.rs"));
        assert!(out.contains(".env"), "other dotfiles should still show up");
        assert!(!out.contains(".git"), "must never descend into .git");
    }

    #[test]
    fn read_file_truncates_and_reports_range() {
        let dir = tempfile::tempdir().unwrap();
        let content: String = (1..=1000).map(|i| format!("line{i}\n")).collect();
        fs::write(dir.path().join("big.txt"), content).unwrap();

        let tool = ReadFileTool;
        let out = tool
            .execute(dir.path(), &json!({ "path": "big.txt" }))
            .unwrap();

        assert!(out.contains("line1\n"));
        assert!(out.contains("1000 lines total"));
        assert!(!out.contains("line501"));
    }

    #[test]
    fn read_file_respects_offset_and_limit() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("f.txt"), "a\nb\nc\nd\n").unwrap();

        let tool = ReadFileTool;
        let out = tool
            .execute(
                dir.path(),
                &json!({ "path": "f.txt", "offset": 2, "limit": 2 }),
            )
            .unwrap();

        assert!(out.starts_with("b\nc"));
        assert!(out.contains("4 lines total, showing 2-3"));
    }

    #[test]
    fn search_code_finds_pattern() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.rs"), "fn main() {}\n").unwrap();

        let tool = SearchCodeTool;
        let out = tool
            .execute(dir.path(), &json!({ "pattern": "fn main" }))
            .unwrap();

        assert!(out.contains("a.rs:1"));
    }
}
