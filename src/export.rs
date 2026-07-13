//! Renders a conversation's message history as human-readable Markdown -
//! KRIS's counterpart to Claude Code's own `/export`: something you can
//! read, share, or hand to someone else, unlike the JSON `session_store`
//! persists purely for KRIS itself to reload.

use crate::message::{Message, Role};

pub fn render_export(history: &[Message]) -> String {
    let mut out = String::from("# KRIS Conversation Export\n");

    for message in history {
        match message.role {
            // The system prompt is internal setup, not part of the
            // visible conversation - skipping it keeps the export
            // readable as an actual back-and-forth rather than starting
            // with a wall of tool-use instructions.
            Role::System => continue,
            Role::User => {
                out.push_str("\n## You\n\n");
                if let Some(content) = &message.content {
                    out.push_str(content);
                    out.push('\n');
                }
            }
            Role::Assistant => {
                out.push_str("\n## KRIS\n\n");
                if let Some(content) = &message.content {
                    if !content.is_empty() {
                        out.push_str(content);
                        out.push('\n');
                    }
                }
                for call in message.tool_calls.iter().flatten() {
                    out.push_str(&format!(
                        "\n**Tool call: `{}`**\n\n```json\n{}\n```\n",
                        call.function.name, call.function.arguments
                    ));
                }
            }
            Role::Tool => {
                out.push_str("\n**Tool result:**\n\n```\n");
                out.push_str(message.content.as_deref().unwrap_or(""));
                out.push_str("\n```\n");
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_user_and_assistant_turns() {
        let history = vec![
            Message::system("internal prompt, never shown"),
            Message::user("halo"),
            Message::assistant_text("halo juga!"),
        ];

        let out = render_export(&history);

        assert!(out.contains("## You"));
        assert!(out.contains("halo"));
        assert!(out.contains("## KRIS"));
        assert!(out.contains("halo juga!"));
        assert!(!out.contains("internal prompt"));
    }

    #[test]
    fn renders_tool_calls_and_results() {
        use crate::message::{FunctionCall, ToolCall};

        let history = vec![
            Message::assistant_tool_calls(
                None,
                vec![ToolCall {
                    id: "call_1".to_string(),
                    kind: "function".to_string(),
                    function: FunctionCall {
                        name: "read_file".to_string(),
                        arguments: "{\"path\":\"a.rs\"}".to_string(),
                    },
                }],
            ),
            Message::tool_result("call_1", "fn main() {}"),
        ];

        let out = render_export(&history);

        assert!(out.contains("Tool call: `read_file`"));
        assert!(out.contains("{\"path\":\"a.rs\"}"));
        assert!(out.contains("Tool result:"));
        assert!(out.contains("fn main() {}"));
    }

    #[test]
    fn empty_history_still_renders_a_title() {
        assert!(render_export(&[]).contains("# KRIS Conversation Export"));
    }
}
