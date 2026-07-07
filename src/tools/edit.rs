use std::fs;
use std::path::Path;

use serde_json::{json, Value};

use crate::diff::render_unified_diff;
use crate::style::cyan;

use super::{Tool, ToolError};

pub struct WriteFileTool;

impl Tool for WriteFileTool {
    fn name(&self) -> &'static str {
        "write_file"
    }

    fn description(&self) -> &'static str {
        "Create a file, or overwrite it if it already exists, with the given content. \
         Path is relative to the project root. Prefer edit_file for small changes to an \
         existing file - this always replaces the whole file."
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

        let full_path = root.join(path);
        let old = fs::read_to_string(&full_path).unwrap_or_default();

        println!("{}", render_unified_diff(path, &old, content));

        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&full_path, content)?;

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
        let content = fs::read_to_string(&full_path)?;

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

        println!("{}", render_unified_diff(path, &content, &updated));

        fs::write(&full_path, &updated)?;

        let count = if replace_all { occurrences } else { 1 };
        Ok(format!("Replaced {count} occurrence(s) in {path}"))
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

        let old = fs::read_to_string(&full_path).unwrap_or_default();
        println!("{}", render_unified_diff(path, &old, ""));

        fs::remove_file(&full_path)?;

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

        let mut removed: Vec<String> = walk_files(&full_path)
            .into_iter()
            .map(|p| p.strip_prefix(root).unwrap_or(&p).display().to_string())
            .collect();
        removed.sort();

        println!("{}", cyan(&format!("Deleting directory {path}:")));
        for entry in &removed {
            println!("  - {entry}");
        }

        fs::remove_dir_all(&full_path)?;

        Ok(format!(
            "Deleted directory {path} ({} file(s))",
            removed.len()
        ))
    }
}

fn walk_files(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(walk_files(&path));
            } else {
                files.push(path);
            }
        }
    }
    files
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

        println!("{}", cyan(&format!("Moving {from} -> {to}")));

        if let Some(parent) = to_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(&from_path, &to_path)?;

        Ok(format!("Moved {from} to {to}"))
    }
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

        fs::create_dir_all(root.join(path))?;

        Ok(format!("Created directory {path}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_file_requires_unique_match() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("f.txt"), "x\nx\n").unwrap();

        let tool = EditFileTool;
        let err = tool
            .execute(
                dir.path(),
                &json!({ "path": "f.txt", "old_string": "x", "new_string": "y" }),
            )
            .unwrap_err();

        assert!(matches!(err, ToolError::Tool(_)));
    }

    #[test]
    fn write_file_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();

        let tool = WriteFileTool;
        tool.execute(
            dir.path(),
            &json!({ "path": "nested/dir/f.txt", "content": "hi" }),
        )
        .unwrap();

        assert_eq!(
            fs::read_to_string(dir.path().join("nested/dir/f.txt")).unwrap(),
            "hi"
        );
    }

    #[test]
    fn delete_directory_refuses_project_root() {
        let dir = tempfile::tempdir().unwrap();

        let tool = DeleteDirectoryTool;
        let err = tool
            .execute(dir.path(), &json!({ "path": "." }))
            .unwrap_err();

        assert!(matches!(err, ToolError::Tool(_)));
    }
}
