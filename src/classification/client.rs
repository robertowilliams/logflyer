use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::error::AppError;

// ── Client ────────────────────────────────────────────────────────────────────

/// Generic LLM client that speaks either the Anthropic Messages API or the
/// OpenAI Chat Completions API.  Set `api_format` to `"anthropic"` (default)
/// or `"openai"` to control which wire format is used.  Any OpenAI-compatible
/// provider (OpenAI, OpenRouter, Ollama, Groq, LM Studio, Together AI, …) works
/// with `"openai"` format by setting `api_base_url` to the provider's endpoint.
pub struct LlmClient {
    http:       Client,
    api_key:    String,
    model:      String,
    base_url:   String,
    /// `"anthropic"` or `"openai"` (anything else falls back to OpenAI format)
    api_format: String,
}

// ── Anthropic wire types ──────────────────────────────────────────────────────

#[derive(Serialize)]
struct AnthropicRequest<'a> {
    model:      &'a str,
    max_tokens: u32,
    system:     &'a str,
    messages:   Vec<AnthropicMessage<'a>>,
}

#[derive(Serialize)]
struct AnthropicMessage<'a> {
    role:    &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct AnthropicResponse {
    content: Vec<AnthropicContentBlock>,
    usage:   AnthropicUsage,
}

#[derive(Deserialize)]
struct AnthropicContentBlock {
    text: String,
}

#[derive(Deserialize)]
struct AnthropicUsage {
    input_tokens:  u32,
    output_tokens: u32,
}

// ── OpenAI wire types ─────────────────────────────────────────────────────────

#[derive(Serialize)]
struct OpenAiRequest<'a> {
    model:      &'a str,
    max_tokens: u32,
    messages:   Vec<OpenAiMessage<'a>>,
}

#[derive(Serialize)]
struct OpenAiMessage<'a> {
    role:    &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct OpenAiResponse {
    choices: Vec<OpenAiChoice>,
    usage:   OpenAiUsage,
}

#[derive(Deserialize)]
struct OpenAiChoice {
    message: OpenAiMessageContent,
}

#[derive(Deserialize)]
struct OpenAiMessageContent {
    content: String,
}

#[derive(Deserialize)]
struct OpenAiUsage {
    prompt_tokens:     u32,
    completion_tokens: u32,
}

// ── Public API ────────────────────────────────────────────────────────────────

impl LlmClient {
    pub fn new(
        api_key:    String,
        model:      String,
        base_url:   String,
        api_format: String,
    ) -> Result<Self, AppError> {
        let http = Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .map_err(|e| AppError::Classification(format!("failed to build HTTP client: {e}")))?;
        Ok(Self { http, api_key, model, base_url, api_format })
    }

    /// Send a system + user prompt and return `(text, input_tokens, output_tokens)`.
    pub async fn complete(
        &self,
        system:     &str,
        user:       &str,
        max_tokens: u32,
    ) -> Result<(String, u32, u32), AppError> {
        if self.api_format.trim().to_ascii_lowercase() == "anthropic" {
            self.complete_anthropic(system, user, max_tokens).await
        } else {
            self.complete_openai(system, user, max_tokens).await
        }
    }

    // ── Anthropic Messages API ────────────────────────────────────────────────

    async fn complete_anthropic(
        &self,
        system:     &str,
        user:       &str,
        max_tokens: u32,
    ) -> Result<(String, u32, u32), AppError> {
        let base = if self.base_url.is_empty() {
            "https://api.anthropic.com"
        } else {
            self.base_url.trim_end_matches('/')
        };
        let url = format!("{base}/v1/messages");

        let body = AnthropicRequest {
            model:    &self.model,
            max_tokens,
            system,
            messages: vec![AnthropicMessage { role: "user", content: user }],
        };

        let resp = self.http
            .post(&url)
            .header("x-api-key",         &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type",      "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| AppError::Classification(format!("API request failed: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(AppError::Classification(format!("API {status}: {text}")));
        }

        let parsed: AnthropicResponse = resp
            .json()
            .await
            .map_err(|e| AppError::Classification(format!("failed to parse Anthropic response: {e}")))?;

        let text = parsed.content.into_iter().next().map(|b| b.text).unwrap_or_default();
        Ok((text, parsed.usage.input_tokens, parsed.usage.output_tokens))
    }

    // ── OpenAI Chat Completions API ───────────────────────────────────────────

    async fn complete_openai(
        &self,
        system:     &str,
        user:       &str,
        max_tokens: u32,
    ) -> Result<(String, u32, u32), AppError> {
        let base = if self.base_url.is_empty() {
            "https://api.openai.com"
        } else {
            self.base_url.trim_end_matches('/')
        };
        let url = format!("{base}/v1/chat/completions");

        let body = OpenAiRequest {
            model: &self.model,
            max_tokens,
            messages: vec![
                OpenAiMessage { role: "system",  content: system },
                OpenAiMessage { role: "user",    content: user   },
            ],
        };

        let resp = self.http
            .post(&url)
            .header("Authorization",  format!("Bearer {}", self.api_key))
            .header("content-type",   "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| AppError::Classification(format!("API request failed: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(AppError::Classification(format!("API {status}: {text}")));
        }

        let parsed: OpenAiResponse = resp
            .json()
            .await
            .map_err(|e| AppError::Classification(format!("failed to parse OpenAI response: {e}")))?;

        let text = parsed.choices.into_iter()
            .next()
            .map(|c| c.message.content)
            .unwrap_or_default();
        let input  = parsed.usage.prompt_tokens;
        let output = parsed.usage.completion_tokens;
        Ok((text, input, output))
    }
}
