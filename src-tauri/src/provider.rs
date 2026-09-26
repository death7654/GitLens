//! Person 6 — Model provider abstraction (proposed interface).
//!
//! P3/P4 build against this trait only. Swapping Gemini↔Bob is a config
//! change on P6's side, not a code change on ours. If the trait shape drifts,
//! update these signatures; P3/P4 call sites don't change.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRequest {
    pub system: String,
    pub user: String,
    /// Optional JSON schema for structured output.
    pub schema: Option<serde_json::Value>,
    pub temperature: f32,
    pub model_id: String,
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelResponse {
    pub text: String,
    pub parsed: Option<serde_json::Value>,
    pub model_id: String,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
}

#[async_trait::async_trait]
pub trait ModelProvider: Send + Sync {
    async fn call(&self, req: ModelRequest) -> Result<ModelResponse, String>;
}