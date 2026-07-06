use std::path::Path;

use anyhow::Result;
use serde_json::Value;

use kris_tools::error::ToolError;
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

    fn system_prompt(&self, project_name: &str, project_type_hint: &str) -> String {
        // Compact (not pretty-printed) to keep the prompt short - every extra
        // token here is reprocessed on every turn, which matters on
        // CPU-only, phone-class hardware.
        let tools_json =
            serde_json::to_string(&self.tools.describe_all()).unwrap_or_else(|_| "[]".to_string());

        let type_line = if project_type_hint.is_empty() {
            String::new()
        } else {
            format!("{project_type_hint}\n")
        };

        format!(
            "You are KRIS, an offline coding assistant running locally in a terminal, \
             currently working inside the project \"{project_name}\".\n\
             {type_line}\n\
             You have access to the following tools:\n{tools_json}\n\n\
             To call a tool, respond with ONLY a single, valid JSON object of the form:\n\
             {{\"tool\": \"<tool_name>\", \"args\": {{...}}}}\n\
             Send exactly one tool call at a time - never more than one JSON object in a \
             single reply. Properly escape any double quotes and newlines inside string \
             values (\\\" and \\n), so the JSON stays valid. Do not add any text before or \
             after the JSON object when calling a tool. Prefer edit_file over write_file \
             when changing part of a file that already exists, and use run_command to \
             build or test code after changing it.\n\n\
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
        project_type_hint: &str,
        user_input: &str,
        mut on_tool_call: impl FnMut(&str, &Value, &str),
    ) -> Result<String> {
        if history.is_empty() {
            history.push(Message::system(
                self.system_prompt(project_name, project_type_hint),
            ));
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
                    let result = match self.tools.execute(&tool_name, root, &args) {
                        Ok(output) => output,
                        Err(ToolError::UnknownTool(name)) => {
                            let available = self
                                .tools
                                .list()
                                .iter()
                                .map(|(name, _)| *name)
                                .collect::<Vec<_>>()
                                .join(", ");

                            format!(
                                "Error: there is no tool called \"{name}\". Available \
                                 tools: {available}. Use exactly one of these names."
                            )
                        }
                        Err(err) => format!("Error: {err}"),
                    };

                    on_tool_call(&tool_name, &args, &result);

                    history.push(Message::user(format!(
                        "[tool_result: {tool_name}]\n{result}"
                    )));
                }
                None if looks_like_failed_tool_call(&response) => {
                    history.push(Message::user(
                        "That was not a single valid JSON tool call (check for unescaped \
                         quotes/newlines inside string values, and send only one JSON \
                         object). Retry with a valid tool call, or answer in plain text \
                         if you don't need a tool.",
                    ));
                }
                None => return Ok(response),
            }
        }

        Ok("Reached the maximum number of tool calls without a final answer.".to_string())
    }
}

/// A response is treated as final text unless it parses as a tool call *or*
/// it clearly looks like a botched attempt at one (so we ask the model to
/// retry instead of showing broken JSON to the user as if it were an answer).
fn looks_like_failed_tool_call(text: &str) -> bool {
    let trimmed = text.trim();

    trimmed.starts_with('{') || trimmed.starts_with("```json") || trimmed.contains("\"tool\"")
}

fn parse_tool_call(text: &str) -> Option<(String, Value)> {
    let json_str = extract_first_json_object(text)?;

    let value: Value = serde_json::from_str(&json_str).ok()?;
    let tool = value.get("tool")?.as_str()?.to_string();
    let args = value.get("args").cloned().unwrap_or_else(|| serde_json::json!({}));

    Some((tool, args))
}

/// Scans for the first balanced `{...}` object in the text (tracking string
/// literals so braces inside strings don't confuse the depth count), so a
/// code fence, trailing prose, or a second JSON object tacked on afterwards
/// doesn't prevent the first (usually intended) tool call from parsing.
fn extract_first_json_object(text: &str) -> Option<String> {
    let start = text.find('{')?;

    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;

    for (offset, ch) in text[start..].char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    let end = start + offset + ch.len_utf8();
                    return Some(text[start..end].to_string());
                }
            }
            _ => {}
        }
    }

    None
}
