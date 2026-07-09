//! End-to-end test of client.rs + agent.rs against a hand-rolled mock
//! HTTP/SSE server standing in for llama-server, so the streaming parser,
//! tool-call accumulation, and tool execution loop can be exercised
//! without needing a real llama.cpp build or GGUF model.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use kris::agent::{Agent, Project};
use kris::client::{Backend, ModelClient};
use kris::message::Message;
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

async fn respond(stream: &mut TcpStream, content_type: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len(),
    );
    stream
        .write_all(response.as_bytes())
        .await
        .expect("write response");
    let _ = stream.shutdown().await;
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[tokio::test]
async fn agent_streams_a_tool_call_then_a_final_answer() {
    let base_url = spawn_mock_server().await;

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "hi").unwrap();

    let client = ModelClient::new(base_url, "test-model".to_string(), Backend::Llama, None);
    let agent = Agent::new(client, ToolRegistry::with_defaults(false), 0.2, 512, 8192);

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
