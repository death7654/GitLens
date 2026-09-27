//! Person 6 — Concrete `ModelProvider` backed directly by the Google Gemini
//! API (the "Generative Language API"), replacing the Bob Shell CLI wrapper.
//!
//! Auth: the API key is supplied at *runtime*, via the Settings page in the
//! app (see the `set_api_key` Tauri command in `lib.rs`) — not read from the
//! environment. There is deliberately no `from_env()` here; until the user
//! enters a key, `lib.rs` falls back to `StubProvider`, which returns a
//! clear "no provider configured" error on every call.
//!
//! Docs: <https://ai.google.dev/gemini-api/docs>
//!
//! Structured output: when `ModelRequest.schema` is `Some`, we ask Gemini
//! for JSON directly via `generationConfig.responseMimeType` +
//! `responseSchema`, and parse the returned text as JSON into
//! `ModelResponse::parsed`. On any parse failure the raw text is still
//! returned so callers can fall back gracefully.
//!
//! Rate limiting: every caller in this codebase (per-subsystem file/commit
//! summaries, the ranking call, and parallel per-stop narration calls) goes
//! through a single shared provider instance (see `ProviderState` in
//! `lib.rs`), so limiting is centralised via `RateLimiter` rather than
//! duplicated at each call site:
//!   - `GEMINI_MAX_CONCURRENT_REQUESTS` (default `2`) caps how many requests
//!     may be in flight at once.
//!   - `GEMINI_MAX_REQUESTS_PER_MINUTE` (default `10`) caps the sustained
//!     rate, by enforcing a minimum spacing between request starts.
//! These are resource/safety knobs, not credentials, so (unlike the API key)
//! they're still fine to configure via the environment.
//! If a `429` slips through anyway, `call` retries with backoff (honouring
//! the response's `Retry-After` header when present) before giving up.

use std::env;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::provider::{ModelProvider, ModelRequest, ModelResponse};
use crate::rate_limiter::RateLimiter;

const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";
const DEFAULT_MAX_CONCURRENT_REQUESTS: usize = 2;
const DEFAULT_MAX_REQUESTS_PER_MINUTE: u64 = 10;
/// How many times to retry a 429 before giving up, on top of the first try.
const MAX_RETRIES_ON_RATE_LIMIT: u32 = 4;
/// Backoff used when the API gives no `Retry-After` header: 2s, 4s, 8s, 16s.
const BASE_BACKOFF_SECS: u64 = 2;
const MAX_BACKOFF_SECS: u64 = 30;

fn limits_from_env() -> (usize, u64) {
    let max_concurrent = env::var("GEMINI_MAX_CONCURRENT_REQUESTS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_MAX_CONCURRENT_REQUESTS);
    let max_per_minute = env::var("GEMINI_MAX_REQUESTS_PER_MINUTE")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_MAX_REQUESTS_PER_MINUTE);
    (max_concurrent, max_per_minute)
}

/// Concrete [`ModelProvider`] that calls the Gemini API over HTTP, with
/// built-in concurrency capping and request spacing to stay under Gemini's
/// per-minute rate limits.
#[derive(Debug)]
pub struct GeminiProvider {
    api_key: String,
    base_url: String,
    limiter: RateLimiter,
}

impl GeminiProvider {
    /// Construct with just an API key — base URL and rate limits take their
    /// defaults (`GEMINI_BASE_URL` / `GEMINI_MAX_*` env vars if set, else the
    /// hardcoded defaults above).
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url(api_key, None)
    }

    /// Construct with an API key and an optional base-URL override — this is
    /// what the Settings UI's "Save & apply" button ultimately calls via
    /// `set_api_key` in `lib.rs`. `base_url` is only meaningful for a proxy
    /// or regional endpoint; leave it `None` for the standard Gemini API.
    pub fn with_base_url(api_key: impl Into<String>, base_url: Option<String>) -> Self {
        let base_url = base_url
            .filter(|s| !s.trim().is_empty())
            .or_else(|| env::var("GEMINI_BASE_URL").ok())
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        let (max_concurrent, max_per_minute) = limits_from_env();
        Self {
            api_key: api_key.into(),
            base_url,
            limiter: RateLimiter::new(max_concurrent, max_per_minute),
        }
    }
}

// ---------------------------------------------------------------------------
// Wire shapes — the subset of the Gemini `generateContent` REST API we use.
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct GenerateContentRequest {
    contents: Vec<Content>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<Content>,
    #[serde(rename = "generationConfig")]
    generation_config: GenerationConfig,
}

#[derive(Debug, Serialize)]
struct Content {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    parts: Vec<Part>,
}

#[derive(Debug, Serialize)]
struct Part {
    text: String,
}

#[derive(Debug, Serialize)]
struct GenerationConfig {
    temperature: f32,
    #[serde(rename = "maxOutputTokens", skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u32>,
    #[serde(rename = "responseMimeType", skip_serializing_if = "Option::is_none")]
    response_mime_type: Option<String>,
    #[serde(rename = "responseSchema", skip_serializing_if = "Option::is_none")]
    response_schema: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct GenerateContentResponse {
    candidates: Option<Vec<CandidateResp>>,
}

#[derive(Debug, Deserialize)]
struct CandidateResp {
    content: Option<ContentResp>,
    #[serde(rename = "finishReason")]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ContentResp {
    parts: Option<Vec<PartResp>>,
}

#[derive(Debug, Deserialize)]
struct PartResp {
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UsageMetadata {
    #[serde(rename = "promptTokenCount")]
    prompt_token_count: Option<u32>,
    #[serde(rename = "candidatesTokenCount")]
    candidates_token_count: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct GenerateContentEnvelope {
    #[serde(flatten)]
    body: GenerateContentResponse,
    #[serde(rename = "usageMetadata")]
    usage_metadata: Option<UsageMetadata>,
}

#[async_trait]
impl ModelProvider for GeminiProvider {
    async fn call(&self, req: ModelRequest) -> Result<ModelResponse, String> {
        let url = format!(
            "{}/models/{}:generateContent?key={}",
            self.base_url, req.model_id, self.api_key
        );

        let system_instruction = if req.system.is_empty() {
            None
        } else {
            Some(Content {
                role: None,
                parts: vec![Part { text: req.system.clone() }],
            })
        };

        // Ask for strict JSON back when the caller supplied a schema — this
        // is what lets summarize_file / summarize_commit / run_ranking /
        // narrate_stop all parse ModelResponse.parsed directly.
        let (response_mime_type, response_schema) = match &req.schema {
            Some(schema) => (Some("application/json".to_string()), Some(schema.clone())),
            None => (None, None),
        };

        let body = GenerateContentRequest {
            contents: vec![Content {
                role: Some("user".to_string()),
                parts: vec![Part { text: req.user.clone() }],
            }],
            system_instruction,
            generation_config: GenerationConfig {
                temperature: req.temperature,
                max_output_tokens: req.max_tokens,
                response_mime_type,
                response_schema,
            },
        };

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| format!("failed to build HTTP client: {e}"))?;

        // Held for the whole retry loop below: reserves this call's
        // concurrency slot and enforces spacing from the previous request;
        // staying acquired across retries stops a different queued caller
        // from jumping in ahead of our backoff sleep.
        let _permit = self.limiter.acquire().await;

        let mut attempt: u32 = 0;
        loop {
            let response = client
                .post(&url)
                .json(&body)
                .send()
                .await
                .map_err(|e| format!("Gemini request failed: {e}"))?;

            let status = response.status();

            if status.as_u16() == 429 {
                if attempt >= MAX_RETRIES_ON_RATE_LIMIT {
                    let text = response.text().await.unwrap_or_default();
                    let snippet: String = text.chars().take(500).collect();
                    return Err(format!(
                        "Gemini API returned HTTP 429 (rate limited) after {} retries: {snippet}",
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
                    "[gemini] 429 rate limited (attempt {}/{}), backing off {:?}",
                    attempt + 1,
                    MAX_RETRIES_ON_RATE_LIMIT,
                    backoff
                );
                tokio::time::sleep(backoff).await;
                attempt += 1;
                continue;
            }

            // Almost always an invalid/revoked key entered in Settings —
            // surface that plainly rather than a generic HTTP status line.
            if status.as_u16() == 401 || status.as_u16() == 403 {
                let text = response.text().await.unwrap_or_default();
                let snippet: String = text.chars().take(300).collect();
                return Err(format!(
                    "Gemini API rejected the API key (HTTP {status}). \
                     Double-check the key in Settings. Details: {snippet}"
                ));
            }

            if !status.is_success() {
                let text = response.text().await.unwrap_or_default();
                let snippet: String = text.chars().take(500).collect();
                return Err(format!("Gemini API returned HTTP {status}: {snippet}"));
            }

            let envelope: GenerateContentEnvelope = response
                .json()
                .await
                .map_err(|e| format!("failed to parse Gemini response JSON: {e}"))?;

            let candidates = envelope.body.candidates.unwrap_or_default();
            let first = candidates.first();

            let text = first
                .and_then(|c| c.content.as_ref())
                .and_then(|c| c.parts.as_ref())
                .and_then(|parts| parts.first())
                .and_then(|p| p.text.clone())
                .ok_or_else(|| {
                    let reason = first
                        .and_then(|c| c.finish_reason.clone())
                        .unwrap_or_else(|| "unknown".to_string());
                    format!("Gemini response had no text content (finish_reason: {reason})")
                })?;

            // Only attempt a JSON parse when we asked for JSON; a non-fatal
            // miss (e.g. the model wrapped the JSON in prose despite the
            // schema) leaves `parsed` as None so callers fall back to
            // `resp.text`.
            let parsed = if req.schema.is_some() {
                serde_json::from_str::<serde_json::Value>(&text).ok()
            } else {
                None
            };

            let (input_tokens, output_tokens) = envelope
                .usage_metadata
                .map(|u| (u.prompt_token_count, u.candidates_token_count))
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
    fn new_stores_api_key_and_default_base_url() {
        let p = GeminiProvider::new("test-key-value");
        assert_eq!(p.api_key, "test-key-value");
        assert_eq!(p.base_url, DEFAULT_BASE_URL);
    }

    #[test]
    fn with_base_url_overrides_default() {
        let p = GeminiProvider::with_base_url("key", Some("https://proxy.example.com/v1beta".into()));
        assert_eq!(p.base_url, "https://proxy.example.com/v1beta");
    }

    #[test]
    fn with_base_url_falls_back_on_blank_override() {
        let p = GeminiProvider::with_base_url("key", Some("   ".into()));
        assert_eq!(p.base_url, DEFAULT_BASE_URL);
    }
}