use std::collections::HashMap;
use std::path::Path;

use serde_json::{json, Value};

use crate::error::ToolError;

use super::builtin::{
    DeleteFileTool, EditFileTool, FindFilesTool, ListDirectoryTool, MoveFileTool, ReadFileTool,
    RunCommandTool, SearchCodeTool, TreeTool, WriteFileTool,
};
use super::Tool;

pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    pub fn with_defaults() -> Self {
        let mut registry = Self::new();

        registry.register(ReadFileTool);
        registry.register(ListDirectoryTool);
        registry.register(FindFilesTool);
        registry.register(TreeTool);
        registry.register(WriteFileTool);
        registry.register(EditFileTool);
        registry.register(SearchCodeTool);
        registry.register(RunCommandTool::new());
        registry.register(DeleteFileTool);
        registry.register(MoveFileTool);

        registry
    }

    pub fn register<T>(&mut self, tool: T)
    where
        T: Tool + 'static,
    {
        self.tools.insert(tool.name().to_string(), Box::new(tool));
    }

    pub fn list(&self) -> Vec<(&str, &str)> {
        let mut tools = self
            .tools
            .values()
            .map(|tool| (tool.name(), tool.description()))
            .collect::<Vec<_>>();

        tools.sort_by(|a, b| a.0.cmp(b.0));

        tools
    }

    pub fn describe_all(&self) -> Vec<Value> {
        let mut tools = self
            .tools
            .values()
            .map(|tool| {
                json!({
                    "name": tool.name(),
                    "description": tool.description(),
                    "parameters": tool.parameters_schema(),
                })
            })
            .collect::<Vec<_>>();

        tools.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));

        tools
    }

    pub fn execute(&self, name: &str, root: &Path, args: &Value) -> Result<String, ToolError> {
        match self.tools.get(name) {
            Some(tool) => tool.execute(root, args),
            None => Err(ToolError::UnknownTool(name.to_string())),
        }
    }
}
