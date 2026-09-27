//! Person 6 — Concrete `ModelProvider` backed directly by the Google Gemini
//! API (the "Generative Language API"), replacing the Bob Shell CLI wrapper.
//!
//! Auth: `GEMINI_API_KEY` environment variable.
//! Docs: <https://ai.google.dev/gemini-api/docs>
//!
//! Structured output: when `ModelRequest.schema` is `Some`, we ask Gemini
//! for JSON directly via `generationConfig.responseMimeType` +
//! `responseSchema` (supported by gemini-1.5-pro and newer), and parse the
//! returned text as JSON into `ModelResponse::parsed`. On any parse failure
//! the raw text is still returned so callers can fall back gracefully —
//! mirrors how `BobShellProvider` behaved.

use std::env;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::provider::{ModelProvider, ModelRequest, ModelResponse};

const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Concrete [`ModelProvider`] that calls the Gemini API over HTTP.
#[derive(Debug)]
pub struct GeminiProvider {
    api_key: String,
    base_url: String,
    /// Shared HTTP client — holds the connection pool; must not be rebuilt per call.
    client: reqwest::Client,
}

impl GeminiProvider {
    /// Construct from explicit values — mainly useful in tests.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(60))
                .build()
                .expect("failed to build reqwest client"),
        }
    }

    /// Construct from the environment.
    ///
    /// Reads `GEMINI_API_KEY` (required) and `GEMINI_BASE_URL` (optional,
    /// defaults to the standard Generative Language API endpoint — override
    /// only for a proxy or a regional endpoint). Returns `Err` if the API
    /// key is absent.
    pub fn from_env() -> Result<Self, String> {
        let api_key = env::var("GEMINI_API_KEY")
            .map_err(|_| "GEMINI_API_KEY environment variable is not set".to_string())?;
        if api_key.trim().is_empty() {
            return Err("GEMINI_API_KEY is set but empty".to_string());
        }
        let base_url = env::var("GEMINI_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
        Ok(Self {
            api_key,
            base_url,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(60))
                .build()
                .expect("failed to build reqwest client"),
        })
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

        let response = self.client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Gemini request failed: {e}"))?;

        let status = response.status();
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

        // Only attempt a JSON parse when we asked for JSON; a non-fatal miss
        // (e.g. the model wrapped the JSON in prose despite the schema)
        // leaves `parsed` as None so callers fall back to `resp.text`.
        let parsed = if req.schema.is_some() {
            serde_json::from_str::<serde_json::Value>(&text).ok()
        } else {
            None
        };

        let (input_tokens, output_tokens) = envelope
            .usage_metadata
            .map(|u| (u.prompt_token_count, u.candidates_token_count))
            .unwrap_or((None, None));

        Ok(ModelResponse {
            text,
            parsed,
            model_id: req.model_id,
            input_tokens,
            output_tokens,
            reasoning_text: None,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_env_errors_without_api_key() {
        let saved = env::var("GEMINI_API_KEY").ok();
        unsafe { env::remove_var("GEMINI_API_KEY"); }
        let result = GeminiProvider::from_env();
        if let Some(v) = saved {
            unsafe { env::set_var("GEMINI_API_KEY", v); }
        }
        assert!(result.is_err());
        assert!(result.unwrap_err().to_lowercase().contains("gemini_api_key"));
    }

    #[test]
    fn from_env_errors_on_empty_api_key() {
        let saved = env::var("GEMINI_API_KEY").ok();
        unsafe { env::set_var("GEMINI_API_KEY", ""); }
        let result = GeminiProvider::from_env();
        if let Some(v) = saved {
            unsafe { env::set_var("GEMINI_API_KEY", v); }
        } else {
            unsafe { env::remove_var("GEMINI_API_KEY"); }
        }
        assert!(result.is_err());
    }

    #[test]
    fn new_stores_api_key_and_default_base_url() {
        let p = GeminiProvider::new("test-key-value");
        assert_eq!(p.api_key, "test-key-value");
        assert_eq!(p.base_url, DEFAULT_BASE_URL);
    }
}