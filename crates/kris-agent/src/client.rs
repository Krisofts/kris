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

        let response = self
            .http
            .post(url)
            .json(&request)
            .send()
            .await?
            .error_for_status()?
            .json::<ChatResponse>()
            .await?;

        response
            .choices
            .into_iter()
            .next()
            .map(|choice| choice.message.content)
            .ok_or_else(|| anyhow!("llama-server returned an empty response"))
    }
}
