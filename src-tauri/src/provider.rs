//! Person 6 — Model provider abstraction (proposed interface).
//!
//! P3/P4 build against this trait only. Swapping Gemini↔Bob is a config
//! change on P6's side, not a code change on ours. If the trait shape drifts,
//! update these signatures; P3/P4 call sites don't change.

use serde::{Deserialize, Serialize};

/// Controls how the model's internal reasoning / thinking step is handled.
///
/// When `enabled` is `None` or `Some(false)`, no reasoning parameters are
/// emitted on the wire and all behaviour is identical to before this type
/// was introduced.  Set `enabled = Some(true)` and choose the appropriate
/// `kind` to activate a backend-specific reasoning mode.
///
/// Backends that do not support a given parameter silently ignore it; the
/// response still succeeds.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ReasoningConfig {
    /// Master switch.  When `None` or `Some(false)`, no reasoning parameters
    /// are emitted on the wire and behaviour is exactly as before this change.
    pub enabled: Option<bool>,
    /// Soft cap on reasoning / thinking tokens.  Backends that do not support
    /// a cap (e.g. plain llama.cpp without a `chat_template_kwargs` patch)
    /// silently ignore this field.
    pub max_reasoning_tokens: Option<u32>,
    /// Temperature applied *only* during the reasoning pass.  Independent of
    /// `ModelRequest.temperature`, which governs the extraction / final-answer
    /// pass.  Backends that do not distinguish reasoning temperature ignore it.
    pub temperature: Option<f32>,
    /// Backend hint.  `Auto` (the default) infers the kind from `OPENAI_BASE_URL`.
    #[serde(default)]
    pub kind: ReasoningKind,
}

/// Selects which wire-format translation the OpenAI-compatible provider uses
/// when emitting reasoning parameters.
///
/// `Auto` (default) infers the correct kind from the base URL at call time,
/// so callers that do not care about the distinction can leave this as the
/// default.
///
/// | Variant      | Wire format emitted                                       | Separate reasoning text? |
/// |--------------|-----------------------------------------------------------|--------------------------|
/// | `OpenAi`     | `reasoning_effort: "low"/"medium"/"high"`                 | No                       |
/// | `LlamaCpp`   | `chat_template_kwargs: { "enable_thinking": true }`       | Yes (reasoning_content)  |
/// | `LmStudio`   | same as `LlamaCpp`                                        | Yes (reasoning_content)  |
/// | `Ollama`     | `chat_template_kwargs: { "enable_thinking": true }`       | Yes (reasoning_content)  |
/// | `OpenRouter` | `reasoning: { "max_tokens": N }`                          | Yes (reasoning field)    |
/// | `Vllm`       | same as `LlamaCpp`                                        | Yes (reasoning_content)  |
/// | `Disabled`   | nothing, regardless of `enabled`                          | N/A                      |
/// | `Auto`       | inferred from base URL (see `OpenAiProvider`)             | per resolved kind        |
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningKind {
    /// Infer the correct kind from the `OPENAI_BASE_URL` at call time.
    #[default]
    Auto,
    /// OpenAI o-series models (o1, o3, o4-*) — emits `reasoning_effort`.
    OpenAi,
    /// llama-server (llama.cpp) — emits `chat_template_kwargs`.
    LlamaCpp,
    /// LM Studio — emits `chat_template_kwargs` (same shape as LlamaCpp).
    LmStudio,
    /// Ollama OpenAI-compat shim — emits `chat_template_kwargs`.
    Ollama,
    /// OpenRouter — emits `reasoning: { max_tokens }`.
    OpenRouter,
    /// vLLM — emits `chat_template_kwargs`.
    Vllm,
    /// Explicitly suppress all reasoning parameters, even when `enabled = Some(true)`.
    Disabled,
}

/// Everything a provider needs to make a single model call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRequest {
    pub system: String,
    pub user: String,
    /// Optional JSON schema for structured output.
    pub schema: Option<serde_json::Value>,
    pub temperature: f32,
    pub model_id: String,
    pub max_tokens: Option<u32>,
    /// Optional reasoning / thinking configuration.
    ///
    /// When `None` or `Some(ReasoningConfig { enabled: None/Some(false), .. })`
    /// the provider emits exactly the same request bytes as before this field
    /// existed — full backwards compatibility.
    #[serde(default)]
    pub reasoning: Option<ReasoningConfig>,
}

impl Default for ModelRequest {
    fn default() -> Self {
        Self {
            system: String::new(),
            user: String::new(),
            schema: None,
            temperature: 0.0,
            model_id: String::new(),
            max_tokens: None,
            reasoning: None,
        }
    }
}

/// Everything returned from a single model call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelResponse {
    pub text: String,
    pub parsed: Option<serde_json::Value>,
    pub model_id: String,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    /// Populated when the backend returned a separate reasoning / thinking
    /// field alongside the answer (e.g. DeepSeek-R1, QwQ, Qwen3-think,
    /// OpenRouter-routed reasoning models).  `None` when the backend either
    /// does not support separate reasoning or inlined it into `text`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_text: Option<String>,
}

#[async_trait::async_trait]
pub trait ModelProvider: Send + Sync {
    async fn call(&self, req: ModelRequest) -> Result<ModelResponse, String>;
}
