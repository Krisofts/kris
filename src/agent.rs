use std::path::Path;

use anyhow::Result;
use serde_json::{json, Value};

use crate::client::{Backend, ModelClient};
use crate::message::{FunctionCall, Message, Role, ToolCall};
use crate::style::yellow;
use crate::tools::{ToolError, ToolRegistry};

/// Groups the three pieces of per-workspace context `run` needs, mainly to
/// keep its argument count reasonable rather than because they're a
/// reusable unit elsewhere.
pub struct Project<'a> {
    pub root: &'a Path,
    pub name: &'a str,
    pub type_hint: &'a str,
}

pub struct Agent {
    client: ModelClient,
    tools: ToolRegistry,
    temperature: f32,
    max_tokens: u32,
    context_size: u32,
}

impl Agent {
    pub fn new(
        client: ModelClient,
        tools: ToolRegistry,
        temperature: f32,
        max_tokens: u32,
        context_size: u32,
    ) -> Self {
        Self {
            client,
            tools,
            temperature,
            max_tokens,
            context_size,
        }
    }

    fn system_prompt(&self, project_name: &str, project_type_hint: &str) -> String {
        // Tool schemas travel in the request's `tools` field now (native
        // tool-calling via llama-server --jinja), not spelled out as JSON
        // prose here - keeps this prompt, and its reprocessing cost on
        // every turn, small.
        let type_line = if project_type_hint.is_empty() {
            String::new()
        } else {
            format!(" {project_type_hint}")
        };

        // Kept short on purpose: every word here is reprocessed on the
        // first turn of each session (and again whenever the KV cache
        // can't be reused), which is real latency on phone-class CPUs -
        // a longer, more thorough-sounding prompt is not a free win.
        format!(
            "You are KRIS, an offline coding assistant running locally in a terminal, \
             working inside \"{project_name}\".{type_line} Only use a tool when the request \
             needs it - for chit-chat or general questions, just answer in plain text. One \
             tool call at a time; wait for its result before the next step. Prefer edit_file \
             over write_file for existing files. For a long new file, write a short first \
             chunk with write_file and grow it with several append_file calls instead of one \
             large write_file - keeps each step within a safe token budget instead of risking \
             a truncated response mid-file. For a brand-new project, use the language's own \
             scaffolding command via run_command (cargo new, npm init -y, django-admin \
             startproject, go mod init, etc.) instead of creating files by hand, unless no \
             such generator exists. A shell built-in (echo, cat, ls, mkdir, ...) is not a \
             tool by itself - run it via run_command. Verify nontrivial changes by \
             building/testing via run_command before finishing, when the project has such a \
             command. Only commit to git when explicitly asked, via git_commit - never \
             automatically. Give your final answer as plain text."
        )
    }

    /// Runs one user turn to completion, calling tools as needed, streaming
    /// assistant text live via `on_delta`, and returns the final
    /// natural-language answer.
    pub async fn run(
        &self,
        history: &mut Vec<Message>,
        project: Project<'_>,
        user_input: &str,
        max_iterations: u32,
        mut on_delta: impl FnMut(&str),
        mut on_tool_call: impl FnMut(&str, &Value, &str),
    ) -> Result<String> {
        let root = project.root;

        if history.is_empty() {
            history.push(Message::system(
                self.system_prompt(project.name, project.type_hint),
            ));
        }

        // Recorded so a request failure (llama-server unreachable mid-turn)
        // can roll the whole turn back out of history instead of leaving a
        // dangling user message a caller's retry would otherwise duplicate.
        let turn_start = history.len();
        history.push(Message::user(user_input));

        // A remote provider (Gemini, Claude) validates tool schemas
        // strictly and expects its own shape, so hand it the matching
        // sanitized form; llama-server takes the full schema as-is.
        let tool_schemas = match self.client.backend() {
            Backend::OpenAiCompat => self.tools.describe_all_gemini(),
            Backend::Anthropic => self.tools.describe_all_anthropic(),
            Backend::Llama => self.tools.describe_all(),
        };

        // Tracks the previous iteration's tool call(s) so an identical
        // repeat (the model proposing the exact same call again right
        // after it was declined or errored, with nothing about the
        // situation having changed) stops the turn instead of looping
        // through the rest of `max_iterations` asking the same thing.
        let mut previous_call_signature: Option<String> = None;

        // Last exact prompt-token count llama-server reported, paired with
        // the history length it was measured at, so the budget check can
        // extrapolate the current size (that count plus a heuristic estimate
        // of whatever's been appended since) instead of paying for a fresh
        // `/tokenize` round trip every iteration.
        let mut measured: Option<(usize, u32)> = None;

        for _ in 0..max_iterations {
            self.enforce_context_budget(history, &mut measured, &mut on_delta)
                .await;

            let sent_len = history.len();
            let outcome = match self
                .client
                .chat_stream(
                    history,
                    Some(&tool_schemas),
                    self.temperature,
                    self.max_tokens,
                    &mut on_delta,
                )
                .await
            {
                Ok(outcome) => outcome,
                Err(err) => {
                    history.truncate(turn_start);
                    return Err(err);
                }
            };

            if let Some(prompt_tokens) = outcome.prompt_tokens {
                measured = Some((sent_len, prompt_tokens));
            }

            let held_back = outcome.held_back;

            let tool_calls = if outcome.tool_calls.is_empty() {
                outcome
                    .content
                    .as_deref()
                    .and_then(parse_tool_call_from_text)
                    // Confirmed on-device: without a working tool-calling
                    // grammar, a model can hallucinate a call to a tool
                    // name that doesn't exist at all (e.g. "hello", echoing
                    // the user's greeting) - executing that just produces
                    // an "unknown tool" error the model then rambles about
                    // instead of answering normally. Only trust this
                    // fallback for a name that's actually registered;
                    // anything else falls through to being shown as plain
                    // text like any other answer.
                    .filter(|(name, _)| self.tools.names().contains(&name.as_str()))
                    .map(|(name, args)| {
                        vec![ToolCall {
                            id: "call_fallback_0".to_string(),
                            kind: "function".to_string(),
                            function: FunctionCall {
                                name,
                                arguments: args.to_string(),
                            },
                        }]
                    })
                    .unwrap_or_default()
            } else {
                outcome.tool_calls
            };

            if tool_calls.is_empty() {
                // Nothing resolved to a tool call after all, so whatever
                // was held back from live streaming (it looked like it
                // might be a leaked call) was actually just part of the
                // answer - show it now before returning.
                if let Some(held_back) = held_back {
                    on_delta(&held_back);
                }

                let answer = outcome.content.unwrap_or_default();
                history.push(Message::assistant_text(answer.clone()));
                return Ok(answer);
            }

            let signature = tool_calls
                .iter()
                .map(|call| format!("{}:{}", call.function.name, call.function.arguments))
                .collect::<Vec<_>>()
                .join("|");

            if previous_call_signature.as_deref() == Some(signature.as_str()) {
                let notice = "Stopped: the same tool call was proposed again right after it \
                     was declined or failed, with nothing else changed. Ask again with more \
                     detail, or approve the action if you do want it to run."
                    .to_string();
                on_delta(&notice);
                history.push(Message::assistant_text(notice.clone()));
                return Ok(notice);
            }
            previous_call_signature = Some(signature);

            history.push(Message::assistant_tool_calls(
                outcome.content.clone(),
                tool_calls.clone(),
            ));

            for call in &tool_calls {
                let args = match call.parsed_arguments() {
                    Ok(args) => args,
                    Err(err) => {
                        let msg = format!("Error: invalid JSON arguments: {err}");
                        on_tool_call(&call.function.name, &json!({}), &msg);
                        history.push(Message::tool_result(call.id.clone(), msg));
                        continue;
                    }
                };

                let result = match self.tools.execute(&call.function.name, root, &args) {
                    Ok(output) => output,
                    Err(ToolError::UnknownTool(name)) => {
                        let available = self.tools.names().join(", ");
                        format!(
                            "Error: there is no tool called \"{name}\". Available tools: \
                             {available}. Use exactly one of these names."
                        )
                    }
                    Err(err) => format!("Error: {err}"),
                };

                on_tool_call(&call.function.name, &args, &result);
                history.push(Message::tool_result(call.id.clone(), result));
            }
        }

        let notice = "Reached the maximum number of tool calls without a final answer. Ask \
             KRIS to continue - the conversation so far is kept, so it can pick up where it \
             left off instead of starting over."
            .to_string();
        on_delta(&notice);
        history.push(Message::assistant_text(notice.clone()));
        Ok(notice)
    }

    /// Decides whether the prompt is close enough to `context_size` to need
    /// trimming, then drops the oldest whole turns (a turn = a user message
    /// through to just before the next one) until back under budget, always
    /// keeping the current, unanswered turn.
    ///
    /// The size estimate is cheap: when llama-server has already reported an
    /// exact `prompt_tokens` for an earlier request this turn (`measured`),
    /// it extrapolates from that plus a chars/4 heuristic for whatever's
    /// been appended since - no network round trip. Only before the first
    /// such report (or right after a trim invalidates it) does it fall back
    /// to the old path: a chars/4 gate, then a real `/tokenize` call.
    async fn enforce_context_budget(
        &self,
        history: &mut Vec<Message>,
        measured: &mut Option<(usize, u32)>,
        on_delta: &mut impl FnMut(&str),
    ) {
        let soft_limit = (self.context_size as f64 * 0.9) as usize;

        let estimate = match *measured {
            Some((len, prompt_tokens)) => {
                let len = len.min(history.len());
                prompt_tokens as usize + heuristic_tokens(&history[len..])
            }
            None => {
                let heuristic = heuristic_tokens(history);
                if heuristic < soft_limit * 3 / 4 {
                    return;
                }
                let joined = joined_text(history);
                self.client.tokenize(&joined).await.unwrap_or(heuristic)
            }
        };

        if estimate <= soft_limit {
            return;
        }

        let mut dropped_turns = 0;

        // Always keep at least the most recent turn (the last user message
        // onward) so the current request never gets erased out from under
        // itself. Recompute turn boundaries after each drop rather than
        // reusing stale indices, since draining shifts everything after it.
        loop {
            let turn_starts: Vec<usize> = history
                .iter()
                .enumerate()
                .filter(|(_, m)| m.role == Role::User)
                .map(|(i, _)| i)
                .collect();

            if turn_starts.len() <= 1 || heuristic_tokens(history) <= soft_limit {
                break;
            }

            let start = if history[0].role == Role::System {
                1
            } else {
                0
            };
            let next_turn_start = turn_starts[1];

            history.drain(start..next_turn_start);
            dropped_turns += 1;
        }

        if dropped_turns > 0 {
            // Draining from the front shifted every index, so the cached
            // (len, prompt_tokens) pair no longer lines up with `history` -
            // drop it and let the next request re-measure from scratch.
            *measured = None;

            let notice = format!(
                "\n{}\n",
                yellow("(older conversation history trimmed to stay within the context window)")
            );
            on_delta(&notice);
        }
    }
}

/// Rough chars/4 token estimate for a slice of messages - exposed so
/// callers (the REPL's post-turn token/time footer) can estimate a
/// specific turn's size without a real `/tokenize` round trip.
pub fn heuristic_tokens(history: &[Message]) -> usize {
    history
        .iter()
        .map(|m| {
            let content_len = m.content.as_deref().map(str::len).unwrap_or(0);
            let calls_len: usize = m
                .tool_calls
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .map(|c| c.function.name.len() + c.function.arguments.len())
                .sum();
            (content_len + calls_len) / 4
        })
        .sum()
}

fn joined_text(history: &[Message]) -> String {
    history
        .iter()
        .filter_map(|m| m.content.as_deref())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Defensive fallback: if the server ever returns a tool call as plain
/// content instead of the structured `tool_calls` field (e.g. --jinja
/// wasn't enabled, an older llama-server build, or this model's chat
/// template just isn't getting server-side tool-call parsing), scan for a
/// single balanced `{...}` JSON object describing a call and pull out a
/// (name, args) pair. Different model families spell this differently -
/// KRIS's own `{"tool": ..., "args": {...}}`, and the OpenAI/Hermes-style
/// `{"name": ..., "arguments": {...}}` that Qwen models leak (often
/// wrapped in `<tool_call>...</tool_call>` tags) plus its nested
/// `{"function": {"name": ..., "arguments": {...}}}` variant - are all
/// tried so a leaked call still actually runs instead of just being
/// printed as if it were the final answer.
fn parse_tool_call_from_text(text: &str) -> Option<(String, Value)> {
    let json_str = extract_first_json_object(text)?;
    let value: Value = serde_json::from_str(&json_str).ok()?;
    extract_tool_call(&value)
}

fn extract_tool_call(value: &Value) -> Option<(String, Value)> {
    if let Some(function) = value.get("function") {
        if let Some(result) = extract_tool_call(function) {
            return Some(result);
        }
    }

    let name_key = ["tool", "name", "tool_name"]
        .into_iter()
        .find(|key| value.get(*key).and_then(Value::as_str).is_some())?;

    let args_key = ["args", "arguments", "parameters", "input"]
        .into_iter()
        .find(|key| value.get(*key).is_some());

    // A bare "name" field with no sibling arguments key is common enough
    // in ordinary JSON (a person record, a config example) that treating
    // it alone as an attempted tool call would misfire too often - only
    // the more distinctive "tool"/"tool_name" keys are trusted on their
    // own.
    if args_key.is_none() && name_key == "name" {
        return None;
    }

    let name = value.get(name_key)?.as_str()?.to_string();
    let args = args_key
        .and_then(|key| value.get(key))
        .cloned()
        .unwrap_or_else(|| json!({}));

    // Arguments sometimes arrive as a JSON-encoded string rather than a
    // nested object, matching how OpenAI's wire format represents them.
    let args = match args {
        Value::String(s) => serde_json::from_str(&s).unwrap_or(Value::String(s)),
        other => other,
    };

    Some((name, args))
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_tool_call_from_leaked_plain_text() {
        let text = "{\"tool\": \"read_file\", \"args\": {\"path\": \"a.rs\"}}";
        let (name, args) = parse_tool_call_from_text(text).unwrap();
        assert_eq!(name, "read_file");
        assert_eq!(args["path"], "a.rs");
    }

    #[test]
    fn ignores_plain_prose_without_json() {
        assert!(parse_tool_call_from_text("Here's the answer: it's 42.").is_none());
    }

    #[test]
    fn extracts_hermes_style_tool_call_wrapped_in_tags() {
        let text = "<tool_call>\n\
             {\"name\": \"create_directory\", \"arguments\": {\"path\": \"myproj\"}}\n\
             </tool_call>";
        let (name, args) = parse_tool_call_from_text(text).unwrap();
        assert_eq!(name, "create_directory");
        assert_eq!(args["path"], "myproj");
    }

    #[test]
    fn extracts_nested_openai_style_function_call() {
        let text = "{\"function\": {\"name\": \"run_command\", \"arguments\": {\"command\": \"cargo new myproj\"}}}";
        let (name, args) = parse_tool_call_from_text(text).unwrap();
        assert_eq!(name, "run_command");
        assert_eq!(args["command"], "cargo new myproj");
    }

    #[test]
    fn parses_stringified_arguments() {
        let text = "{\"name\": \"write_file\", \"arguments\": \"{\\\"path\\\": \\\"a.rs\\\", \\\"content\\\": \\\"fn main() {}\\\"}\"}";
        let (name, args) = parse_tool_call_from_text(text).unwrap();
        assert_eq!(name, "write_file");
        assert_eq!(args["path"], "a.rs");
    }

    #[test]
    fn ignores_bare_name_field_without_arguments() {
        assert!(parse_tool_call_from_text("{\"name\": \"John\", \"age\": 30}").is_none());
    }

    #[test]
    fn heuristic_tokens_counts_content_and_tool_calls() {
        let history = vec![Message::user("hello world")];
        assert_eq!(heuristic_tokens(&history), "hello world".len() / 4);
    }
}
