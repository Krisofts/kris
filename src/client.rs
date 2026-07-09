use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::StreamExt;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::message::{FunctionCall, Message, ToolCall};

/// Which flavour of OpenAI-compatible endpoint we're talking to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// A local `llama-server` (llama.cpp, `--jinja`). Accepts and benefits
    /// from llama.cpp-specific request fields (`cache_prompt`,
    /// `repeat_penalty`) and exposes `/tokenize` and `/health`.
    Llama,
    /// A remote OpenAI-compatible API (Gemini's compat endpoint). Reached
    /// over HTTPS with a bearer token; only standard OpenAI request fields
    /// are sent, since the extra llama.cpp ones would be rejected.
    OpenAiCompat,
}

/// Talks to an OpenAI-compatible chat-completions endpoint - either a local
/// `llama-server` over plain HTTP (fully offline), or a remote provider like
/// Gemini over HTTPS. The `backend` decides which endpoint-specific request
/// fields and auth are used.
pub struct ModelClient {
    http: reqwest::Client,
    base_url: String,
    model: String,
    backend: Backend,
    api_key: Option<String>,
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
    /// request - the single biggest latency win available on CPU. A
    /// llama.cpp extension, so it's omitted for remote providers that would
    /// reject the unknown field.
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_prompt: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a [Value]>,
    /// Sent explicitly (rather than left to llama-server's own default)
    /// because some chat-template builds bias heavily toward always
    /// emitting a tool call once `tools` is present unless `tool_choice`
    /// says otherwise - "auto" is what actually lets the model answer in
    /// plain text when no tool is warranted.
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'a str>,
    /// Sent explicitly rather than left to llama-server's default -
    /// without a real penalty against repeating itself, a small model at
    /// low temperature (KRIS defaults to 0.2, deterministic on purpose
    /// for coding tasks) is prone to latching onto a repetitive loop and
    /// never emitting a stop token, running all the way to `max_tokens`
    /// instead of a normal-length reply. A llama.cpp field name, so it's
    /// omitted for remote providers (which use `frequency_penalty` instead
    /// and would reject this one).
    #[serde(skip_serializing_if = "Option::is_none")]
    repeat_penalty: Option<f32>,
    /// Asks llama-server to append a final usage chunk (with `choices: []`)
    /// carrying the exact `prompt_tokens` it actually processed. That count
    /// is what the context-budget check needs, and getting it here for free
    /// avoids a separate `/tokenize` round trip that re-tokenizes the whole
    /// conversation every turn once it grows large - real overhead on a
    /// phone. It's also strictly more accurate than tokenizing the raw
    /// message text ourselves, since it reflects the chat-template and tool-
    /// schema tokens the server actually rendered.
    stream_options: StreamOptions,
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
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
    /// Exact number of prompt tokens llama-server reported processing for
    /// this request (from the streamed usage chunk), or `None` if the
    /// server build didn't send one. Lets the caller track context usage
    /// without a separate `/tokenize` call.
    pub prompt_tokens: Option<u32>,
}

/// Whether content deltas are being streamed live via `on_delta`, held
/// back pending a decision, or still too short to tell which.
enum StreamMode {
    Deciding(String),
    Live,
    HeldBack(String),
}

impl ModelClient {
    pub fn new(
        base_url: impl Into<String>,
        model: impl Into<String>,
        backend: Backend,
        api_key: Option<String>,
    ) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(600))
            .build()
            .unwrap_or_default();

        Self {
            http,
            base_url: base_url.into(),
            model: model.into(),
            backend,
            api_key,
        }
    }

    /// Which backend this client talks to - lets the agent pick the right
    /// tool-schema flavour (llama-server takes full JSON Schema; a remote
    /// provider needs it sanitized to the subset it accepts).
    pub fn backend(&self) -> Backend {
        self.backend
    }

    /// The chat-completions URL for this backend. llama-server mounts the
    /// OpenAI routes under `/v1`; Gemini's compat base already ends in
    /// `.../openai`, so the route is just `/chat/completions`.
    fn chat_url(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        match self.backend {
            Backend::Llama => format!("{base}/v1/chat/completions"),
            Backend::OpenAiCompat => format!("{base}/chat/completions"),
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
        let url = self.chat_url();

        // The llama.cpp-only fields are sent only to that backend; a remote
        // OpenAI-compatible provider would reject the unknown keys.
        let is_llama = self.backend == Backend::Llama;

        let request = ChatRequest {
            model: &self.model,
            messages,
            temperature,
            max_tokens,
            stream: true,
            cache_prompt: is_llama.then_some(true),
            tools,
            tool_choice: tools.map(|_| "auto"),
            repeat_penalty: is_llama.then_some(1.1),
            stream_options: StreamOptions {
                include_usage: true,
            },
        };

        const MAX_ATTEMPTS: u32 = 4;
        let mut attempt = 0;

        let response = loop {
            attempt += 1;

            let mut builder = self.http.post(&url).json(&request);
            if let Some(key) = &self.api_key {
                builder = builder.bearer_auth(key);
            }

            match builder.send().await {
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
        let mut prompt_tokens = None;
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

                if let Some(usage) = parsed.usage {
                    if let Some(pt) = usage.prompt_tokens {
                        prompt_tokens = Some(pt);
                    }
                }

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
            prompt_tokens,
        })
    }

    /// Exact token count for `text` via llama-server's `/tokenize`
    /// endpoint, used for context-window budgeting instead of a rough
    /// chars/4 guess.
    pub async fn tokenize(&self, text: &str) -> Result<usize> {
        // Only llama-server exposes `/tokenize`; for a remote provider the
        // caller falls back to its own heuristic when this errors, so bail
        // early rather than firing a request that would just 404.
        if self.backend != Backend::Llama {
            anyhow::bail!("tokenize is only available on the local llama-server backend");
        }

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
    #[serde(default)]
    choices: Vec<ChunkChoice>,
    /// Present only on the final usage chunk (emitted because we send
    /// `stream_options.include_usage`); that chunk carries an empty
    /// `choices` array, hence the `default` above.
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Usage {
    #[serde(default)]
    prompt_tokens: Option<u32>,
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
    /// Defaulted because Gemini's OpenAI-compatible endpoint omits `index`
    /// when there's a single tool call; without the default the whole chunk
    /// would fail to parse and the tool call would be silently dropped. A
    /// lone call at index 0 is exactly what we want in that case anyway.
    #[serde(default)]
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
    /// The OpenAI wire format sends `arguments` as a JSON-encoded *string*
    /// (often fragmented across deltas), but Gemini's compatibility layer
    /// sometimes sends it as an already-parsed JSON *object*. Accept either:
    /// a string is taken as-is (and concatenated by the accumulator), an
    /// object is re-serialized to its JSON text. Without this, an object
    /// payload would fail to deserialize and the tool call would vanish.
    #[serde(default, deserialize_with = "deserialize_arguments")]
    arguments: Option<String>,
}

/// Deserializes a tool-call `arguments` field that may arrive as either a
/// JSON string (OpenAI/llama.cpp) or a JSON object (Gemini), normalizing
/// both to a string.
fn deserialize_arguments<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrJson {
        Str(String),
        Other(Value),
    }

    Ok(
        Option::<StringOrJson>::deserialize(deserializer)?.map(|value| match value {
            StringOrJson::Str(s) => s,
            StringOrJson::Other(v) => v.to_string(),
        }),
    )
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
    fn parses_usage_only_final_chunk() {
        // The chunk llama-server appends when stream_options.include_usage
        // is set: empty choices, a usage object carrying prompt_tokens.
        let chunk: ChatChunk =
            serde_json::from_str(r#"{"choices":[],"usage":{"prompt_tokens":1234,"completion_tokens":7}}"#)
                .unwrap();
        assert!(chunk.choices.is_empty());
        assert_eq!(chunk.usage.and_then(|u| u.prompt_tokens), Some(1234));
    }

    #[test]
    fn parses_content_chunk_without_usage() {
        // An ordinary content delta has no usage field at all - must still
        // parse, with usage left as None.
        let chunk: ChatChunk =
            serde_json::from_str(r#"{"choices":[{"delta":{"content":"hi"}}]}"#).unwrap();
        assert_eq!(chunk.choices.len(), 1);
        assert!(chunk.usage.is_none());
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
    fn parses_gemini_style_tool_call_with_object_args_and_no_index() {
        // Gemini's OpenAI-compat layer can send `arguments` as an object
        // (not the OpenAI-standard JSON string) and omit `index` for a lone
        // call. Both must still parse and yield the tool call.
        let json = r#"{"choices":[{"delta":{"tool_calls":[{"id":"call_x","function":{"name":"read_file","arguments":{"path":"a.rs"}}}]}}]}"#;
        let chunk: ChatChunk = serde_json::from_str(json).unwrap();

        let mut acc = ToolCallAccumulator::default();
        for choice in chunk.choices {
            if let Some(deltas) = choice.delta.tool_calls {
                for delta in deltas {
                    acc.apply(delta);
                }
            }
        }

        let calls = acc.finish();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "read_file");
        let parsed: Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(parsed["path"], "a.rs");
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
