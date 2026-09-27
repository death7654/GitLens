//! Concrete [`ModelProvider`] backed by any OpenAI-compatible v1 API.
//!
//! Works with the real OpenAI service and with local inference servers that
//! expose the `/v1/chat/completions` endpoint (Ollama, llama.cpp, LM Studio,
//! vLLM, Jan, etc.).
//!
//! # Configuration (environment variables)
//!
//! | Variable              | Required? | Default                         | Description                                           |
//! |-----------------------|-----------|---------------------------------|-------------------------------------------------------|
//! | `OPENAI_API_KEY`      | No        | absent (no `Authorization` hdr) | Bearer token sent as `Authorization: Bearer <key>`.  |
//! |                       |           |                                 | Omit or leave blank for unauthenticated local servers.|
//! |                       |           |                                 | Set to your `sk-…` key for the real OpenAI API.       |
//! | `OPENAI_BASE_URL`     | No        | `http://localhost:11434/v1`     | Base URL of the v1 API.  Must include `/v1`.  Override|
//! |                       |           |                                 | to point at any compatible local or remote server.   |
//!
//! When both `OPENAI_API_KEY` and `OPENAI_BASE_URL` are absent the provider
//! defaults to a local Ollama instance — no configuration needed for purely
//! local use.
//!
//! # Structured output
//!
//! When `ModelRequest.schema` is `Some`, the provider sets
//! `response_format = { type: "json_object" }` and instructs the model via
//! the system prompt to emit valid JSON.  Not all local servers honour the
//! `response_format` field, so the system-prompt hint ensures correct
//! behaviour even without it.  The raw text is always returned in
//! `ModelResponse.text`; `ModelResponse.parsed` is set on a successful
//! `serde_json` parse and left as `None` otherwise.

use std::env;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::provider::{ModelProvider, ModelRequest, ModelResponse};

const DEFAULT_BASE_URL: &str = "http://localhost:11434/v1";

/// Concrete [`ModelProvider`] that calls an OpenAI-compatible `/v1/chat/completions` endpoint.
#[derive(Debug)]
pub struct OpenAiProvider {
    /// Optional bearer token.  `None` means no `Authorization` header is sent.
    api_key: Option<String>,
    base_url: String,
    /// Shared HTTP client — holds the connection pool; must not be rebuilt per call.
    client: reqwest::Client,
}

impl OpenAiProvider {
    /// Construct from explicit values — mainly useful in tests.
    ///
    /// Pass `api_key = None` to omit the `Authorization` header entirely
    /// (correct for most unauthenticated local servers).
    pub fn new(api_key: Option<impl Into<String>>, base_url: impl Into<String>) -> Self {
        Self {
            api_key: api_key.map(Into::into),
            base_url: base_url.into(),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .expect("failed to build reqwest client"),
        }
    }

    /// Returns the base URL this provider was constructed with — used for
    /// diagnostic logging in `lib.rs`.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Construct from the environment.
    ///
    /// Reads `OPENAI_API_KEY` (optional) and `OPENAI_BASE_URL` (optional).
    /// Never returns `Err` — both variables are optional; the provider always
    /// constructs successfully, falling back to the local-Ollama default.
    pub fn from_env() -> Self {
        let api_key = env::var("OPENAI_API_KEY")
            .ok()
            .filter(|v| !v.trim().is_empty());
        let base_url = env::var("OPENAI_BASE_URL")
            .unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
        Self {
            api_key,
            base_url,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .expect("failed to build reqwest client"),
        }
    }
}

// ---------------------------------------------------------------------------
// Wire shapes — OpenAI `/v1/chat/completions` request / response subset
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<Message>,
    temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<ResponseFormat>,
}

#[derive(Debug, Serialize)]
struct Message {
    role: String,
    content: String,
}

/// Instructs compatible servers to return raw JSON.
#[derive(Debug, Serialize)]
struct ResponseFormat {
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Option<Vec<Choice>>,
    usage: Option<Usage>,
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: Option<ChoiceMessage>,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChoiceMessage {
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Usage {
    prompt_tokens: Option<u32>,
    completion_tokens: Option<u32>,
}

// ---------------------------------------------------------------------------
// ModelProvider impl
// ---------------------------------------------------------------------------

#[async_trait]
impl ModelProvider for OpenAiProvider {
    async fn call(&self, req: ModelRequest) -> Result<ModelResponse, String> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));

        // Build the messages array.
        // When a JSON schema is requested, append a JSON-mode instruction to
        // the system prompt so servers that ignore `response_format` still
        // produce valid JSON.
        //
        // Important: the real OpenAI API requires the word "json" to appear
        // somewhere in the messages when `response_format: json_object` is set,
        // or it returns a 400.  We therefore always include a system message
        // when structured output is requested, even if `req.system` was empty.
        let system_content = if req.schema.is_some() {
            Some(format!(
                "{}\n\nYou MUST respond with valid JSON only — no prose, no markdown fences.",
                req.system
            ))
        } else if !req.system.trim().is_empty() {
            Some(req.system.clone())
        } else {
            None
        };

        let mut messages: Vec<Message> = Vec::new();
        if let Some(content) = system_content {
            messages.push(Message {
                role: "system".to_string(),
                content,
            });
        }
        messages.push(Message {
            role: "user".to_string(),
            content: req.user.clone(),
        });

        // Ask for JSON output when the caller supplied a schema.
        let response_format = req.schema.as_ref().map(|_| ResponseFormat {
            kind: "json_object".to_string(),
        });

        let body = ChatRequest {
            model: req.model_id.clone(),
            messages,
            temperature: req.temperature,
            max_tokens: req.max_tokens,
            response_format,
        };

        let mut builder = self.client.post(&url).json(&body);

        // Only attach the Authorization header when an API key was provided.
        if let Some(key) = &self.api_key {
            builder = builder.header("Authorization", format!("Bearer {key}"));
        }

        let response = builder
            .send()
            .await
            .map_err(|e| format!("OpenAI request failed: {e}"))?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            let snippet: String = text.chars().take(500).collect();
            return Err(format!("OpenAI API returned HTTP {status}: {snippet}"));
        }

        let chat_resp: ChatResponse = response
            .json()
            .await
            .map_err(|e| format!("failed to parse OpenAI response JSON: {e}"))?;

        let choices = chat_resp.choices.unwrap_or_default();
        let first = choices.first();

        let text = first
            .and_then(|c| c.message.as_ref())
            .and_then(|m| m.content.clone())
            .ok_or_else(|| {
                let reason = first
                    .and_then(|c| c.finish_reason.clone())
                    .unwrap_or_else(|| "unknown".to_string());
                format!("OpenAI response had no text content (finish_reason: {reason})")
            })?;

        // Attempt a JSON parse only when we requested structured output.
        let parsed = if req.schema.is_some() {
            serde_json::from_str::<serde_json::Value>(&text).ok()
        } else {
            None
        };

        let model_id = chat_resp
            .model
            .unwrap_or_else(|| req.model_id.clone());

        let (input_tokens, output_tokens) = chat_resp
            .usage
            .map(|u| (u.prompt_tokens, u.completion_tokens))
            .unwrap_or((None, None));

        Ok(ModelResponse {
            text,
            parsed,
            model_id,
            input_tokens,
            output_tokens,
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
    fn from_env_uses_default_base_url_when_unset() {
        let saved_url = env::var("OPENAI_BASE_URL").ok();
        let saved_key = env::var("OPENAI_API_KEY").ok();
        unsafe {
            env::remove_var("OPENAI_BASE_URL");
            env::remove_var("OPENAI_API_KEY");
        }
        let p = OpenAiProvider::from_env();
        // Restore
        if let Some(v) = saved_url {
            unsafe { env::set_var("OPENAI_BASE_URL", v); }
        }
        if let Some(v) = saved_key {
            unsafe { env::set_var("OPENAI_API_KEY", v); }
        }
        assert_eq!(p.base_url, DEFAULT_BASE_URL);
        assert!(p.api_key.is_none());
    }

    #[test]
    fn from_env_picks_up_custom_base_url_and_key() {
        unsafe {
            env::set_var("OPENAI_BASE_URL", "http://custom-host:8080/v1");
            env::set_var("OPENAI_API_KEY", "sk-test");
        }
        let p = OpenAiProvider::from_env();
        unsafe {
            env::remove_var("OPENAI_BASE_URL");
            env::remove_var("OPENAI_API_KEY");
        }
        assert_eq!(p.base_url, "http://custom-host:8080/v1");
        assert_eq!(p.api_key.as_deref(), Some("sk-test"));
    }

    #[test]
    fn empty_api_key_is_treated_as_none() {
        unsafe { env::set_var("OPENAI_API_KEY", "   "); }
        let p = OpenAiProvider::from_env();
        unsafe { env::remove_var("OPENAI_API_KEY"); }
        assert!(p.api_key.is_none(), "whitespace-only key must be treated as absent");
    }

    #[test]
    fn new_stores_values() {
        let p = OpenAiProvider::new(Some("key-abc"), "http://localhost:1234/v1");
        assert_eq!(p.api_key.as_deref(), Some("key-abc"));
        assert_eq!(p.base_url, "http://localhost:1234/v1");
    }
}
