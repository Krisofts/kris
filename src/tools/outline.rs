use std::fs;
use std::path::Path;

use regex::Regex;
use serde_json::{json, Value};

use super::{Tool, ToolError};

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

        let content = fs::read_to_string(root.join(path))?;

        let regex = Regex::new(
            r"^\s*(pub(\s*\([^)]*\))?\s+)?(export\s+(default\s+)?)?(async\s+)?(fn|struct|enum|trait|impl|class|interface|def|function)\s+\w",
        )
        .expect("static outline regex is valid");

        let lines: Vec<String> = content
            .lines()
            .enumerate()
            .filter(|(_, line)| regex.is_match(line))
            .map(|(i, line)| format!("{}: {}", i + 1, line.trim()))
            .collect();

        if lines.is_empty() {
            Ok(
                "No recognizable top-level definitions found (or unsupported language)."
                    .to_string(),
            )
        } else {
            Ok(lines.join("\n"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_rust_function_definitions() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("f.rs"),
            "use std::fmt;\n\npub fn hello() {}\n\nstruct Foo;\n",
        )
        .unwrap();

        let tool = OutlineFileTool;
        let out = tool
            .execute(dir.path(), &json!({ "path": "f.rs" }))
            .unwrap();

        assert!(out.contains("pub fn hello"));
        assert!(out.contains("struct Foo"));
        assert!(!out.contains("use std::fmt"));
    }
}
