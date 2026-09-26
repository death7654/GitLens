//! Person 3 — test-only `ModelProvider`.
//!
//! Returns a shape that matches each request's schema by filling every
//! `required` field with a placeholder. Good enough to smoke-test the
//! pipeline end-to-end before P6's real provider lands. NOT for production.

use crate::provider::{ModelProvider, ModelRequest, ModelResponse};

pub struct MockProvider;

fn placeholder(name: &str) -> serde_json::Value {
    use serde_json::Value;
    match name {
        "provides" | "purpose" | "notes" | "what_changed" | "why" | "takeaway"
        | "rationale" | "evidence_summary" | "summary" | "text" => Value::String(format!("[mock {name}]")),
        "externals" | "functionalities" => Value::Array(vec![]),
        "score" => Value::Number(serde_json::Number::from_f64(0.5).unwrap()),
        "architecture_shaping" => Value::Bool(false),
        "candidates" => Value::Array(vec![]),
        _ => Value::String(format!("[mock {name}]")),
    }
}

#[async_trait::async_trait]
impl ModelProvider for MockProvider {
    async fn call(&self, req: ModelRequest) -> Result<ModelResponse, String> {
        let v = match req.schema.as_ref()
            .and_then(|s| s.get("required"))
            .and_then(|r| r.as_array())
        {
            Some(fields) => {
                let mut obj = serde_json::Map::new();
                for f in fields {
                    let name = f.as_str().unwrap_or("");
                    obj.insert(name.to_string(), placeholder(name));
                }
                serde_json::Value::Object(obj)
            }
            None => serde_json::Value::String(format!("[mock prose for {}]", req.user.lines().next().unwrap_or(""))),
        };
        let text = match &v {
            serde_json::Value::String(s) => s.clone(),
            _ => serde_json::to_string(&v).unwrap_or_default(),
        };
        Ok(ModelResponse {
            text,
            parsed: Some(v),
            model_id: req.model_id,
            input_tokens: None,
            output_tokens: None,
        })
    }
}