use std::path::Path;

use anyhow::Result;
use serde_json::Value;

use kris_tools::tool::ToolRegistry;

use crate::client::LlamaClient;
use crate::message::Message;

pub struct Agent {
    client: LlamaClient,
    tools: ToolRegistry,
    temperature: f32,
    max_tokens: u32,
    max_iterations: u32,
}

impl Agent {
    pub fn new(
        client: LlamaClient,
        tools: ToolRegistry,
        temperature: f32,
        max_tokens: u32,
        max_iterations: u32,
    ) -> Self {
        Self {
            client,
            tools,
            temperature,
            max_tokens,
            max_iterations,
        }
    }

    fn system_prompt(&self, project_name: &str) -> String {
        let tools_json = serde_json::to_string_pretty(&self.tools.describe_all())
            .unwrap_or_else(|_| "[]".to_string());

        format!(
            "You are KRIS, an offline coding assistant running locally in a terminal, \
             currently working inside the project \"{project_name}\".\n\n\
             You have access to the following tools:\n{tools_json}\n\n\
             To call a tool, respond with ONLY a single JSON object of the form:\n\
             {{\"tool\": \"<tool_name>\", \"args\": {{...}}}}\n\
             Do not add any text before or after the JSON object when calling a tool.\n\n\
             Once you have enough information to answer the user, respond with plain \
             text (no JSON) containing your final answer. Keep answers focused and \
             include code blocks when showing code."
        )
    }

    /// Runs one user turn to completion, calling tools as needed, and returns
    /// the assistant's final natural-language answer.
    pub async fn run(
        &self,
        history: &mut Vec<Message>,
        root: &Path,
        project_name: &str,
        user_input: &str,
        mut on_tool_call: impl FnMut(&str, &Value),
    ) -> Result<String> {
        if history.is_empty() {
            history.push(Message::system(self.system_prompt(project_name)));
        }

        history.push(Message::user(user_input));

        for _ in 0..self.max_iterations {
            let response = self
                .client
                .chat(history, self.temperature, self.max_tokens)
                .await?;

            history.push(Message::assistant(response.clone()));

            match parse_tool_call(&response) {
                Some((tool_name, args)) => {
                    on_tool_call(&tool_name, &args);

                    let result = self
                        .tools
                        .execute(&tool_name, root, &args)
                        .unwrap_or_else(|err| format!("Error: {err}"));

                    history.push(Message::user(format!(
                        "[tool_result: {tool_name}]\n{result}"
                    )));
                }
                None => return Ok(response),
            }
        }

        Ok("Reached the maximum number of tool calls without a final answer.".to_string())
    }
}

fn parse_tool_call(text: &str) -> Option<(String, Value)> {
    let trimmed = text.trim();
    let trimmed = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed)
        .trim();
    let trimmed = trimmed.strip_suffix("```").unwrap_or(trimmed).trim();

    let value: Value = serde_json::from_str(trimmed).ok()?;
    let tool = value.get("tool")?.as_str()?.to_string();
    let args = value.get("args").cloned().unwrap_or_else(|| serde_json::json!({}));

    Some((tool, args))
}
