use std::time::Duration;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

use crate::message::Message;

/// Talks to a locally running `llama-server` (llama.cpp's OpenAI-compatible
/// HTTP server) so that inference happens fully offline on-device.
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
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: ChoiceMessage,
}

#[derive(Deserialize)]
struct ChoiceMessage {
    content: String,
}

impl LlamaClient {
    pub fn new(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .unwrap_or_default();

        Self {
            http,
            base_url: base_url.into(),
            model: model.into(),
        }
    }

    pub async fn chat(
        &self,
        messages: &[Message],
        temperature: f32,
        max_tokens: u32,
    ) -> Result<String> {
        let url = format!(
            "{}/v1/chat/completions",
            self.base_url.trim_end_matches('/')
        );

        let request = ChatRequest {
            model: &self.model,
            messages,
            temperature,
            max_tokens,
            stream: false,
        };

        // A connection failure here usually means llama-server briefly
        // starved for CPU/RAM (e.g. a heavy `cargo build` run via
        // run_command) rather than a real crash, so retry a few times with
        // backoff before giving up - this alone resolves most of those
        // blips without the user needing to check health/serve manually.
        const MAX_ATTEMPTS: u32 = 4;
        let mut attempt = 0;

        loop {
            attempt += 1;

            match self.http.post(&url).json(&request).send().await {
                Ok(response) => {
                    let body = response.error_for_status()?.json::<ChatResponse>().await?;

                    return body
                        .choices
                        .into_iter()
                        .next()
                        .map(|choice| choice.message.content)
                        .ok_or_else(|| anyhow!("llama-server returned an empty response"));
                }
                Err(err) if err.is_connect() && attempt < MAX_ATTEMPTS => {
                    tokio::time::sleep(Duration::from_secs(2 * attempt as u64)).await;
                }
                Err(err) => return Err(err.into()),
            }
        }
    }

    /// Checks llama-server's `/health` endpoint with a short timeout, since
    /// this is meant for quick "is it up?" checks, not waiting on inference.
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
