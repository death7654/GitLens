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
//!
//! # Reasoning / thinking support
//!
//! Set `ModelRequest.reasoning = Some(ReasoningConfig { enabled: Some(true), .. })`
//! to activate backend-specific chain-of-thought.  The wire format is chosen
//! automatically based on `ReasoningKind` (or inferred from `OPENAI_BASE_URL`
//! when `kind == Auto`):
//!
//! | `ReasoningKind` | Env var hint (`OPENAI_BASE_URL`)     | Wire parameter emitted                            |
//! |-----------------|--------------------------------------|---------------------------------------------------|
//! | `OpenAi`        | `api.openai.com`                     | `reasoning_effort: "low"/"medium"/"high"`         |
//! | `LlamaCpp`      | `localhost:8080` or URL has `llama`  | `chat_template_kwargs: { enable_thinking: true }` |
//! | `LmStudio`      | `localhost:1234`                     | `chat_template_kwargs: { enable_thinking: true }` |
//! | `Ollama`        | `localhost:11434` (default)          | `chat_template_kwargs: { enable_thinking: true }` |
//! | `OpenRouter`    | `openrouter.ai`                      | `reasoning: { max_tokens: N }`                    |
//! | `Vllm`          | anything else                        | `chat_template_kwargs: { enable_thinking: true }` |
//! | `Disabled`      | —                                    | nothing emitted                                   |
//!
//! Note that `chat_template_kwargs` is a broad-but-best-effort hint: older
//! LM Studio builds ignore it, and vLLM requires `--enable-reasoning` to be
//! set server-side before the flag has any effect.  Unsupported servers
//! silently no-op rather than erroring.
//!
//! Separate reasoning text (`reasoning_content`, `reasoning`, or inline
//! `<thinking>…</thinking>`) is extracted into `ModelResponse.reasoning_text`
//! when the backend returns it.
//!
//! # Ranking pipeline env vars
//!
//! The following variables are read by `RankingConfig::default()` (not by this
//! provider directly) and thread reasoning through the pipeline:
//!
//! | Variable                                  | Default | Effect                                          |
//! |-------------------------------------------|---------|-------------------------------------------------|
//! | `GITLENS_REASONING_ENABLED`               | `false` | Master switch for ranking-stage reasoning       |
//! | `GITLENS_MAX_OUTPUT_TOKENS`               | `4096`  | Token budget for the final answer / extraction  |
//! | `GITLENS_MAX_REASONING_TOKENS`            | `2048`  | Soft cap on reasoning tokens                    |
//! | `GITLENS_REASONING_TEMPERATURE`           | `0.6`   | Temperature for the reasoning pass              |
//! | `GITLENS_SPLIT_REASONING_FROM_EXTRACTION` | `false` | Two-call mode: reasoning then JSON extraction   |

use std::env;
use std::time::Duration;

use async_trait::async_trait;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

use crate::provider::{ModelProvider, ModelRequest, ModelResponse, ReasoningKind};

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

    /// Resolve `Auto` to a concrete `ReasoningKind` based on the base URL.
    ///
    /// The heuristics are intentionally broad — they only need to cover the
    /// common default ports and hostnames.  Users who run a non-standard setup
    /// should set `kind` explicitly.
    pub fn resolve_kind(kind: ReasoningKind, base_url: &str) -> ReasoningKind {
        if kind != ReasoningKind::Auto {
            return kind;
        }
        let url = base_url.to_ascii_lowercase();
        if url.contains("openrouter.ai") {
            ReasoningKind::OpenRouter
        } else if url.contains("api.openai.com") {
            ReasoningKind::OpenAi
        } else if url.contains("localhost:11434") || url.contains("127.0.0.1:11434") {
            ReasoningKind::Ollama
        } else if url.contains("localhost:1234") || url.contains("127.0.0.1:1234") {
            ReasoningKind::LmStudio
        } else if url.contains("localhost:8080") || url.contains("127.0.0.1:8080") || url.contains("llama") {
            ReasoningKind::LlamaCpp
        } else {
            // Safest generic for unknown endpoints: chat_template_kwargs is
            // broadly supported by open-source servers; it no-ops silently
            // when the server doesn't recognise it.
            ReasoningKind::LlamaCpp
        }
    }

    /// Compute `reasoning_effort` string from a token count.
    ///
    /// Mapping: `< 512 → "low"`, `< 2048 → "medium"`, else `"high"`.
    fn effort_from_tokens(tokens: Option<u32>) -> &'static str {
        match tokens {
            Some(n) if n < 512 => "low",
            Some(n) if n < 2048 => "medium",
            _ => "high",
        }
    }

    /// Build the serialisable `ChatRequest` body from a `ModelRequest`.
    ///
    /// Separated from `call` so that tests can inspect the serialised JSON
    /// without needing a live server.  Called unconditionally from `call`,
    /// so this is never dead code.
    pub(crate) fn build_request_body(&self, req: &ModelRequest) -> ChatRequest {
        // Build the messages array.
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

        // Determine whether to emit reasoning parameters.
        let reasoning_active = req
            .reasoning
            .as_ref()
            .and_then(|r| r.enabled)
            .unwrap_or(false);

        let resolved_kind = req
            .reasoning
            .as_ref()
            .map(|r| Self::resolve_kind(r.kind, &self.base_url))
            .unwrap_or(ReasoningKind::Disabled);

        let max_reasoning_tokens = req.reasoning.as_ref().and_then(|r| r.max_reasoning_tokens);

        // Build optional reasoning extension fields.
        let (reasoning_effort, chat_template_kwargs, openrouter_reasoning) =
            if reasoning_active && resolved_kind != ReasoningKind::Disabled {
                match resolved_kind {
                    ReasoningKind::OpenAi => {
                        let effort = Self::effort_from_tokens(max_reasoning_tokens);
                        (Some(effort.to_string()), None, None)
                    }
                    ReasoningKind::LlamaCpp
                    | ReasoningKind::LmStudio
                    | ReasoningKind::Ollama
                    | ReasoningKind::Vllm => {
                        let kwargs = serde_json::json!({ "enable_thinking": true });
                        (None, Some(kwargs), None)
                    }
                    ReasoningKind::OpenRouter => {
                        let reasoning_obj = OpenRouterReasoning {
                            max_tokens: max_reasoning_tokens,
                        };
                        (None, None, Some(reasoning_obj))
                    }
                    // Auto should have been resolved above; treat residual Auto as LlamaCpp.
                    ReasoningKind::Auto => {
                        let kwargs = serde_json::json!({ "enable_thinking": true });
                        (None, Some(kwargs), None)
                    }
                    ReasoningKind::Disabled => (None, None, None),
                }
            } else {
                (None, None, None)
            };

        // OpenAI o-series models (ids matching `^o\d`, e.g. o1, o3-mini,
        // o4-mini) reject `temperature` unconditionally — not only when
        // reasoning is explicitly enabled.  Emit `None` whenever the model id
        // matches, regardless of which `ReasoningKind` was selected, so a
        // misconfigured `kind` doesn't turn into a 400 from the API.
        let temperature = {
            static RE: OnceLock<Regex> = OnceLock::new();
            let is_o_series = RE
                .get_or_init(|| Regex::new(r"^o\d").unwrap())
                .is_match(&req.model_id);
            if is_o_series {
                None
            } else {
                Some(req.temperature)
            }
        };

        ChatRequest {
            model: req.model_id.clone(),
            messages,
            temperature,
            max_tokens: req.max_tokens,
            response_format,
            reasoning_effort,
            chat_template_kwargs,
            reasoning: openrouter_reasoning,
        }
    }

    /// Extract reasoning text from a `ChoiceMessage`, following the priority:
    ///
    /// 1. `reasoning_content` field
    /// 2. `reasoning` field
    /// 3. Inline `<thinking>…</thinking>` / triple-backtick `thinking` block in `content`
    ///
    /// Returns `(content_text, reasoning_text)` where `content_text` has any
    /// inline thinking block stripped.
    fn extract_reasoning(message: &ChoiceMessage) -> (String, Option<String>) {
        let content = message.content.clone().unwrap_or_default();

        // Priority 1: dedicated reasoning_content field.
        if let Some(rc) = &message.reasoning_content {
            if !rc.is_empty() {
                return (content, Some(rc.clone()));
            }
        }
        // Priority 2: reasoning field (OpenRouter).
        if let Some(r) = &message.reasoning {
            if !r.is_empty() {
                return (content, Some(r.clone()));
            }
        }
        // Priority 3: inline thinking block.
        // Try <thinking>…</thinking> first, then triple-backtick thinking block.
        if let Some((stripped, thinking)) = strip_thinking_xml(&content) {
            return (stripped, Some(thinking));
        }
        if let Some((stripped, thinking)) = strip_thinking_fence(&content) {
            return (stripped, Some(thinking));
        }

        (content, None)
    }
}

/// Strip `<thinking>…</thinking>` from `content`.
///
/// Returns `(content_without_block, thinking_text)` or `None` if not present.
fn strip_thinking_xml(content: &str) -> Option<(String, String)> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?s)<thinking>(.*?)</thinking>").unwrap()
    });
    let caps = re.captures(content)?;
    let thinking = caps.get(1)?.as_str().trim().to_string();
    let stripped = re.replace(content, "").trim().to_string();
    Some((stripped, thinking))
}

/// Strip a triple-backtick `thinking` fenced block from `content`.
///
/// Returns `(content_without_block, thinking_text)` or `None` if not present.
fn strip_thinking_fence(content: &str) -> Option<(String, String)> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?s)```thinking\n?(.*?)```").unwrap()
    });
    let caps = re.captures(content)?;
    let thinking = caps.get(1)?.as_str().trim().to_string();
    let stripped = re.replace(content, "").trim().to_string();
    Some((stripped, thinking))
}

// ---------------------------------------------------------------------------
// Wire shapes — OpenAI `/v1/chat/completions` request / response subset
// ---------------------------------------------------------------------------

/// Serialisable request body for `/v1/chat/completions`.
///
/// All optional extension fields use `skip_serializing_if = "Option::is_none"`
/// so that when reasoning is disabled the bytes are byte-for-byte identical to
/// the old implementation.
#[derive(Debug, Serialize)]
pub(crate) struct ChatRequest {
    model: String,
    messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<ResponseFormat>,
    /// OpenAI o-series reasoning effort.  Only present when
    /// `reasoning.kind == OpenAi` and `reasoning.enabled == true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<String>,
    /// llama.cpp / LM Studio / Ollama / vLLM thinking activation.  Only
    /// present when the resolved kind is one of those backends and reasoning
    /// is enabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    chat_template_kwargs: Option<serde_json::Value>,
    /// OpenRouter reasoning object.  Only present for the OpenRouter backend.
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<OpenRouterReasoning>,
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

/// OpenRouter `reasoning` object.
#[derive(Debug, Serialize)]
struct OpenRouterReasoning {
    /// Soft cap on reasoning tokens.  Omitted when `None` so OpenRouter uses
    /// its own default.
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
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
    /// Separate reasoning text returned by DeepSeek-R1, QwQ, Qwen3-think,
    /// and llama.cpp with `enable_thinking`.
    #[serde(default)]
    reasoning_content: Option<String>,
    /// Separate reasoning text returned by OpenRouter-routed reasoning models.
    #[serde(default)]
    reasoning: Option<String>,
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

        let body = self.build_request_body(&req);

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

        let first_msg = first.and_then(|c| c.message.as_ref());

        // Extract reasoning text alongside content, stripping any inline block.
        let (text, reasoning_text) = if let Some(msg) = first_msg {
            Self::extract_reasoning(msg)
        } else {
            let reason = first
                .and_then(|c| c.finish_reason.clone())
                .unwrap_or_else(|| "unknown".to_string());
            return Err(format!(
                "OpenAI response had no text content (finish_reason: {reason})"
            ));
        };

        // Ensure we have a non-empty content string (reasoning_text may be Some
        // even when content is empty for pure-reasoning responses).
        if text.is_empty() && reasoning_text.is_none() {
            let reason = first
                .and_then(|c| c.finish_reason.clone())
                .unwrap_or_else(|| "unknown".to_string());
            return Err(format!(
                "OpenAI response had no text content (finish_reason: {reason})"
            ));
        }

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
            reasoning_text,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ReasoningConfig, ReasoningKind};

    fn make_provider(base_url: &str) -> OpenAiProvider {
        OpenAiProvider::new(None::<String>, base_url)
    }

    fn base_req() -> ModelRequest {
        ModelRequest {
            system: "sys".into(),
            user: "user".into(),
            schema: None,
            temperature: 0.5,
            model_id: "gpt-4o".into(),
            max_tokens: Some(512),
            reasoning: None,
        }
    }

    // -- pre-existing env-var tests ------------------------------------------

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

    // -- Test 1: reasoning disabled emits no extra fields --------------------

    #[test]
    fn reasoning_disabled_emits_no_extra_fields() {
        let p = make_provider("http://localhost:11434/v1");

        // Case A: reasoning is None
        let req_none = base_req();
        let body = p.build_request_body(&req_none);
        let json = serde_json::to_value(&body).unwrap();
        assert!(json.get("reasoning_effort").is_none(), "should not have reasoning_effort");
        assert!(json.get("chat_template_kwargs").is_none(), "should not have chat_template_kwargs");
        assert!(json.get("reasoning").is_none(), "should not have reasoning");

        // Case B: reasoning.enabled = Some(false)
        let mut req_false = base_req();
        req_false.reasoning = Some(ReasoningConfig {
            enabled: Some(false),
            max_reasoning_tokens: Some(1000),
            temperature: Some(0.6),
            kind: ReasoningKind::Auto,
        });
        let body2 = p.build_request_body(&req_false);
        let json2 = serde_json::to_value(&body2).unwrap();
        assert!(json2.get("reasoning_effort").is_none());
        assert!(json2.get("chat_template_kwargs").is_none());
        assert!(json2.get("reasoning").is_none());
    }

    // -- Test 2: auto kind detection from base URL ---------------------------

    #[test]
    fn reasoning_kind_auto_detects_from_base_url() {
        let cases: &[(&str, ReasoningKind)] = &[
            ("http://localhost:11434/v1",          ReasoningKind::Ollama),
            ("http://127.0.0.1:11434/v1",          ReasoningKind::Ollama),
            ("http://localhost:1234/v1",            ReasoningKind::LmStudio),
            ("http://127.0.0.1:1234/v1",           ReasoningKind::LmStudio),
            ("http://localhost:8080/v1",            ReasoningKind::LlamaCpp),
            ("http://my-llama-host:5000/v1",        ReasoningKind::LlamaCpp),
            ("https://openrouter.ai/api/v1",        ReasoningKind::OpenRouter),
            ("https://api.openai.com/v1",           ReasoningKind::OpenAi),
            ("http://someserver:9999/v1",           ReasoningKind::LlamaCpp), // fallback
        ];
        for (url, expected) in cases {
            let resolved = OpenAiProvider::resolve_kind(ReasoningKind::Auto, url);
            assert_eq!(resolved, *expected, "URL: {url}");
        }
    }

    // -- Test 3: OpenAI effort mapping ---------------------------------------

    #[test]
    fn reasoning_openai_emits_reasoning_effort() {
        let p = make_provider("https://api.openai.com/v1");
        let cases: &[(u32, &str)] = &[
            (100, "low"),
            (511, "low"),
            (512, "medium"),
            (1500, "medium"),
            (2047, "medium"),
            (2048, "high"),
            (4000, "high"),
        ];
        for (tokens, expected_effort) in cases {
            let mut req = base_req();
            req.model_id = "gpt-4o".into(); // non-o-series so temperature is kept
            req.reasoning = Some(ReasoningConfig {
                enabled: Some(true),
                max_reasoning_tokens: Some(*tokens),
                temperature: None,
                kind: ReasoningKind::OpenAi,
            });
            let body = p.build_request_body(&req);
            let json = serde_json::to_value(&body).unwrap();
            assert_eq!(
                json.get("reasoning_effort").and_then(|v| v.as_str()),
                Some(*expected_effort),
                "tokens={tokens}"
            );
        }
    }

    // -- Test 4: llama.cpp emits chat_template_kwargs ------------------------

    #[test]
    fn reasoning_llamacpp_emits_chat_template_kwargs() {
        let p = make_provider("http://localhost:8080/v1");
        let mut req = base_req();
        req.reasoning = Some(ReasoningConfig {
            enabled: Some(true),
            max_reasoning_tokens: Some(512),
            temperature: None,
            kind: ReasoningKind::LlamaCpp,
        });
        let body = p.build_request_body(&req);
        let json = serde_json::to_value(&body).unwrap();
        let ctk = json.get("chat_template_kwargs").expect("should have chat_template_kwargs");
        assert_eq!(
            ctk.get("enable_thinking").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert!(json.get("reasoning_effort").is_none());
    }

    // -- Test 5: OpenRouter emits reasoning.max_tokens ----------------------

    #[test]
    fn reasoning_openrouter_emits_reasoning_max_tokens() {
        let p = make_provider("https://openrouter.ai/api/v1");
        let mut req = base_req();
        req.reasoning = Some(ReasoningConfig {
            enabled: Some(true),
            max_reasoning_tokens: Some(1024),
            temperature: None,
            kind: ReasoningKind::OpenRouter,
        });
        let body = p.build_request_body(&req);
        let json = serde_json::to_value(&body).unwrap();
        let reasoning_obj = json.get("reasoning").expect("should have reasoning object");
        assert_eq!(
            reasoning_obj.get("max_tokens").and_then(|v| v.as_u64()),
            Some(1024)
        );
    }

    // -- Test 6: parse reasoning_content field ------------------------------

    #[test]
    fn parses_reasoning_content_field() {
        let msg = ChoiceMessage {
            content: Some("final answer".to_string()),
            reasoning_content: Some("my chain of thought".to_string()),
            reasoning: None,
        };
        let (text, rt) = OpenAiProvider::extract_reasoning(&msg);
        assert_eq!(text, "final answer");
        assert_eq!(rt, Some("my chain of thought".to_string()));
    }

    // -- Test 7: parse reasoning field (OpenRouter) -------------------------

    #[test]
    fn parses_reasoning_field() {
        let msg = ChoiceMessage {
            content: Some("the answer".to_string()),
            reasoning_content: None,
            reasoning: Some("openrouter thinking".to_string()),
        };
        let (text, rt) = OpenAiProvider::extract_reasoning(&msg);
        assert_eq!(text, "the answer");
        assert_eq!(rt, Some("openrouter thinking".to_string()));
    }

    // -- Test 8: inline <thinking> block extraction -------------------------

    #[test]
    fn extracts_inline_thinking_block() {
        let msg = ChoiceMessage {
            content: Some("<thinking>foo</thinking>bar".to_string()),
            reasoning_content: None,
            reasoning: None,
        };
        let (text, rt) = OpenAiProvider::extract_reasoning(&msg);
        assert_eq!(rt, Some("foo".to_string()));
        assert_eq!(text, "bar");
    }

    // -- Test 8b: reasoning_content takes priority over inline block --------

    #[test]
    fn reasoning_content_takes_priority_over_inline() {
        let msg = ChoiceMessage {
            content: Some("<thinking>inline</thinking>answer".to_string()),
            reasoning_content: Some("dedicated field".to_string()),
            reasoning: None,
        };
        let (text, rt) = OpenAiProvider::extract_reasoning(&msg);
        // reasoning_content has higher priority; content is not stripped
        assert_eq!(rt, Some("dedicated field".to_string()));
        assert_eq!(text, "<thinking>inline</thinking>answer");
    }

    // -- Test: triple-backtick thinking fence --------------------------------

    #[test]
    fn extracts_backtick_thinking_fence() {
        let content = "```thinking\nmy thoughts\n```\nthe answer";
        let msg = ChoiceMessage {
            content: Some(content.to_string()),
            reasoning_content: None,
            reasoning: None,
        };
        let (text, rt) = OpenAiProvider::extract_reasoning(&msg);
        assert_eq!(rt, Some("my thoughts".to_string()));
        assert_eq!(text, "the answer");
    }

    // -- o-series model omits temperature regardless of reasoning state ------

    #[test]
    fn o_series_omits_temperature_when_reasoning_enabled() {
        let p = make_provider("https://api.openai.com/v1");
        let mut req = base_req();
        req.model_id = "o3-mini".into();
        req.reasoning = Some(ReasoningConfig {
            enabled: Some(true),
            max_reasoning_tokens: Some(1000),
            temperature: None,
            kind: ReasoningKind::OpenAi,
        });
        let body = p.build_request_body(&req);
        let json = serde_json::to_value(&body).unwrap();
        // temperature must be omitted for o-series
        assert!(json.get("temperature").is_none(), "o-series should not send temperature");
    }

    /// o-series rejects `temperature` unconditionally — even when reasoning
    /// is disabled and even when the user has picked a wrong `kind`.  Emitting
    /// it just produces an HTTP 400 from OpenAI, so we omit it whenever the
    /// model id matches regardless of reasoning state.
    #[test]
    fn o_series_omits_temperature_when_reasoning_disabled() {
        let p = make_provider("https://api.openai.com/v1");

        // reasoning = None
        let mut req_none = base_req();
        req_none.model_id = "o3-mini".into();
        let body = p.build_request_body(&req_none);
        let json = serde_json::to_value(&body).unwrap();
        assert!(
            json.get("temperature").is_none(),
            "o-series must not send temperature even with reasoning = None"
        );

        // reasoning.enabled = Some(false)
        let mut req_off = base_req();
        req_off.model_id = "o4-mini".into();
        req_off.reasoning = Some(ReasoningConfig {
            enabled: Some(false),
            ..Default::default()
        });
        let body = p.build_request_body(&req_off);
        let json = serde_json::to_value(&body).unwrap();
        assert!(
            json.get("temperature").is_none(),
            "o-series must not send temperature even with reasoning disabled"
        );

        // reasoning.enabled = Some(true), kind = LlamaCpp (user error)
        let mut req_wrong_kind = base_req();
        req_wrong_kind.model_id = "o1-preview".into();
        req_wrong_kind.reasoning = Some(ReasoningConfig {
            enabled: Some(true),
            max_reasoning_tokens: Some(1000),
            temperature: None,
            kind: ReasoningKind::LlamaCpp,
        });
        let body = p.build_request_body(&req_wrong_kind);
        let json = serde_json::to_value(&body).unwrap();
        assert!(
            json.get("temperature").is_none(),
            "o-series must not send temperature even when a non-OpenAI kind is selected"
        );
    }
}