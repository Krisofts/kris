use std::path::Path;

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
