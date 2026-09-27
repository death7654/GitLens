//! Concrete `ModelProvider` for Anthropic's Messages API. Backs the
//! "Anthropic Claude" option in the Settings page.
//!
//! Auth/config is supplied at *runtime* via the Settings UI (`set_api_key`
//! Tauri command in `lib.rs`), not read from the environment — like
//! `GeminiProvider` and `OpenAiProvider`, this provider has no `from_env()`.
//!
//! Docs: <https://docs.anthropic.com/en/api/messages>
//!
//! Structured output: the Messages API has no `response_format` JSON mode,
//! so — mirroring `OpenAiProvider` and `BobShellProvider` — a schema request
//! has its JSON schema spelled out directly in the prompt text, and the
//! response is parsed as JSON on our side. On any parse failure the raw
//! text is still returned so callers can fall back gracefully.
//!
//! Rate limiting: shares the same `RateLimiter` used by the other providers,
//! configured independently via `ANTHROPIC_MAX_CONCURRENT_REQUESTS` (default
//! 3) and `ANTHROPIC_MAX_REQUESTS_PER_MINUTE` (default 20) — these are
//! safety/resource knobs rather than credentials, so (unlike the API key)
//! they're still fine to configure via the environment.

use std::env;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::provider::{ModelProvider, ModelRequest, ModelResponse};
use crate::rate_limiter::RateLimiter;

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_CONCURRENT_REQUESTS: usize = 3;
const DEFAULT_MAX_REQUESTS_PER_MINUTE: u64 = 20;
const MAX_RETRIES_ON_RATE_LIMIT: u32 = 4;
const BASE_BACKOFF_SECS: u64 = 2;
const MAX_BACKOFF_SECS: u64 = 30;
/// The Messages API requires `max_tokens`; `ModelRequest.max_tokens` is
/// optional elsewhere in this codebase, so fall back to a sane default
/// rather than erroring when a caller doesn't set one.
const DEFAULT_MAX_TOKENS: u32 = 1024;

fn limits_from_env() -> (usize, u64) {
    let max_concurrent = env::var("ANTHROPIC_MAX_CONCURRENT_REQUESTS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_MAX_CONCURRENT_REQUESTS);
    let max_per_minute = env::var("ANTHROPIC_MAX_REQUESTS_PER_MINUTE")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_MAX_REQUESTS_PER_MINUTE);
    (max_concurrent, max_per_minute)
}

/// Concrete [`ModelProvider`] that calls Anthropic's `/v1/messages` endpoint
/// over HTTP.
#[derive(Debug)]
pub struct AnthropicProvider {
    api_key: String,
    /// No trailing slash — `call()` appends `/v1/messages` directly.
    base_url: String,
    limiter: RateLimiter,
}

impl AnthropicProvider {
    /// Construct with just an API key, targeting the real Anthropic API.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url(api_key, None)
    }

    /// Construct with an API key and an optional base-URL override (e.g. a
    /// proxy). Leave `None` for the real Anthropic API.
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
// Wire shapes — the subset of the Anthropic Messages API we use.
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct MessagesRequest {
    model: String,
    max_tokens: u32,
    temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<MessageIn>,
}

#[derive(Debug, Serialize)]
struct MessageIn {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct MessagesResponse {
    content: Option<Vec<ContentBlock>>,
    #[serde(rename = "stop_reason")]
    stop_reason: Option<String>,
    usage: Option<UsageInfo>,
}

#[derive(Debug, Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UsageInfo {
    #[serde(rename = "input_tokens")]
    input_tokens: Option<u32>,
    #[serde(rename = "output_tokens")]
    output_tokens: Option<u32>,
}

#[async_trait]
impl ModelProvider for AnthropicProvider {
    async fn call(&self, req: ModelRequest) -> Result<ModelResponse, String> {
        let url = format!("{}/v1/messages", self.base_url);

        // See module doc comment: no response_format JSON mode on this API,
        // so a schema request gets the schema spelled out in the prompt.
        let mut user_content = req.user.clone();
        if let Some(schema) = &req.schema {
            user_content.push_str("\n\nRespond with valid JSON only, matching this schema:\n");
            user_content.push_str(&serde_json::to_string_pretty(schema).unwrap_or_default());
            user_content.push_str("\nReturn only the JSON object — no markdown fences, no explanation.");
        }

        let body = MessagesRequest {
            model: req.model_id.clone(),
            max_tokens: req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
            temperature: req.temperature,
            system: if req.system.is_empty() { None } else { Some(req.system.clone()) },
            messages: vec![MessageIn { role: "user".to_string(), content: user_content }],
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
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", ANTHROPIC_VERSION)
                .json(&body)
                .send()
                .await
                .map_err(|e| format!("Anthropic request failed: {e}"))?;

            let status = response.status();

            // 429 = rate limited; 529 = "overloaded_error" — Anthropic's own
            // docs recommend the same backoff-and-retry treatment for both.
            if status.as_u16() == 429 || status.as_u16() == 529 {
                if attempt >= MAX_RETRIES_ON_RATE_LIMIT {
                    let text = response.text().await.unwrap_or_default();
                    let snippet: String = text.chars().take(500).collect();
                    return Err(format!(
                        "Anthropic API returned HTTP {status} after {} retries: {snippet}",
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
                    "[anthropic] HTTP {status} (attempt {}/{}), backing off {:?}",
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
                    "Anthropic API rejected the API key (HTTP {status}). \
                     Double-check the key in Settings. Details: {snippet}"
                ));
            }

            if !status.is_success() {
                let text = response.text().await.unwrap_or_default();
                let snippet: String = text.chars().take(500).collect();
                return Err(format!("Anthropic API returned HTTP {status}: {snippet}"));
            }

            let parsed_resp: MessagesResponse = response
                .json()
                .await
                .map_err(|e| format!("failed to parse Anthropic response JSON: {e}"))?;

            let blocks = parsed_resp.content.unwrap_or_default();
            let text = blocks
                .iter()
                .find(|b| b.kind == "text")
                .and_then(|b| b.text.clone())
                .ok_or_else(|| {
                    let reason = parsed_resp.stop_reason.clone().unwrap_or_else(|| "unknown".to_string());
                    format!("Anthropic response had no text content block (stop_reason: {reason})")
                })?;

            let parsed = if req.schema.is_some() {
                serde_json::from_str::<serde_json::Value>(&text).ok()
            } else {
                None
            };

            let (input_tokens, output_tokens) = parsed_resp
                .usage
                .map(|u| (u.input_tokens, u.output_tokens))
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
    fn new_defaults_to_anthropic_base_url() {
        let p = AnthropicProvider::new("key");
        assert_eq!(p.base_url, DEFAULT_BASE_URL);
    }

    #[test]
    fn with_base_url_trims_trailing_slash() {
        let p = AnthropicProvider::with_base_url("key", Some("https://proxy.example.com/".into()));
        assert_eq!(p.base_url, "https://proxy.example.com");
    }

    #[test]
    fn with_base_url_falls_back_on_blank_override() {
        let p = AnthropicProvider::with_base_url("key", Some("   ".into()));
        assert_eq!(p.base_url, DEFAULT_BASE_URL);
    }
}