//! End-to-end test of client.rs + agent.rs against a hand-rolled mock
//! HTTP/SSE server standing in for llama-server, so the streaming parser,
//! tool-call accumulation, and tool execution loop can be exercised
//! without needing a real llama.cpp build or GGUF model.

use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kris::agent::{Agent, Project};
use kris::client::{Backend, ModelClient};
use kris::message::{Message, Role};
use kris::tools::ToolRegistry;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const TOOL_CALL_SSE: &str = concat!(
    "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"tool_calls\":",
    "[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",",
    "\"function\":{\"name\":\"read_file\",\"arguments\":\"\"}}]}}]}\n\n",
    "data: {\"choices\":[{\"delta\":{\"tool_calls\":",
    "[{\"index\":0,\"function\":{\"arguments\":\"{\\\"path\\\": \\\"a.txt\\\"}\"}}]}}]}\n\n",
    "data: [DONE]\n\n",
);

const FINAL_ANSWER_SSE: &str = concat!(
    "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"The file says hi.\"}}]}\n\n",
    "data: [DONE]\n\n",
);

const SUMMARY_SSE: &str = concat!(
    "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"Summary: did X, next is Y.\"}}]}\n\n",
    "data: [DONE]\n\n",
);

// Mirrors a real, on-device observation: without a working tool-calling
// grammar, a model can hallucinate a call to a tool name that was never
// registered (here, "hello" - echoing back the user's greeting) instead of
// using the native tool_calls field.
const HALLUCINATED_TOOL_SSE: &str = concat!(
    "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",",
    "\"content\":\"{\\\"tool\\\": \\\"hello\\\", \\\"args\\\": {}}\"}}]}\n\n",
    "data: [DONE]\n\n",
);

const ANTHROPIC_TOOL_CALL_SSE: &str = concat!(
    "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"role\":\"assistant\",\"content\":[],\"usage\":{\"input_tokens\":50,\"output_tokens\":1}}}\n\n",
    "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"read_file\",\"input\":{}}}\n\n",
    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\": \\\"a.txt\\\"}\"}}\n\n",
    "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":10}}\n\n",
    "data: {\"type\":\"message_stop\"}\n\n",
);

const ANTHROPIC_FINAL_ANSWER_SSE: &str = concat!(
    "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_2\",\"role\":\"assistant\",\"content\":[],\"usage\":{\"input_tokens\":80,\"output_tokens\":1}}}\n\n",
    "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"The file says hi.\"}}\n\n",
    "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":6}}\n\n",
    "data: {\"type\":\"message_stop\"}\n\n",
);

async fn spawn_mock_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let chat_calls = Arc::new(AtomicUsize::new(0));

    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let chat_calls = chat_calls.clone();
            tokio::spawn(handle_connection(stream, chat_calls));
        }
    });

    format!("http://{addr}")
}

async fn handle_connection(mut stream: TcpStream, chat_calls: Arc<AtomicUsize>) {
    let header_text = read_request_headers(&mut stream).await;
    let path = header_text
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .nth(1)
        .unwrap_or("")
        .to_string();

    match path.as_str() {
        "/health" => respond(&mut stream, "application/json", "{}").await,
        "/tokenize" => respond(&mut stream, "application/json", r#"{"tokens":[1,2,3]}"#).await,
        "/v1/chat/completions" => {
            let call_index = chat_calls.fetch_add(1, Ordering::SeqCst);
            let body = if call_index == 0 {
                TOOL_CALL_SSE
            } else {
                FINAL_ANSWER_SSE
            };
            respond(&mut stream, "text/event-stream", body).await;
        }
        "/v1/messages" => {
            let call_index = chat_calls.fetch_add(1, Ordering::SeqCst);
            let body = if call_index == 0 {
                ANTHROPIC_TOOL_CALL_SSE
            } else {
                ANTHROPIC_FINAL_ANSWER_SSE
            };
            respond(&mut stream, "text/event-stream", body).await;
        }
        other => panic!("mock server got an unexpected request path: {other}"),
    }
}

/// Reads headers up to `\r\n\r\n`, then drains exactly `Content-Length`
/// more bytes of body (KRIS always sends one) before returning - so the
/// client doesn't see the connection close mid-request.
async fn read_request_headers(stream: &mut TcpStream) -> String {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];

    let headers_end = loop {
        let n = stream.read(&mut chunk).await.expect("read request");
        assert!(n > 0, "connection closed before headers were fully sent");
        buf.extend_from_slice(&chunk[..n]);

        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            break pos;
        }
    };

    let header_text = String::from_utf8_lossy(&buf[..headers_end]).to_string();

    let content_length: usize = header_text
        .lines()
        .find_map(|line| {
            let lower = line.to_ascii_lowercase();
            lower
                .strip_prefix("content-length:")
                .map(|v| v.trim().to_string())
        })
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let body_start = headers_end + 4;
    while buf.len() < body_start + content_length {
        let n = stream.read(&mut chunk).await.expect("read body");
        assert!(n > 0, "connection closed before body was fully sent");
        buf.extend_from_slice(&chunk[..n]);
    }

    header_text
}

/// Like `read_request_headers`, but also returns the request body - for
/// tests that need to inspect what was actually sent (e.g. confirming a
/// project's KRIS.md conventions made it into the system prompt).
async fn read_request_headers_and_body(stream: &mut TcpStream) -> (String, String) {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];

    let headers_end = loop {
        let n = stream.read(&mut chunk).await.expect("read request");
        assert!(n > 0, "connection closed before headers were fully sent");
        buf.extend_from_slice(&chunk[..n]);

        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            break pos;
        }
    };

    let header_text = String::from_utf8_lossy(&buf[..headers_end]).to_string();

    let content_length: usize = header_text
        .lines()
        .find_map(|line| {
            let lower = line.to_ascii_lowercase();
            lower
                .strip_prefix("content-length:")
                .map(|v| v.trim().to_string())
        })
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let body_start = headers_end + 4;
    while buf.len() < body_start + content_length {
        let n = stream.read(&mut chunk).await.expect("read body");
        assert!(n > 0, "connection closed before body was fully sent");
        buf.extend_from_slice(&chunk[..n]);
    }

    let body = String::from_utf8_lossy(&buf[body_start..body_start + content_length]).to_string();
    (header_text, body)
}

async fn respond(stream: &mut TcpStream, content_type: &str, body: &str) {
    respond_with_status(stream, 200, "OK", content_type, body).await;
}

async fn respond_with_status(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    content_type: &str,
    body: &str,
) {
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len(),
    );
    stream
        .write_all(response.as_bytes())
        .await
        .expect("write response");
    let _ = stream.shutdown().await;
}

/// A single-route mock server that always answers `/v1/messages` with the
/// given status/body, for testing how KRIS surfaces a rejected request
/// (e.g. an invalid model id) rather than a successful stream.
async fn spawn_error_server(status: u16, body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let _ = read_request_headers(&mut stream).await;
                respond_with_status(&mut stream, status, "Bad Request", "application/json", body)
                    .await;
            });
        }
    });

    format!("http://{addr}")
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Answers the first request with a normal tool call, then drops the
/// connection with no response at all on the second - standing in for
/// llama-server getting reaped, or a flaky network, partway through a
/// multi-iteration turn.
async fn spawn_tool_call_then_drop_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let chat_calls = Arc::new(AtomicUsize::new(0));

    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let chat_calls = chat_calls.clone();
            tokio::spawn(async move {
                let _ = read_request_headers(&mut stream).await;
                if chat_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    respond(&mut stream, "text/event-stream", TOOL_CALL_SSE).await;
                } else {
                    let _ = stream.shutdown().await;
                }
            });
        }
    });

    format!("http://{addr}")
}

/// A mock server that never gives a final answer - every request gets a
/// distinct tool call (a different file path each time, so it never
/// collides with the "same call proposed twice" early-stop check) - so
/// the agent loop is guaranteed to run all the way to `max_iterations`.
async fn spawn_endless_tool_call_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let chat_calls = Arc::new(AtomicUsize::new(0));

    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(handle_endless_tool_call_connection(
                stream,
                chat_calls.clone(),
            ));
        }
    });

    format!("http://{addr}")
}

async fn handle_endless_tool_call_connection(mut stream: TcpStream, chat_calls: Arc<AtomicUsize>) {
    let header_text = read_request_headers(&mut stream).await;
    let path = header_text
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .nth(1)
        .unwrap_or("");

    match path {
        "/health" => respond(&mut stream, "application/json", "{}").await,
        "/v1/chat/completions" => {
            let index = chat_calls.fetch_add(1, Ordering::SeqCst);
            let payload = serde_json::json!({
                "choices": [{
                    "delta": {
                        "role": "assistant",
                        "tool_calls": [{
                            "index": 0,
                            "id": format!("call_{index}"),
                            "type": "function",
                            "function": {
                                "name": "read_file",
                                "arguments": format!("{{\"path\": \"file_{index}.txt\"}}"),
                            }
                        }]
                    }
                }]
            });
            let body = format!("data: {payload}\n\ndata: [DONE]\n\n");
            respond(&mut stream, "text/event-stream", &body).await;
        }
        other => panic!("mock server got an unexpected request path: {other}"),
    }
}

async fn spawn_single_response_server(body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let header_text = read_request_headers(&mut stream).await;
                let path = header_text
                    .lines()
                    .next()
                    .unwrap_or("")
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("");

                match path {
                    "/health" => respond(&mut stream, "application/json", "{}").await,
                    "/v1/chat/completions" => respond(&mut stream, "text/event-stream", body).await,
                    other => panic!("mock server got an unexpected request path: {other}"),
                }
            });
        }
    });

    format!("http://{addr}")
}

#[tokio::test]
async fn agent_streams_a_tool_call_then_a_final_answer() {
    let base_url = spawn_mock_server().await;

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "hi").unwrap();

    let client = ModelClient::new(base_url, "test-model".to_string(), Backend::Llama, None);
    let agent = Agent::new(
        client,
        ToolRegistry::with_defaults(false, false),
        0.2,
        512,
        8192,
    );

    let mut history: Vec<Message> = Vec::new();
    let mut tool_calls_seen: Vec<(String, String)> = Vec::new();
    let mut streamed_text = String::new();

    let answer = agent
        .run(
            &mut history,
            Project {
                root: dir.path(),
                name: "test-project",
                type_hint: "",
            },
            "please read a.txt",
            5,
            |delta| streamed_text.push_str(delta),
            |name, _args, result| tool_calls_seen.push((name.to_string(), result.to_string())),
            |_| {},
        )
        .await
        .expect("agent turn should succeed against the mock server");

    assert_eq!(answer, "The file says hi.");
    assert!(streamed_text.contains("The file says hi."));

    assert_eq!(tool_calls_seen.len(), 1);
    assert_eq!(tool_calls_seen[0].0, "read_file");
    assert!(tool_calls_seen[0].1.contains("hi"));

    // History should carry the full turn: system, user, assistant(tool_calls),
    // tool result, assistant(final text) - so a follow-up turn has the
    // right context.
    assert_eq!(history.len(), 5);
}

#[tokio::test]
async fn on_activity_reports_the_tool_name_while_its_arguments_are_still_streaming() {
    // Regression test: the REPL's spinner used to show only a generic
    // rotating verb ("Thinking...") for the entire wait, with no signal
    // for what the model was actually doing behind the scenes. on_activity
    // should fire with the tool's name as soon as it's known - before
    // on_tool_call, which only fires once the tool has actually run.
    let base_url = spawn_mock_server().await;

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "hi").unwrap();

    let client = ModelClient::new(base_url, "test-model".to_string(), Backend::Llama, None);
    let agent = Agent::new(
        client,
        ToolRegistry::with_defaults(false, false),
        0.2,
        512,
        8192,
    );

    let mut history: Vec<Message> = Vec::new();
    let mut activity_seen: Vec<String> = Vec::new();
    let tool_call_seen = std::cell::Cell::new(false);

    agent
        .run(
            &mut history,
            Project {
                root: dir.path(),
                name: "test-project",
                type_hint: "",
            },
            "please read a.txt",
            5,
            |_delta| {},
            |_name, _args, _result| {
                tool_call_seen.set(true);
            },
            |name| {
                // Confirms on_activity fires before the tool actually runs,
                // not just alongside/after it.
                assert!(
                    !tool_call_seen.get(),
                    "on_activity should fire before on_tool_call"
                );
                activity_seen.push(name.to_string());
            },
        )
        .await
        .expect("agent turn should succeed against the mock server");

    assert!(activity_seen.iter().any(|name| name == "read_file"));
    assert!(tool_call_seen.get());
}

#[tokio::test]
async fn connection_failure_after_progress_keeps_history_instead_of_rolling_it_back() {
    // Regression test: a request failure used to always roll the *whole*
    // turn back out of history via `history.truncate(turn_start)` - fine
    // when nothing had happened yet, but if llama-server got reaped (or the
    // network dropped) after a tool call had already completed this turn,
    // it discarded that real progress too, forcing a retry to redo the
    // whole task from scratch instead of continuing from where it left off.
    let base_url = spawn_tool_call_then_drop_server().await;

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "hi").unwrap();

    let client = ModelClient::new(base_url, "test-model".to_string(), Backend::Llama, None);
    let agent = Agent::new(
        client,
        ToolRegistry::with_defaults(false, false),
        0.2,
        512,
        8192,
    );

    let mut history: Vec<Message> = Vec::new();

    agent
        .run(
            &mut history,
            Project {
                root: dir.path(),
                name: "test-project",
                type_hint: "",
            },
            "please read a.txt",
            5,
            |_delta| {},
            |_name, _args, _result| {},
            |_| {},
        )
        .await
        .expect_err("the second request should fail once the connection is dropped");

    // The first iteration's tool call/result must survive: system, user,
    // assistant(tool_calls), tool result - not rolled back to empty.
    assert_eq!(history.len(), 4);
    assert_eq!(history[2].role, Role::Assistant);
    assert_eq!(history[3].role, Role::Tool);
}

#[tokio::test]
async fn agent_streams_a_tool_call_then_a_final_answer_via_claude() {
    let base_url = spawn_mock_server().await;

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "hi").unwrap();

    let client = ModelClient::new(
        base_url,
        "claude-sonnet-5".to_string(),
        Backend::Anthropic,
        Some("test-key".to_string()),
    );
    let agent = Agent::new(
        client,
        ToolRegistry::with_defaults(false, false),
        0.2,
        512,
        200_000,
    );

    let mut history: Vec<Message> = Vec::new();
    let mut tool_calls_seen: Vec<(String, String)> = Vec::new();
    let mut streamed_text = String::new();

    let answer = agent
        .run(
            &mut history,
            Project {
                root: dir.path(),
                name: "test-project",
                type_hint: "",
            },
            "please read a.txt",
            5,
            |delta| streamed_text.push_str(delta),
            |name, _args, result| tool_calls_seen.push((name.to_string(), result.to_string())),
            |_| {},
        )
        .await
        .expect("agent turn should succeed against the mock Claude server");

    assert_eq!(answer, "The file says hi.");
    assert!(streamed_text.contains("The file says hi."));

    assert_eq!(tool_calls_seen.len(), 1);
    assert_eq!(tool_calls_seen[0].0, "read_file");
    assert!(tool_calls_seen[0].1.contains("hi"));
    assert_eq!(history.len(), 5);
}

#[tokio::test]
async fn a_rejected_request_surfaces_the_provider_error_body() {
    // Regression test: a bare `.error_for_status()` discards the response
    // body, so a 400 from Claude/Gemini explaining exactly what was wrong
    // (bad model id, malformed schema, ...) used to come through to the
    // user as a content-free "400 Bad Request". The error KRIS surfaces
    // must include that body.
    let body = r#"{"type":"error","error":{"type":"invalid_request_error","message":"model: claude-sonnet-5 is not supported"}}"#;
    let base_url = spawn_error_server(400, body).await;

    let client = ModelClient::new(
        base_url,
        "claude-sonnet-5".to_string(),
        Backend::Anthropic,
        Some("test-key".to_string()),
    );

    let err = client
        .chat_stream(&[Message::user("hi")], None, 0.2, 512, |_| {}, |_| {})
        .await
        .expect_err("a 400 response should surface as an error");

    let message = err.to_string();
    assert!(message.contains("400"));
    assert!(message.contains("claude-sonnet-5 is not supported"));
}

#[tokio::test]
async fn truncated_reasoning_reply_surfaces_a_diagnostic_instead_of_silence() {
    // Regression test: a reasoning model can spend its entire max_tokens
    // budget on a hidden "thinking" field this client never parses, so
    // delta.content stays empty for the whole stream and the request ends
    // with finish_reason "length" - on-device this showed up as OpenRouter's
    // free tencent/hy3:free model answering a short message with nothing at
    // all, indistinguishable from a crash. It must come back as a visible
    // diagnostic instead of a silent empty reply.
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    let base_url = spawn_single_response_server(body).await;

    let client = ModelClient::new(base_url, "test-model".to_string(), Backend::Llama, None);

    let mut streamed = String::new();
    let outcome = client
        .chat_stream(
            &[Message::user("hi")],
            None,
            0.2,
            16,
            |delta| streamed.push_str(delta),
            |_| {},
        )
        .await
        .expect("a truncated-with-no-content stream should still succeed");

    let content = outcome
        .content
        .expect("should synthesize a diagnostic note instead of None");
    assert!(content.contains("max_tokens"));
    assert!(streamed.contains("max_tokens"));
}

#[tokio::test]
async fn reasoning_trace_is_not_dumped_to_the_terminal() {
    // A reasoning model (e.g. Tencent's Hy3 via OpenRouter) streams its
    // hidden "thinking" separately from the real answer. On-device, an
    // earlier version of this streamed that raw chain-of-thought straight
    // to the terminal, which flooded a phone screen with a wall of text
    // for a genuinely long reasoning phase. It must not be streamed at
    // all - the REPL's spinner already covers "still working" - and must
    // never leak into the final content either.
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"reasoning\":\"let me check the file\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"It says hi.\"}}]}\n\n",
        "data: [DONE]\n\n",
    );
    let base_url = spawn_single_response_server(body).await;

    let client = ModelClient::new(base_url, "test-model".to_string(), Backend::Llama, None);

    let mut streamed = String::new();
    let outcome = client
        .chat_stream(
            &[Message::user("hi")],
            None,
            0.2,
            512,
            |delta| streamed.push_str(delta),
            |_| {},
        )
        .await
        .expect("stream should succeed");

    assert!(!streamed.contains("let me check the file"));
    assert_eq!(streamed, "It says hi.");
    assert_eq!(outcome.content.as_deref(), Some("It says hi."));
}

#[tokio::test]
async fn done_marker_ends_the_stream_even_if_the_connection_stays_open() {
    // Regression test for an on-device hang: chat_stream_openai used to
    // rely entirely on the connection itself closing (byte_stream.next()
    // returning None) to know a reply was finished - the SSE "[DONE]"
    // event was seen but only caused a `continue`, not a break. A server/
    // proxy that sends "[DONE]" and then leaves an idle keep-alive
    // connection open (no Content-Length, no closing handshake) left the
    // client waiting forever for more bytes that were never coming - a
    // total, unrecoverable freeze right after the visible answer text,
    // with no error and no way out short of restarting KRIS.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let _ = read_request_headers(&mut stream).await;

        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"hi\"}}]}\n\n",
            "data: [DONE]\n\n",
        );
        // Deliberately no Content-Length/Transfer-Encoding and no
        // shutdown() afterward - the connection is just left open, as if
        // kept alive for a future request.
        let response = format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n{body}");
        let _ = stream.write_all(response.as_bytes()).await;
        tokio::time::sleep(Duration::from_secs(600)).await;
    });

    let base_url = format!("http://{addr}");
    let client = ModelClient::new(base_url, "test-model".to_string(), Backend::Llama, None);

    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        client.chat_stream(&[Message::user("hi")], None, 0.2, 512, |_| {}, |_| {}),
    )
    .await
    .expect(
        "chat_stream should return as soon as [DONE] arrives, not hang waiting for the \
         connection to close",
    )
    .expect("stream should succeed");

    assert_eq!(outcome.content.as_deref(), Some("hi"));
}

#[tokio::test]
async fn a_stream_that_goes_silent_times_out_instead_of_hanging_forever() {
    // Regression test for an on-device symptom: a large local model
    // decoding slowly on a phone CPU could still be making steady
    // progress well past any reasonable "this looks stuck" cutoff - a
    // blanket whole-request timeout used to kill it regardless, which got
    // misread as a dropped connection and auto-retried, sending a fresh
    // request that hit the exact same wall - visible in llama-server's own
    // log as one cancelled task after another, forever. There must still
    // be *some* bound, though, for a connection that goes genuinely silent
    // (no bytes at all) rather than just slow - confirmed here with a
    // short inactivity timeout so the test itself doesn't have to wait out
    // the real, deliberately generous production value.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let _ = read_request_headers(&mut stream).await;

        // Sends a real chunk (so the client knows the request succeeded
        // and starts reading the stream), then goes completely silent -
        // no more bytes, no close - standing in for a response that's
        // still "in progress" from the connection's point of view but has
        // stopped producing anything at all.
        let body =
            "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"star\"}}]}\n\n";
        let response = format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n{body}");
        let _ = stream.write_all(response.as_bytes()).await;
        tokio::time::sleep(Duration::from_secs(600)).await;
    });

    let base_url = format!("http://{addr}");
    let client = ModelClient::new(base_url, "test-model".to_string(), Backend::Llama, None)
        .with_stream_inactivity_timeout(Duration::from_millis(200));

    let err = tokio::time::timeout(
        Duration::from_secs(5),
        client.chat_stream(&[Message::user("hi")], None, 0.2, 512, |_| {}, |_| {}),
    )
    .await
    .expect("a genuinely silent stream should time out well before the test's own 5s bound")
    .expect_err("a silent stream should surface as an error, not a successful empty reply");

    assert!(err.to_string().contains("no response"));
}

#[tokio::test]
async fn reaching_max_iterations_persists_the_notice_and_invites_a_continue() {
    // Regression test: previously, when the model never produced a final
    // answer within max_iterations, the "reached the maximum" notice was
    // only streamed live via on_delta - it was never pushed into `history`,
    // leaving the turn dangling right after an unanswered tool result
    // instead of closing normally. That left the next turn's history in an
    // inconsistent state (no assistant message ever closed the loop).
    let base_url = spawn_endless_tool_call_server().await;
    let dir = tempfile::tempdir().unwrap();

    let client = ModelClient::new(base_url, "test-model".to_string(), Backend::Llama, None);
    let agent = Agent::new(
        client,
        ToolRegistry::with_defaults(false, false),
        0.2,
        512,
        8192,
    );

    let mut history: Vec<Message> = Vec::new();

    let answer = agent
        .run(
            &mut history,
            Project {
                root: dir.path(),
                name: "test-project",
                type_hint: "",
            },
            "keep reading files forever",
            3,
            |_delta| {},
            |_name, _args, _result| {},
            |_| {},
        )
        .await
        .expect("hitting max_iterations should still be a successful turn, not an error");

    assert!(answer.contains("maximum number of tool calls"));
    assert!(answer.to_lowercase().contains("continue"));

    let last = history.last().expect("history should not be empty");
    assert_eq!(last.role, Role::Assistant);
    assert_eq!(last.content.as_deref(), Some(answer.as_str()));
}

#[tokio::test]
async fn agent_ignores_hallucinated_call_to_a_nonexistent_tool() {
    // Regression test for an on-device observation: without a working
    // tool-calling grammar, a local model can hallucinate a call to a tool
    // name that was never registered instead of using the native
    // tool_calls field or answering normally. Executing that call just
    // produces an "unknown tool" error the model then rambles about -
    // worse than simply showing its raw text as the answer.
    let base_url = spawn_single_response_server(HALLUCINATED_TOOL_SSE).await;

    let dir = tempfile::tempdir().unwrap();
    let client = ModelClient::new(base_url, "test-model".to_string(), Backend::Llama, None);
    let agent = Agent::new(
        client,
        ToolRegistry::with_defaults(false, false),
        0.2,
        512,
        8192,
    );

    let mut history: Vec<Message> = Vec::new();
    let mut tool_calls_seen: Vec<(String, String)> = Vec::new();

    let answer = agent
        .run(
            &mut history,
            Project {
                root: dir.path(),
                name: "test-project",
                type_hint: "",
            },
            "halo",
            5,
            |_delta| {},
            |name, _args, result| tool_calls_seen.push((name.to_string(), result.to_string())),
            |_| {},
        )
        .await
        .expect("agent turn should succeed even with a hallucinated tool name");

    assert!(
        tool_calls_seen.is_empty(),
        "a call to a nonexistent tool name should never actually be executed"
    );
    assert!(
        answer.contains("hello"),
        "the model's raw text should still be shown as the answer: {answer}"
    );
}

#[tokio::test]
async fn summarize_returns_the_models_plain_text_recap_for_the_compact_command() {
    let base_url = spawn_single_response_server(SUMMARY_SSE).await;

    let client = ModelClient::new(base_url, "test-model".to_string(), Backend::Llama, None);
    let agent = Agent::new(
        client,
        ToolRegistry::with_defaults(false, false),
        0.2,
        512,
        8192,
    );

    let history = vec![
        Message::user("tolong perbaiki bug X"),
        Message::assistant_text("sudah diperbaiki"),
    ];
    let mut streamed = String::new();

    let summary = agent
        .summarize(&history, "", |delta: &str| streamed.push_str(delta))
        .await
        .expect("summarize should succeed against a normal plain-text reply");

    assert_eq!(summary, "Summary: did X, next is Y.");
    assert_eq!(
        streamed, summary,
        "the recap should stream live via on_delta just like a normal answer"
    );
}

#[tokio::test]
async fn project_kris_md_conventions_are_folded_into_the_system_prompt() {
    // A project's own KRIS.md is meant to actually steer the model every
    // session, not just sit there as a file it has to remember to
    // read_file on its own initiative - Agent::run folds it straight into
    // the system prompt on the first turn.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let captured_body: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let captured_clone = captured_body.clone();

    tokio::spawn(async move {
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };
        let (_, body) = read_request_headers_and_body(&mut stream).await;
        *captured_clone.lock().unwrap() = body;
        respond(&mut stream, "text/event-stream", FINAL_ANSWER_SSE).await;
    });

    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("KRIS.md"), "Always write tests first.").unwrap();

    let base_url = format!("http://{addr}");
    let client = ModelClient::new(base_url, "test-model".to_string(), Backend::Llama, None);
    let agent = Agent::new(
        client,
        ToolRegistry::with_defaults(false, false),
        0.2,
        512,
        8192,
    );

    let mut history: Vec<Message> = Vec::new();
    agent
        .run(
            &mut history,
            Project {
                root: dir.path(),
                name: "test-project",
                type_hint: "",
            },
            "halo",
            5,
            |_delta| {},
            |_name, _args, _result| {},
            |_| {},
        )
        .await
        .expect("agent turn should succeed");

    let body = captured_body.lock().unwrap().clone();
    assert!(
        body.contains("Always write tests first."),
        "KRIS.md content should be folded into the request's system prompt: {body}"
    );
}
