use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::message::{FunctionCall, Message, ToolCall};

/// Talks to a locally running `llama-server` (llama.cpp's OpenAI-compatible
/// HTTP server, started with `--jinja` so it renders Qwen's native
/// tool-calling chat template) over plain HTTP - no TLS stack needed since
/// this only ever talks to 127.0.0.1.
pub struct LlamaClient {
    http: reqwest::Client,
    base_url: String,
    model: String,
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [Message],
    temperature: f32,
    max_tokens: u32,
    stream: bool,
    /// Lets llama-server reuse the KV cache for the unchanged prefix
    /// (system prompt + earlier turns) instead of recomputing it every
    /// request - the single biggest latency win available on CPU.
    cache_prompt: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a [Value]>,
    /// Sent explicitly (rather than left to llama-server's own default)
    /// because some chat-template builds bias heavily toward always
    /// emitting a tool call once `tools` is present unless `tool_choice`
    /// says otherwise - "auto" is what actually lets the model answer in
    /// plain text when no tool is warranted.
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'a str>,
}

/// Final, fully-accumulated result of a streamed chat turn.
#[derive(Debug, Default)]
pub struct StreamOutcome {
    pub content: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    /// Content that looked like it might be a leaked tool call (started
    /// with `{`, `` ` `` as in a ```` ```json ```` fence, or `<` as in
    /// `<tool_call>`) and so was held back from `on_delta` instead of
    /// streamed live. The caller should print this itself if it turns
    /// out not to actually resolve to a tool call - that way a real
    /// leaked call never flashes raw JSON on screen, while a plain-text
    /// answer that just happens to start the same way still gets shown
    /// once it's clear it isn't one.
    pub held_back: Option<String>,
}

/// Whether content deltas are being streamed live via `on_delta`, held
/// back pending a decision, or still too short to tell which.
enum StreamMode {
    Deciding(String),
    Live,
    HeldBack(String),
}

impl LlamaClient {
    pub fn new(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(600))
            .build()
            .unwrap_or_default();

        Self {
            http,
            base_url: base_url.into(),
            model: model.into(),
        }
    }

    /// Streams one chat completion, invoking `on_delta` with each piece of
    /// assistant text content as it arrives so the caller can print it
    /// live, and returns the fully accumulated content/tool_calls once the
    /// stream ends. Retries the initial connection (not a mid-stream drop)
    /// a few times with backoff, since a connection refusal right after
    /// starting llama-server usually means it's still loading the model.
    pub async fn chat_stream(
        &self,
        messages: &[Message],
        tools: Option<&[Value]>,
        temperature: f32,
        max_tokens: u32,
        mut on_delta: impl FnMut(&str),
    ) -> Result<StreamOutcome> {
        let url = format!(
            "{}/v1/chat/completions",
            self.base_url.trim_end_matches('/')
        );

        let request = ChatRequest {
            model: &self.model,
            messages,
            temperature,
            max_tokens,
            stream: true,
            cache_prompt: true,
            tools,
            tool_choice: tools.map(|_| "auto"),
        };

        const MAX_ATTEMPTS: u32 = 4;
        let mut attempt = 0;

        let response = loop {
            attempt += 1;

            match self.http.post(&url).json(&request).send().await {
                Ok(response) => break response.error_for_status()?,
                Err(err) if err.is_connect() && attempt < MAX_ATTEMPTS => {
                    tokio::time::sleep(Duration::from_secs(2 * attempt as u64)).await;
                }
                Err(err) => return Err(err.into()),
            }
        };

        let mut byte_stream = response.bytes_stream();
        let mut buffer: Vec<u8> = Vec::new();
        let mut accumulator = ToolCallAccumulator::default();
        let mut content = String::new();
        let mut got_any_content = false;
        let mut mode = StreamMode::Deciding(String::new());

        while let Some(chunk) = byte_stream.next().await {
            let chunk = chunk.context("reading stream chunk from llama-server")?;
            buffer.extend_from_slice(&chunk);

            for payload in drain_sse_events(&mut buffer) {
                if payload == "[DONE]" {
                    continue;
                }

                let parsed: ChatChunk = match serde_json::from_str(&payload) {
                    Ok(parsed) => parsed,
                    Err(_) => continue, // ignore stray/keep-alive lines
                };

                for choice in parsed.choices {
                    if let Some(text) = choice.delta.content {
                        if !text.is_empty() {
                            content.push_str(&text);
                            got_any_content = true;

                            mode = match mode {
                                StreamMode::Live => {
                                    on_delta(&text);
                                    StreamMode::Live
                                }
                                StreamMode::HeldBack(mut buf) => {
                                    buf.push_str(&text);
                                    StreamMode::HeldBack(buf)
                                }
                                StreamMode::Deciding(mut buf) => {
                                    buf.push_str(&text);
                                    match buf.trim_start().chars().next() {
                                        None => StreamMode::Deciding(buf),
                                        Some('{') | Some('`') | Some('<') => {
                                            StreamMode::HeldBack(buf)
                                        }
                                        Some(_) => {
                                            on_delta(&buf);
                                            StreamMode::Live
                                        }
                                    }
                                }
                            };
                        }
                    }

                    if let Some(deltas) = choice.delta.tool_calls {
                        for delta in deltas {
                            accumulator.apply(delta);
                        }
                    }
                }
            }
        }

        let held_back = match mode {
            StreamMode::HeldBack(buf) => Some(buf),
            StreamMode::Deciding(buf) => {
                // Stream ended before enough arrived to decide (e.g. all
                // whitespace, or a very short reply) - nothing suspicious
                // was ever detected, so just flush it as normal content.
                if !buf.is_empty() {
                    on_delta(&buf);
                }
                None
            }
            StreamMode::Live => None,
        };

        Ok(StreamOutcome {
            content: if got_any_content { Some(content) } else { None },
            tool_calls: accumulator.finish(),
            held_back,
        })
    }

    /// Exact token count for `text` via llama-server's `/tokenize`
    /// endpoint, used for context-window budgeting instead of a rough
    /// chars/4 guess.
    pub async fn tokenize(&self, text: &str) -> Result<usize> {
        let url = format!("{}/tokenize", self.base_url.trim_end_matches('/'));

        #[derive(Serialize)]
        struct Req<'a> {
            content: &'a str,
        }
        #[derive(Deserialize)]
        struct Resp {
            tokens: Vec<Value>,
        }

        let resp: Resp = self
            .http
            .post(&url)
            .json(&Req { content: text })
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        Ok(resp.tokens.len())
    }

    /// Checks llama-server's `/health` endpoint with a short timeout, for
    /// quick "is it up?" checks rather than waiting on inference.
    pub async fn health(&self) -> bool {
        let url = format!("{}/health", self.base_url.trim_end_matches('/'));

        self.http
            .get(url)
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map(|response| response.status().is_success())
            .unwrap_or(false)
    }
}

#[derive(Deserialize)]
struct ChatChunk {
    choices: Vec<ChunkChoice>,
}

#[derive(Deserialize)]
struct ChunkChoice {
    delta: ChunkDelta,
}

#[derive(Deserialize, Default)]
struct ChunkDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ToolCallDelta>>,
}

#[derive(Deserialize)]
struct ToolCallDelta {
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<FunctionDelta>,
}

#[derive(Deserialize, Default)]
struct FunctionDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

/// Accumulates streamed `tool_calls` deltas, which arrive as fragments
/// keyed by `index` (id/name usually only on the first fragment for a
/// given index, `arguments` arrives split across many fragments that must
/// be concatenated in order).
#[derive(Default)]
struct ToolCallAccumulator {
    slots: Vec<Option<PartialToolCall>>,
}

#[derive(Default)]
struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,
}

impl ToolCallAccumulator {
    fn apply(&mut self, delta: ToolCallDelta) {
        if self.slots.len() <= delta.index {
            self.slots.resize_with(delta.index + 1, || None);
        }

        let slot = self.slots[delta.index].get_or_insert_with(PartialToolCall::default);

        if let Some(id) = delta.id {
            slot.id = id;
        }
        if let Some(function) = delta.function {
            if let Some(name) = function.name {
                slot.name.push_str(&name);
            }
            if let Some(arguments) = function.arguments {
                slot.arguments.push_str(&arguments);
            }
        }
    }

    fn finish(self) -> Vec<ToolCall> {
        self.slots
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(i, partial)| ToolCall {
                id: if partial.id.is_empty() {
                    format!("call_{i}")
                } else {
                    partial.id
                },
                kind: "function".to_string(),
                function: FunctionCall {
                    name: partial.name,
                    arguments: partial.arguments,
                },
            })
            .collect()
    }
}

/// Pulls complete SSE `data: ...` payloads out of `buffer`, leaving any
/// trailing partial event in place for the next chunk. SSE frames are
/// separated by a blank line (`\n\n`); splitting only ever happens at that
/// ASCII byte sequence, which can't occur inside a multi-byte UTF-8
/// sequence, so the extracted slices are always valid cut points.
fn drain_sse_events(buffer: &mut Vec<u8>) -> Vec<String> {
    let mut events = Vec::new();

    loop {
        let Some(pos) = find_subslice(buffer, b"\n\n") else {
            break;
        };

        let event_bytes: Vec<u8> = buffer.drain(..pos + 2).collect();
        let event_text = String::from_utf8_lossy(&event_bytes);

        let mut data = String::new();
        for line in event_text.lines() {
            if let Some(rest) = line.strip_prefix("data:") {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(rest.trim_start());
            }
        }

        if !data.is_empty() {
            events.push(data);
        }
    }

    events
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drains_single_complete_event() {
        let mut buffer = b"data: {\"choices\":[]}\n\n".to_vec();
        let events = drain_sse_events(&mut buffer);
        assert_eq!(events, vec!["{\"choices\":[]}".to_string()]);
        assert!(buffer.is_empty());
    }

    #[test]
    fn leaves_partial_event_buffered() {
        let mut buffer = b"data: {\"choic".to_vec();
        let events = drain_sse_events(&mut buffer);
        assert!(events.is_empty());
        assert_eq!(buffer, b"data: {\"choic");
    }

    #[test]
    fn accumulates_split_events_across_chunks() {
        let mut buffer = b"data: {\"choic".to_vec();
        buffer.extend_from_slice(b"es\":[]}\n\ndata: [DONE]\n\n");

        let events = drain_sse_events(&mut buffer);
        assert_eq!(
            events,
            vec!["{\"choices\":[]}".to_string(), "[DONE]".to_string()]
        );
    }

    #[test]
    fn accumulator_joins_fragmented_arguments_in_order() {
        let mut acc = ToolCallAccumulator::default();
        acc.apply(ToolCallDelta {
            index: 0,
            id: Some("call_abc".to_string()),
            function: Some(FunctionDelta {
                name: Some("read_file".to_string()),
                arguments: Some("{\"path\":".to_string()),
            }),
        });
        acc.apply(ToolCallDelta {
            index: 0,
            id: None,
            function: Some(FunctionDelta {
                name: None,
                arguments: Some("\"a.rs\"}".to_string()),
            }),
        });

        let calls = acc.finish();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_abc");
        assert_eq!(calls[0].function.name, "read_file");
        assert_eq!(calls[0].function.arguments, "{\"path\":\"a.rs\"}");
    }

    #[test]
    fn accumulator_handles_multiple_indices() {
        let mut acc = ToolCallAccumulator::default();
        acc.apply(ToolCallDelta {
            index: 1,
            id: Some("call_b".to_string()),
            function: Some(FunctionDelta {
                name: Some("tool_b".to_string()),
                arguments: Some("{}".to_string()),
            }),
        });
        acc.apply(ToolCallDelta {
            index: 0,
            id: Some("call_a".to_string()),
            function: Some(FunctionDelta {
                name: Some("tool_a".to_string()),
                arguments: Some("{}".to_string()),
            }),
        });

        let calls = acc.finish();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].function.name, "tool_a");
        assert_eq!(calls[1].function.name, "tool_b");
    }
}
