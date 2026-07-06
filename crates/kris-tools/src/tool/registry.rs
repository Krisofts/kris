use std::collections::HashMap;

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
}
