mod edit;
mod fs;
mod git;
mod outline;
mod run_command;

use std::cell::Cell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

use serde_json::{json, Value};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Unknown tool: {0}")]
    UnknownTool(String),
    #[error("Invalid or missing argument: {0}")]
    InvalidArgs(String),
    #[error("{0}")]
    Tool(String),
}

pub trait Tool {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn parameters_schema(&self) -> Value;
    fn execute(&self, root: &Path, args: &Value) -> Result<String, ToolError>;
}

pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::with_defaults()
    }
}

impl ToolRegistry {
    pub fn with_defaults() -> Self {
        let mut registry = Self {
            tools: HashMap::new(),
        };

        // Shared so approving one filesystem change with "always" covers
        // writes, edits, deletes, and moves for the rest of the session,
        // rather than needing a separate "always" per tool.
        let auto_approve = Rc::new(Cell::new(false));

        registry.register(fs::ReadFileTool);
        registry.register(fs::ListDirectoryTool);
        registry.register(fs::TreeTool);
        registry.register(fs::FindFilesTool);
        registry.register(fs::SearchCodeTool);
        registry.register(edit::WriteFileTool::new(auto_approve.clone()));
        registry.register(edit::EditFileTool::new(auto_approve.clone()));
        registry.register(edit::DeleteFileTool::new(auto_approve.clone()));
        registry.register(edit::DeleteDirectoryTool::new(auto_approve.clone()));
        registry.register(edit::MoveFileTool::new(auto_approve.clone()));
        registry.register(edit::CreateDirectoryTool::new(auto_approve));
        registry.register(run_command::RunCommandTool::new());
        registry.register(git::GitTool);
        registry.register(outline::OutlineFileTool);

        registry
    }

    fn register<T>(&mut self, tool: T)
    where
        T: Tool + 'static,
    {
        self.tools.insert(tool.name().to_string(), Box::new(tool));
    }

    pub fn names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.tools.keys().map(String::as_str).collect();
        names.sort_unstable();
        names
    }

    /// OpenAI `tools` array shape (`{"type":"function","function":{...}}`),
    /// what llama-server's `--jinja` template rendering expects so the
    /// model gets grammar-constrained structured tool calls instead of
    /// having to be told the schema in the system prompt as prose.
    pub fn describe_all(&self) -> Vec<Value> {
        let mut names = self.names();
        names.sort_unstable();

        names
            .into_iter()
            .map(|name| {
                let tool = &self.tools[name];
                json!({
                    "type": "function",
                    "function": {
                        "name": tool.name(),
                        "description": tool.description(),
                        "parameters": tool.parameters_schema(),
                    }
                })
            })
            .collect()
    }

    pub fn execute(&self, name: &str, root: &Path, args: &Value) -> Result<String, ToolError> {
        match self.tools.get(name) {
            Some(tool) => tool.execute(root, args),
            None => Err(ToolError::UnknownTool(name.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_all_uses_openai_function_shape() {
        let registry = ToolRegistry::with_defaults();
        let described = registry.describe_all();

        assert!(!described.is_empty());
        for entry in &described {
            assert_eq!(entry["type"], "function");
            assert!(entry["function"]["name"].is_string());
            assert!(entry["function"]["parameters"].is_object());
        }
    }

    #[test]
    fn execute_reports_unknown_tool() {
        let registry = ToolRegistry::with_defaults();
        let err = registry
            .execute("does_not_exist", Path::new("."), &json!({}))
            .unwrap_err();

        assert!(matches!(err, ToolError::UnknownTool(_)));
    }
}
