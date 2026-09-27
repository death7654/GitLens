//! Concrete `ModelProvider` for OpenAI and OpenAI-*compatible*
//! chat-completions endpoints — the real OpenAI API, or a local server such
//! as Ollama or LM Studio that exposes the same `/chat/completions` shape.
//! Backs the "OpenAI / OpenAI-compatible" option in the Settings page.
//!
//! Auth/config is supplied at *runtime* via the Settings UI (`set_api_key`
//! Tauri command in `lib.rs`), not read from the environment — like
//! `GeminiProvider`, this provider has no `from_env()`.
//!
//! Structured output: unlike Gemini's `responseSchema`, wide OpenAI-compatible
//! support only really exists for `response_format: {"type": "json_object"}`
//! (accepts *any* JSON, not a specific shape), so — mirroring
//! `BobShellProvider` — the schema is also spelled out in the prompt text
//! itself, and the response is parsed as JSON on our side.
//!
//! Rate limiting: shares the same `RateLimiter` used by `GeminiProvider`,
//! configured independently via `OPENAI_MAX_CONCURRENT_REQUESTS` (default 4)
//! and `OPENAI_MAX_REQUESTS_PER_MINUTE` (default 30) — OpenAI's own limits
//! are typically more generous than Gemini's free tier, hence the higher
//! defaults, but both are safety knobs rather than credentials so they're
//! still fine to configure via the environment.

use std::env;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::provider::{ModelProvider, ModelRequest, ModelResponse};
use crate::rate_limiter::RateLimiter;

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
const DEFAULT_MAX_CONCURRENT_REQUESTS: usize = 4;
const DEFAULT_MAX_REQUESTS_PER_MINUTE: u64 = 30;
const MAX_RETRIES_ON_RATE_LIMIT: u32 = 4;
const BASE_BACKOFF_SECS: u64 = 2;
const MAX_BACKOFF_SECS: u64 = 30;

fn limits_from_env() -> (usize, u64) {
    let max_concurrent = env::var("OPENAI_MAX_CONCURRENT_REQUESTS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_MAX_CONCURRENT_REQUESTS);
    let max_per_minute = env::var("OPENAI_MAX_REQUESTS_PER_MINUTE")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_MAX_REQUESTS_PER_MINUTE);
    (max_concurrent, max_per_minute)
}

/// Concrete [`ModelProvider`] that calls an OpenAI-compatible
/// `/chat/completions` endpoint over HTTP.
#[derive(Debug)]
pub struct OpenAiProvider {
    api_key: String,
    /// No trailing slash — `call()` appends `/chat/completions` directly.
    base_url: String,
    limiter: RateLimiter,
}

impl OpenAiProvider {
    /// Construct with just an API key, targeting the real OpenAI API.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url(api_key, None)
    }

    /// Construct with an API key and an optional base-URL override — this is
    /// what the Settings UI's "Base URL" field (shown only for this provider)
    /// ultimately feeds into via `set_api_key` in `lib.rs`. Use this to point
    /// at a local OpenAI-compatible server, e.g. `http://localhost:11434/v1`
    /// for Ollama. Leave `None` for the real OpenAI API.
    pub fn with_base_url(api_key: impl Into<String>, base_url: Option<String>) -> Self {
        let base_url = base_url
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        let base_url = base_url.trim_end_matches('/').to_string();
        let (max_concurrent, max_per_minute) = limits_from_env();
        Self {
            api_key: api_key.into(),
            base_url,
            limiter: RateLimiter::new(max_concurrent, max_per_minute),
        }
    }
}

// ---------------------------------------------------------------------------
// Wire shapes — the subset of the OpenAI chat-completions REST API we use.
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Debug, Serialize)]
struct ResponseFormat {
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    temperature: f32,
    #[serde(rename = "max_tokens", skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(rename = "response_format", skip_serializing_if = "Option::is_none")]
    response_format: Option<ResponseFormat>,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Option<Vec<Choice>>,
    usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: Option<ChoiceMessage>,
    #[serde(rename = "finish_reason")]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChoiceMessage {
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Usage {
    #[serde(rename = "prompt_tokens")]
    prompt_tokens: Option<u32>,
    #[serde(rename = "completion_tokens")]
    completion_tokens: Option<u32>,
}

#[async_trait]
impl ModelProvider for OpenAiProvider {
    async fn call(&self, req: ModelRequest) -> Result<ModelResponse, String> {
        let url = format!("{}/chat/completions", self.base_url);

        let mut messages = Vec::new();
        if !req.system.is_empty() {
            messages.push(ChatMessage {
                role: "system".to_string(),
                content: req.system.clone(),
            });
        }

        // See module doc comment: schema is spelled out in the prompt itself
        // (not every OpenAI-compatible server supports strict schema
        // enforcement), and `json_object` mode just asks for *some* JSON.
        let mut user_content = req.user.clone();
        let response_format = if let Some(schema) = &req.schema {
            user_content.push_str("\n\nRespond with valid JSON only, matching this schema:\n");
            user_content.push_str(&serde_json::to_string_pretty(schema).unwrap_or_default());
            user_content.push_str("\nReturn only the JSON object — no markdown fences, no explanation.");
            Some(ResponseFormat { kind: "json_object".to_string() })
        } else {
            None
        };
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: user_content,
        });

        let body = ChatRequest {
            model: req.model_id.clone(),
            messages,
            temperature: req.temperature,
            max_tokens: req.max_tokens,
            response_format,
        };

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| format!("failed to build HTTP client: {e}"))?;

        let _permit = self.limiter.acquire().await;

        let mut attempt: u32 = 0;
        loop {
            let response = client
                .post(&url)
                .bearer_auth(&self.api_key)
                .json(&body)
                .send()
                .await
                .map_err(|e| format!("OpenAI request failed: {e}"))?;

            let status = response.status();

            if status.as_u16() == 429 {
                if attempt >= MAX_RETRIES_ON_RATE_LIMIT {
                    let text = response.text().await.unwrap_or_default();
                    let snippet: String = text.chars().take(500).collect();
                    return Err(format!(
                        "OpenAI API returned HTTP 429 (rate limited) after {} retries: {snippet}",
                        MAX_RETRIES_ON_RATE_LIMIT
                    ));
                }

                let retry_after = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .map(Duration::from_secs);

                let backoff = retry_after.unwrap_or_else(|| {
                    let secs = (BASE_BACKOFF_SECS.saturating_mul(1u64 << attempt)).min(MAX_BACKOFF_SECS);
                    Duration::from_secs(secs)
                });

                eprintln!(
                    "[openai] 429 rate limited (attempt {}/{}), backing off {:?}",
                    attempt + 1,
                    MAX_RETRIES_ON_RATE_LIMIT,
                    backoff
                );
                tokio::time::sleep(backoff).await;
                attempt += 1;
                continue;
            }

            if status.as_u16() == 401 || status.as_u16() == 403 {
                let text = response.text().await.unwrap_or_default();
                let snippet: String = text.chars().take(300).collect();
                return Err(format!(
                    "OpenAI API rejected the API key (HTTP {status}). \
                     Double-check the key in Settings. Details: {snippet}"
                ));
            }

            if !status.is_success() {
                let text = response.text().await.unwrap_or_default();
                let snippet: String = text.chars().take(500).collect();
                return Err(format!("OpenAI API returned HTTP {status}: {snippet}"));
            }

            let parsed_resp: ChatResponse = response
                .json()
                .await
                .map_err(|e| format!("failed to parse OpenAI response JSON: {e}"))?;

            let choices = parsed_resp.choices.unwrap_or_default();
            let first = choices.first();

            let text = first
                .and_then(|c| c.message.as_ref())
                .and_then(|m| m.content.clone())
                .ok_or_else(|| {
                    let reason = first
                        .and_then(|c| c.finish_reason.clone())
                        .unwrap_or_else(|| "unknown".to_string());
                    format!("OpenAI response had no message content (finish_reason: {reason})")
                })?;

            let parsed = if req.schema.is_some() {
                serde_json::from_str::<serde_json::Value>(&text).ok()
            } else {
                None
            };

            let (input_tokens, output_tokens) = parsed_resp
                .usage
                .map(|u| (u.prompt_tokens, u.completion_tokens))
                .unwrap_or((None, None));

            return Ok(ModelResponse {
                text,
                parsed,
                model_id: req.model_id,
                input_tokens,
                output_tokens,
                reasoning_text: None,
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_defaults_to_openai_base_url() {
        let p = OpenAiProvider::new("key");
        assert_eq!(p.base_url, DEFAULT_BASE_URL);
    }

    #[test]
    fn with_base_url_trims_trailing_slash() {
        let p = OpenAiProvider::with_base_url("key", Some("http://localhost:11434/v1/".into()));
        assert_eq!(p.base_url, "http://localhost:11434/v1");
    }

    #[test]
    fn with_base_url_falls_back_on_blank_override() {
        let p = OpenAiProvider::with_base_url("key", Some("   ".into()));
        assert_eq!(p.base_url, DEFAULT_BASE_URL);
    }
}