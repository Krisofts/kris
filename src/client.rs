use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::StreamExt;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{json, Value};

use crate::message::{FunctionCall, Message, Role, ToolCall};

/// Which flavour of endpoint we're talking to.
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
    /// Claude's native Messages API (`/v1/messages`). Not OpenAI-shaped at
    /// all: system prompt is a separate top-level field, message content is
    /// an array of typed blocks (text/tool_use/tool_result) rather than a
    /// flat string, auth is an `x-api-key` header rather than a bearer
    /// token, and streaming uses named SSE events instead of OpenAI's
    /// undifferentiated delta chunks.
    Anthropic,
}

/// Anthropic API version pinned in the required `anthropic-version` header.
const ANTHROPIC_VERSION: &str = "2023-06-01";

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
    /// OpenRouter's `reasoning.effort` override (`"none"`, `"minimal"`,
    /// `"low"`, `"medium"`, or `"high"`), unset by default so most
    /// providers/models see no `reasoning` field at all. Reasoning models
    /// routed through OpenRouter (e.g. Tencent's Hy3) can otherwise spend
    /// their entire `max_tokens` budget on hidden "thinking" before ever
    /// writing a visible answer or tool call - capping effort here leaves
    /// more of that budget for the actual response.
    reasoning_effort: Option<String>,
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
    /// OpenRouter's reasoning-effort override (see `ModelClient::
    /// with_reasoning_effort`). Omitted entirely unless explicitly set, so
    /// providers/models with no opinion on reasoning never see this field.
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<Value>,
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

/// Cap on how much text the "might be a leaked tool call" heuristic will
/// hold back before giving up and flushing it live regardless. Without a
/// cap, a reply that merely *starts* with `{`/`` ` ``/`<` (Qwen occasionally
/// opens a plain-text answer with a stray `<tool_call>`-shaped fragment
/// before abandoning it) stayed buffered for the *entire* remaining
/// generation - `on_delta` never fired, so the REPL's "thinking..." spinner
/// never updated, making a merely slow reply look completely frozen.
///
/// Confirmed against a real llama-server (Qwen2.5-Coder-1.5B, build
/// b9888): when `--jinja` doesn't actually engage grammar-constrained
/// tool-calling for a given model/template, attaching `tools` to the
/// request doesn't make the model emit structured `tool_calls` at all -
/// it just writes a plain-text ` ```json ` fence attempting to imitate one,
/// unconstrained, with no natural stopping point. That's a real, observed
/// failure mode here, not a hypothetical - so this leans toward showing
/// something on screen sooner rather than optimizing for never
/// prematurely revealing a real leaked tool call (which is typically
/// short and well under this anyway). It only shortens the *visible*
/// silence, not total generation time - that needs a working
/// tool-calling grammar (recent llama.cpp) or a smaller max_tokens.
const MAX_HELD_BACK_BYTES: usize = 150;

/// Advances the held-back/live decision state machine by one content
/// delta, calling `on_delta` immediately for text that's live (or that
/// just became live/was released from holding), and never for text that's
/// still being held back pending a decision. Pulled out of the streaming
/// loop so the cap behavior can be unit-tested deterministically, without
/// depending on how a real byte stream happens to chunk network reads.
fn apply_content_delta(
    mode: StreamMode,
    text: &str,
    on_delta: &mut impl FnMut(&str),
) -> StreamMode {
    match mode {
        StreamMode::Live => {
            on_delta(text);
            StreamMode::Live
        }
        StreamMode::HeldBack(mut buf) => {
            buf.push_str(text);
            if buf.len() >= MAX_HELD_BACK_BYTES {
                // Held long enough without turning into a real tool call -
                // stop gambling on it being one and let the rest stream
                // live, so the spinner has something to show.
                on_delta(&buf);
                StreamMode::Live
            } else {
                StreamMode::HeldBack(buf)
            }
        }
        StreamMode::Deciding(mut buf) => {
            buf.push_str(text);
            match buf.trim_start().chars().next() {
                None => StreamMode::Deciding(buf),
                Some('{') | Some('`') | Some('<') => StreamMode::HeldBack(buf),
                Some(_) => {
                    on_delta(&buf);
                    StreamMode::Live
                }
            }
        }
    }
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
            reasoning_effort: None,
        }
    }

    /// Sets the `reasoning.effort` override sent with every request on the
    /// OpenAI-compatible path (OpenRouter's own field - ignored by
    /// llama-server and Gemini's compat endpoint, which never receive it
    /// since only `server::client_for`'s OpenRouter branch calls this).
    pub fn with_reasoning_effort(mut self, effort: Option<String>) -> Self {
        self.reasoning_effort = effort;
        self
    }

    /// Which backend this client talks to - lets the agent pick the right
    /// tool-schema flavour (llama-server takes full JSON Schema; a remote
    /// provider needs it sanitized to the subset it accepts).
    pub fn backend(&self) -> Backend {
        self.backend
    }

    /// The chat-completions URL for this backend. llama-server mounts the
    /// OpenAI routes under `/v1`; Gemini's compat base already ends in
    /// `.../openai`, so the route is just `/chat/completions`; Claude's
    /// native route is `/v1/messages`.
    fn chat_url(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        match self.backend {
            Backend::Llama => format!("{base}/v1/chat/completions"),
            Backend::OpenAiCompat => format!("{base}/chat/completions"),
            Backend::Anthropic => format!("{base}/v1/messages"),
        }
    }

    /// Streams one chat completion, invoking `on_delta` with each piece of
    /// assistant text content as it arrives so the caller can print it
    /// live, and returns the fully accumulated content/tool_calls once the
    /// stream ends. Dispatches to the OpenAI-shaped or Claude-native
    /// implementation depending on `backend`.
    pub async fn chat_stream(
        &self,
        messages: &[Message],
        tools: Option<&[Value]>,
        temperature: f32,
        max_tokens: u32,
        on_delta: impl FnMut(&str),
        on_activity: impl FnMut(&str),
    ) -> Result<StreamOutcome> {
        match self.backend {
            Backend::Llama | Backend::OpenAiCompat => {
                self.chat_stream_openai(
                    messages,
                    tools,
                    temperature,
                    max_tokens,
                    on_delta,
                    on_activity,
                )
                .await
            }
            Backend::Anthropic => {
                self.chat_stream_anthropic(
                    messages,
                    tools,
                    temperature,
                    max_tokens,
                    on_delta,
                    on_activity,
                )
                .await
            }
        }
    }

    /// Retries the initial connection (not a mid-stream drop) a few times
    /// with backoff, since a connection refusal right after starting
    /// llama-server usually means it's still loading the model.
    async fn chat_stream_openai(
        &self,
        messages: &[Message],
        tools: Option<&[Value]>,
        temperature: f32,
        max_tokens: u32,
        mut on_delta: impl FnMut(&str),
        mut on_activity: impl FnMut(&str),
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
            reasoning: self
                .reasoning_effort
                .as_ref()
                .map(|effort| json!({ "effort": effort })),
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
                Ok(response) if response.status().is_success() => break response,
                Ok(response) => return Err(response_error(response).await),
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
        let mut finish_reason: Option<String> = None;

        // Labeled so "[DONE]" can end the whole read loop immediately,
        // rather than relying on the connection itself closing right
        // after: on-device, a server/proxy that keeps an idle keep-alive
        // connection open past the final SSE event left byte_stream.next()
        // parked waiting for more bytes that were never coming, hanging
        // the entire turn forever with no error and no recovery. The
        // Anthropic path already breaks on its own terminal event
        // (MessageStop) for the same reason - this brings the OpenAI-
        // compatible path (llama-server, Gemini, OpenRouter) in line with
        // it instead of trusting the transport to end things.
        // This path is shared by llama-server, Gemini, and OpenRouter - a
        // hardcoded "llama-server" here used to show up verbatim even when
        // talking to an online provider, confusingly blaming the wrong
        // backend for a dropped connection.
        let source = if is_llama {
            "llama-server"
        } else {
            "the model"
        };

        'outer: while let Some(chunk) = byte_stream.next().await {
            let chunk = chunk.with_context(|| format!("reading stream chunk from {source}"))?;
            buffer.extend_from_slice(&chunk);

            for payload in drain_sse_events(&mut buffer) {
                if payload == "[DONE]" {
                    break 'outer;
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
                    // Deliberately not streamed to on_delta: a reasoning
                    // model's raw chain-of-thought can run to thousands of
                    // words, which used to flood a phone terminal with a
                    // wall of text (confirmed on-device). The REPL's
                    // spinner already shows a live "still working" status
                    // for as long as no real content has arrived - a
                    // reasoning trace doesn't need its own visible
                    // rendering on top of that. It's still never counted
                    // toward `got_any_content` either way, so a reasoning-
                    // only stream still triggers the truncation
                    // diagnostic below rather than being mistaken for a
                    // real answer.
                    if let Some(text) = choice.delta.content {
                        if !text.is_empty() {
                            content.push_str(&text);
                            got_any_content = true;
                            mode = apply_content_delta(mode, &text, &mut on_delta);
                        }
                    }

                    if let Some(deltas) = choice.delta.tool_calls {
                        for delta in deltas {
                            let index = delta.index;
                            accumulator.apply(delta);
                            // Tells the caller which tool the model is in
                            // the middle of calling well before it's
                            // actually executed - the name usually arrives
                            // whole in one delta, but arguments keep
                            // streaming in after it, so this fires on
                            // every one of those too (harmless: same name,
                            // just re-reported).
                            if let Some(name) = accumulator.current_name(index) {
                                on_activity(name);
                            }
                        }
                    }

                    if let Some(reason) = choice.finish_reason {
                        finish_reason = Some(reason);
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

        let tool_calls = accumulator.finish();

        // A stream that ended with no content and no tool call at all
        // usually isn't a deliberate empty reply - it's most often a
        // reasoning model that spent its entire `max_tokens` budget on a
        // hidden "thinking" field this client never sees, so nothing ever
        // arrived in `delta.content`. Surface that instead of silently
        // showing nothing, which otherwise looks indistinguishable from a
        // crash. Left as `None` for a genuine `finish_reason: "stop"` with
        // no content, since that's a real (if unusual) empty answer.
        let content = if got_any_content {
            Some(content)
        } else if tool_calls.is_empty() {
            let note = match finish_reason.as_deref() {
                Some("length") => Some(
                    "(Model gave no visible answer before hitting max_tokens - it likely spent \
                     its whole budget on hidden reasoning. Try raising it, e.g. `config set \
                     max_tokens 4096`, or switch models.)"
                        .to_string(),
                ),
                Some(reason) if reason != "stop" => Some(format!(
                    "(Model gave no visible answer - finish_reason: \"{reason}\".)"
                )),
                _ => None,
            };
            if let Some(note) = &note {
                on_delta(note);
            }
            note
        } else {
            None
        };

        Ok(StreamOutcome {
            content,
            tool_calls,
            held_back,
            prompt_tokens,
        })
    }

    /// Claude's native Messages API streaming implementation. Unlike the
    /// OpenAI-shaped path, text and tool-use content arrive as distinct,
    /// explicitly typed content blocks - there's no risk of a tool call
    /// "leaking" into plain text the way an imperfectly grammar-constrained
    /// local model can, so text deltas are streamed straight to `on_delta`
    /// with no held-back/deciding buffering.
    async fn chat_stream_anthropic(
        &self,
        messages: &[Message],
        tools: Option<&[Value]>,
        temperature: f32,
        max_tokens: u32,
        mut on_delta: impl FnMut(&str),
        mut on_activity: impl FnMut(&str),
    ) -> Result<StreamOutcome> {
        let url = self.chat_url();
        let (system, anthropic_messages) = to_anthropic_messages(messages);

        let request = AnthropicRequest {
            model: &self.model,
            max_tokens,
            temperature,
            stream: true,
            system,
            messages: anthropic_messages,
            tools: tools.map(|t| t.to_vec()),
            tool_choice: tools.map(|_| json!({ "type": "auto" })),
        };

        const MAX_ATTEMPTS: u32 = 4;
        let mut attempt = 0;

        let response = loop {
            attempt += 1;

            let mut builder = self
                .http
                .post(&url)
                .header("anthropic-version", ANTHROPIC_VERSION)
                .json(&request);
            if let Some(key) = &self.api_key {
                builder = builder.header("x-api-key", key);
            }

            match builder.send().await {
                Ok(response) if response.status().is_success() => break response,
                Ok(response) => return Err(response_error(response).await),
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
        let mut stream_error: Option<String> = None;

        'outer: while let Some(chunk) = byte_stream.next().await {
            let chunk = chunk.context("reading stream chunk from Claude")?;
            buffer.extend_from_slice(&chunk);

            for payload in drain_sse_events(&mut buffer) {
                let event: AnthropicStreamEvent = match serde_json::from_str(&payload) {
                    Ok(event) => event,
                    Err(_) => continue, // ignore stray/keep-alive lines
                };

                match event {
                    AnthropicStreamEvent::MessageStart { message } => {
                        if let Some(usage) = message.usage {
                            prompt_tokens = usage.input_tokens;
                        }
                    }
                    AnthropicStreamEvent::ContentBlockStart {
                        index,
                        content_block: AnthropicContentBlockStart::ToolUse { id, name },
                    } => {
                        // Unlike the OpenAI-compatible path, Anthropic always
                        // sends the whole tool name in this one event rather
                        // than fragmenting it - safe to report right away.
                        on_activity(&name);
                        accumulator.apply(ToolCallDelta {
                            index,
                            id: Some(id),
                            function: Some(FunctionDelta {
                                name: Some(name),
                                arguments: None,
                            }),
                        });
                    }
                    AnthropicStreamEvent::ContentBlockStart { .. } => {}
                    AnthropicStreamEvent::ContentBlockDelta {
                        delta: AnthropicDelta::TextDelta { text },
                        ..
                    } => {
                        if !text.is_empty() {
                            on_delta(&text);
                            content.push_str(&text);
                            got_any_content = true;
                        }
                    }
                    AnthropicStreamEvent::ContentBlockDelta {
                        index,
                        delta: AnthropicDelta::InputJsonDelta { partial_json },
                    } => {
                        accumulator.apply(ToolCallDelta {
                            index,
                            id: None,
                            function: Some(FunctionDelta {
                                name: None,
                                arguments: Some(partial_json),
                            }),
                        });
                    }
                    AnthropicStreamEvent::ContentBlockStop { .. }
                    | AnthropicStreamEvent::MessageDelta
                    | AnthropicStreamEvent::Ping => {}
                    AnthropicStreamEvent::MessageStop => break 'outer,
                    AnthropicStreamEvent::Error { error } => {
                        stream_error = Some(error.message);
                        break 'outer;
                    }
                }
            }
        }

        if let Some(message) = stream_error {
            anyhow::bail!("Claude returned an error: {message}");
        }

        Ok(StreamOutcome {
            content: if got_any_content { Some(content) } else { None },
            tool_calls: accumulator.finish(),
            held_back: None,
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
    /// Only present on the final chunk for a choice. Used to tell a
    /// genuinely empty reply apart from one truncated by `max_tokens` -
    /// the latter is common with reasoning models that spend their whole
    /// budget on a hidden "thinking" field this client doesn't parse,
    /// leaving `delta.content` empty for the entire stream.
    #[serde(default)]
    finish_reason: Option<String>,
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

// --- Claude's native Messages API wire format ---------------------------

#[derive(Serialize)]
struct AnthropicRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    temperature: f32,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<Value>,
}

#[derive(Serialize)]
struct AnthropicMessage {
    role: &'static str,
    content: Vec<AnthropicContentBlock>,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
    },
}

/// Converts KRIS's internal OpenAI-shaped `Message` history into Claude's
/// native shape: the system prompt is pulled out into its own field (there
/// is no `system`-role message), and consecutive messages of the same role
/// are merged into one - Claude's API rejects two consecutive messages with
/// the same role, but the agent loop pushes one `Role::Tool` entry *per*
/// tool call, which after a multi-tool-call turn means several consecutive
/// "tool" entries that all need to become content blocks of a single
/// "user" message replying to the preceding assistant turn.
fn to_anthropic_messages(messages: &[Message]) -> (Option<String>, Vec<AnthropicMessage>) {
    let mut system = String::new();
    let mut out: Vec<AnthropicMessage> = Vec::new();

    for message in messages {
        let (role, block): (&'static str, AnthropicContentBlock) = match message.role {
            Role::System => {
                if let Some(text) = &message.content {
                    if !system.is_empty() {
                        system.push('\n');
                    }
                    system.push_str(text);
                }
                continue;
            }
            Role::User => (
                "user",
                AnthropicContentBlock::Text {
                    text: message.content.clone().unwrap_or_default(),
                },
            ),
            Role::Tool => (
                "user",
                AnthropicContentBlock::ToolResult {
                    tool_use_id: message.tool_call_id.clone().unwrap_or_default(),
                    content: message.content.clone().unwrap_or_default(),
                },
            ),
            Role::Assistant => {
                let mut blocks = Vec::new();
                if let Some(text) = &message.content {
                    if !text.is_empty() {
                        blocks.push(AnthropicContentBlock::Text { text: text.clone() });
                    }
                }
                for call in message.tool_calls.iter().flatten() {
                    blocks.push(AnthropicContentBlock::ToolUse {
                        id: call.id.clone(),
                        name: call.function.name.clone(),
                        input: call.parsed_arguments().unwrap_or_else(|_| json!({})),
                    });
                }
                // Claude rejects an empty content array outright.
                if blocks.is_empty() {
                    blocks.push(AnthropicContentBlock::Text {
                        text: String::new(),
                    });
                }
                merge_or_push(&mut out, "assistant", blocks);
                continue;
            }
        };

        merge_or_push(&mut out, role, vec![block]);
    }

    let system = (!system.is_empty()).then_some(system);
    (system, out)
}

/// Appends `blocks` to the last message if it's already the same role,
/// otherwise starts a new message - the merging step `to_anthropic_messages`
/// relies on to keep roles strictly alternating.
fn merge_or_push(
    out: &mut Vec<AnthropicMessage>,
    role: &'static str,
    blocks: Vec<AnthropicContentBlock>,
) {
    match out.last_mut() {
        Some(last) if last.role == role => last.content.extend(blocks),
        _ => out.push(AnthropicMessage {
            role,
            content: blocks,
        }),
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicStreamEvent {
    MessageStart {
        message: AnthropicMessageStart,
    },
    ContentBlockStart {
        index: usize,
        content_block: AnthropicContentBlockStart,
    },
    ContentBlockDelta {
        index: usize,
        delta: AnthropicDelta,
    },
    ContentBlockStop {
        #[allow(dead_code)]
        index: usize,
    },
    MessageDelta,
    MessageStop,
    Ping,
    Error {
        error: AnthropicError,
    },
}

#[derive(Deserialize)]
struct AnthropicMessageStart {
    #[serde(default)]
    usage: Option<AnthropicUsage>,
}

#[derive(Deserialize)]
struct AnthropicUsage {
    #[serde(default)]
    input_tokens: Option<u32>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicContentBlockStart {
    Text {
        #[serde(default)]
        #[allow(dead_code)]
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
    },
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicDelta {
    TextDelta { text: String },
    InputJsonDelta { partial_json: String },
}

#[derive(Deserialize)]
struct AnthropicError {
    message: String,
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
    /// The tool name accumulated so far for `index`, if any - used to tell
    /// the caller which tool the model is in the middle of calling while
    /// its arguments are still streaming in, well before it's actually
    /// executed.
    fn current_name(&self, index: usize) -> Option<&str> {
        self.slots
            .get(index)?
            .as_ref()
            .filter(|slot| !slot.name.is_empty())
            .map(|slot| slot.name.as_str())
    }

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

/// Turns a non-2xx HTTP response into an error that includes the response
/// body, not just the status code. `reqwest::Response::error_for_status`
/// alone discards the body, which for a JSON API error (Anthropic and
/// Gemini both return a structured `{"error": {...}}` explaining exactly
/// what was wrong - bad model id, malformed schema, auth failure, etc.) is
/// the one piece of information that actually explains a 400/401/403 to
/// the person looking at it instead of just "400 Bad Request".
async fn response_error(response: reqwest::Response) -> anyhow::Error {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    let body = body.trim();

    if body.is_empty() {
        anyhow::anyhow!("HTTP {status}")
    } else {
        anyhow::anyhow!("HTTP {status}: {body}")
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
    fn plain_text_reply_streams_live_immediately() {
        let mut seen = Vec::new();
        let mode = apply_content_delta(
            StreamMode::Deciding(String::new()),
            "Hello there",
            &mut |d: &str| seen.push(d.to_string()),
        );

        assert!(matches!(mode, StreamMode::Live));
        assert_eq!(seen, vec!["Hello there".to_string()]);
    }

    #[test]
    fn reply_starting_with_brace_is_held_back_pending_more_text() {
        let mut seen: Vec<String> = Vec::new();
        let mode =
            apply_content_delta(StreamMode::Deciding(String::new()), "{\"tool\"", &mut |d| {
                seen.push(d.to_string())
            });

        assert!(matches!(mode, StreamMode::HeldBack(_)));
        assert!(
            seen.is_empty(),
            "nothing should stream while still deciding/held back"
        );
    }

    #[test]
    fn held_back_buffer_flushes_once_it_exceeds_the_cap_instead_of_hanging_forever() {
        // Regression test: without a cap, a reply that starts with `{` but
        // never resolves into a real tool call stayed silently buffered for
        // the *entire* remaining generation, so on_delta (which is what
        // stops the REPL's "thinking..." spinner) never fired until the
        // whole response finished - a slow-but-working reply looked
        // completely frozen. This feeds only 5 small chunks (well under
        // what a full reply would be) and asserts the cap has already
        // released them into on_delta by then, rather than needing the
        // caller to send arbitrarily more text before anything shows up.
        let seen: std::cell::RefCell<Vec<String>> = std::cell::RefCell::new(Vec::new());
        let mut mode = StreamMode::Deciding(String::new());

        {
            let mut on_delta = |d: &str| seen.borrow_mut().push(d.to_string());
            mode = apply_content_delta(mode, "{not json", &mut on_delta);
            assert!(matches!(mode, StreamMode::HeldBack(_)));

            // Feed 20-byte chunks, stopping as soon as the cap is crossed,
            // however MAX_HELD_BACK_BYTES is tuned - so on_delta firing
            // more than once here would be a real regression rather than
            // this test just having outlived the flush point.
            let chunk = "x".repeat(20);
            let chunks_needed = MAX_HELD_BACK_BYTES.div_ceil(chunk.len()) + 1;
            for _ in 0..chunks_needed {
                if seen.borrow().is_empty() {
                    mode = apply_content_delta(mode, &chunk, &mut on_delta);
                }
            }
        }

        assert!(matches!(mode, StreamMode::Live));
        let seen = seen.into_inner();
        assert_eq!(
            seen.len(),
            1,
            "on_delta should have fired exactly once, when the cap was crossed"
        );
        assert!(
            seen[0].len() < MAX_HELD_BACK_BYTES + 40,
            "held far more than the cap before flushing"
        );
    }

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
        let chunk: ChatChunk = serde_json::from_str(
            r#"{"choices":[],"usage":{"prompt_tokens":1234,"completion_tokens":7}}"#,
        )
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
    fn reasoning_delta_is_ignored_rather_than_failing_to_parse() {
        // A reasoning model's delta.reasoning/reasoning_content is
        // deliberately not modeled - streaming it raw used to flood a
        // phone terminal with a wall of chain-of-thought text (confirmed
        // on-device). It must still parse cleanly as an unknown field
        // (content stays None) rather than erroring the whole chunk out.
        let chunk: ChatChunk =
            serde_json::from_str(r#"{"choices":[{"delta":{"reasoning":"pondering..."}}]}"#)
                .unwrap();
        assert!(chunk.choices[0].delta.content.is_none());

        let chunk: ChatChunk =
            serde_json::from_str(r#"{"choices":[{"delta":{"reasoning_content":"pondering..."}}]}"#)
                .unwrap();
        assert!(chunk.choices[0].delta.content.is_none());
    }

    #[test]
    fn reasoning_effort_is_omitted_unless_explicitly_set() {
        let request = ChatRequest {
            model: "test-model",
            messages: &[],
            temperature: 0.2,
            max_tokens: 512,
            stream: true,
            cache_prompt: None,
            tools: None,
            tool_choice: None,
            repeat_penalty: None,
            stream_options: StreamOptions {
                include_usage: true,
            },
            reasoning: None,
        };
        let value = serde_json::to_value(&request).unwrap();
        assert!(value.get("reasoning").is_none());

        let request_with_effort = ChatRequest {
            reasoning: Some(json!({ "effort": "low" })),
            ..request
        };
        let value = serde_json::to_value(&request_with_effort).unwrap();
        assert_eq!(value["reasoning"]["effort"], "low");
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

    #[test]
    fn anthropic_conversion_pulls_system_prompt_out() {
        let messages = vec![Message::system("be helpful"), Message::user("hi")];
        let (system, converted) = to_anthropic_messages(&messages);

        assert_eq!(system, Some("be helpful".to_string()));
        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0].role, "user");
    }

    #[test]
    fn anthropic_conversion_merges_consecutive_tool_results_into_one_user_message() {
        // Mirrors what the agent loop actually produces after a multi-tool
        // response: one assistant message with two tool_calls, followed by
        // two separate Role::Tool history entries (one per call).
        let messages = vec![
            Message::user("read both files"),
            Message::assistant_tool_calls(
                None,
                vec![
                    ToolCall {
                        id: "call_1".to_string(),
                        kind: "function".to_string(),
                        function: FunctionCall {
                            name: "read_file".to_string(),
                            arguments: "{\"path\":\"a.txt\"}".to_string(),
                        },
                    },
                    ToolCall {
                        id: "call_2".to_string(),
                        kind: "function".to_string(),
                        function: FunctionCall {
                            name: "read_file".to_string(),
                            arguments: "{\"path\":\"b.txt\"}".to_string(),
                        },
                    },
                ],
            ),
            Message::tool_result("call_1", "contents of a"),
            Message::tool_result("call_2", "contents of b"),
        ];

        let (_, converted) = to_anthropic_messages(&messages);

        // user, assistant(2 tool_use blocks), user(2 tool_result blocks
        // merged) - never two consecutive same-role messages, which
        // Claude's API rejects outright.
        assert_eq!(converted.len(), 3);
        assert_eq!(converted[0].role, "user");
        assert_eq!(converted[1].role, "assistant");
        assert_eq!(converted[1].content.len(), 2);
        assert_eq!(converted[2].role, "user");
        assert_eq!(converted[2].content.len(), 2);
    }

    #[test]
    fn anthropic_conversion_carries_tool_use_id_and_parsed_input() {
        let messages = vec![Message::assistant_tool_calls(
            Some("checking".to_string()),
            vec![ToolCall {
                id: "call_abc".to_string(),
                kind: "function".to_string(),
                function: FunctionCall {
                    name: "read_file".to_string(),
                    arguments: "{\"path\":\"a.rs\"}".to_string(),
                },
            }],
        )];

        let (_, converted) = to_anthropic_messages(&messages);
        assert_eq!(converted[0].content.len(), 2);

        let rendered = serde_json::to_value(&converted[0]).unwrap();
        let blocks = rendered["content"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[0]["text"], "checking");
        assert_eq!(blocks[1]["type"], "tool_use");
        assert_eq!(blocks[1]["id"], "call_abc");
        assert_eq!(blocks[1]["input"]["path"], "a.rs");
    }

    #[test]
    fn anthropic_stream_event_parses_text_delta() {
        let event: AnthropicStreamEvent = serde_json::from_str(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}}"#,
        )
        .unwrap();

        match event {
            AnthropicStreamEvent::ContentBlockDelta {
                delta: AnthropicDelta::TextDelta { text },
                ..
            } => assert_eq!(text, "hi"),
            _ => panic!("expected a text delta"),
        }
    }

    #[test]
    fn anthropic_stream_event_parses_tool_use_start_and_input_delta() {
        let start: AnthropicStreamEvent = serde_json::from_str(
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"read_file","input":{}}}"#,
        )
        .unwrap();
        match start {
            AnthropicStreamEvent::ContentBlockStart {
                index,
                content_block: AnthropicContentBlockStart::ToolUse { id, name },
            } => {
                assert_eq!(index, 1);
                assert_eq!(id, "toolu_1");
                assert_eq!(name, "read_file");
            }
            _ => panic!("expected a tool_use content_block_start"),
        }

        let delta: AnthropicStreamEvent = serde_json::from_str(
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"a.rs\"}"}}"#,
        )
        .unwrap();
        match delta {
            AnthropicStreamEvent::ContentBlockDelta {
                index,
                delta: AnthropicDelta::InputJsonDelta { partial_json },
            } => {
                assert_eq!(index, 1);
                assert_eq!(partial_json, "{\"path\":\"a.rs\"}");
            }
            _ => panic!("expected an input_json_delta"),
        }
    }

    #[test]
    fn anthropic_stream_event_parses_message_start_usage() {
        let event: AnthropicStreamEvent = serde_json::from_str(
            r#"{"type":"message_start","message":{"id":"msg_1","role":"assistant","content":[],"usage":{"input_tokens":42,"output_tokens":1}}}"#,
        )
        .unwrap();

        match event {
            AnthropicStreamEvent::MessageStart { message } => {
                assert_eq!(message.usage.and_then(|u| u.input_tokens), Some(42));
            }
            _ => panic!("expected message_start"),
        }
    }

    #[test]
    fn anthropic_stream_event_ignores_extra_fields_on_unit_variants() {
        // message_delta and message_stop carry extra fields (delta, usage)
        // this crate doesn't model - they must still parse as their unit
        // variant rather than fail deserialization.
        let event: AnthropicStreamEvent = serde_json::from_str(
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":10}}"#,
        )
        .unwrap();
        assert!(matches!(event, AnthropicStreamEvent::MessageDelta));

        let event: AnthropicStreamEvent =
            serde_json::from_str(r#"{"type":"message_stop"}"#).unwrap();
        assert!(matches!(event, AnthropicStreamEvent::MessageStop));

        let event: AnthropicStreamEvent = serde_json::from_str(r#"{"type":"ping"}"#).unwrap();
        assert!(matches!(event, AnthropicStreamEvent::Ping));
    }

    #[test]
    fn anthropic_stream_event_parses_error() {
        let event: AnthropicStreamEvent = serde_json::from_str(
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
        )
        .unwrap();

        match event {
            AnthropicStreamEvent::Error { error } => assert_eq!(error.message, "Overloaded"),
            _ => panic!("expected an error event"),
        }
    }
}
