//! Person 3 — test-only `ModelProvider`.
//!
//! Returns a shape that matches each request's schema by filling every
//! `required` field with a placeholder. Good enough to smoke-test the
//! pipeline end-to-end before P6's real provider lands. NOT for production.
//!
//! NARRATION EXTENSION (Workstream A):
//! When the user prompt contains a `MOCK_TRIGGER:` hint line, the provider
//! returns canned-but-realistic narration JSON keyed by trigger kind.
//! If the prompt also contains `MOCK_FAIL_HASH` (the constant defined below),
//! the call returns `Err` — allowing the partial-failure UI state to be
//! triggered deliberately on camera.
//!
//! The `MOCK_TRIGGER:` parsing and canned responses live ONLY in this file.
//! They must not leak into `TriggerKind` or any production schema.

use crate::provider::{ModelProvider, ModelRequest, ModelResponse};

/// Hardcoded hash that always causes `MockProvider::call` to return `Err`
/// when a `MOCK_TRIGGER:` hint is present. Workstream D must ensure this hash
/// is produced by a fixture commit with pinned dates so it is stable across
/// machines.
pub const MOCK_FAIL_HASH: &str = "0000000000000000000000000000000000000001";

pub struct MockProvider;

/// Returns a canned-but-realistic `{"title": ..., "narration": ...}` JSON
/// value for the given trigger kind (snake_case). Used only by `MockProvider`.
fn narration_response_for_trigger(trigger: &str) -> serde_json::Value {
    match trigger {
        "revert" => serde_json::json!({
            "title": "The rollback that revealed the real dependency",
            "narration": "What looked like a straightforward feature addition turned into a two-day \
                          incident when it reached the staging environment. The commit introduced a \
                          subtle ordering assumption between the initialisation of two subsystems \
                          that had always been true in development but broke under the staging \
                          deployment sequence. The revert was raised within four hours of the deploy.\n\n\
                          The rollback itself is less interesting than what it uncovered: the \
                          dependency between those two subsystems had never been documented, \
                          and the team had not written an integration test that covered the \
                          startup sequence. Both gaps were addressed in the commits that followed \
                          this one, making this revert the catalyst for a more robust initialisation \
                          contract — look at the next two stops to see how the fix landed."
        }),
        "was_reverted" => serde_json::json!({
            "title": "The change that didn't survive contact with production",
            "narration": "This commit represents a decision that seemed sound in isolation but \
                          proved brittle under real load. The approach it introduced — caching \
                          query results at the handler layer rather than the service layer — \
                          worked correctly in unit tests but produced stale reads when multiple \
                          replicas ran concurrently. It was reverted within 24 hours.\n\n\
                          Understanding why this change failed is as valuable as understanding \
                          why the replacement succeeded. The mismatch between the mental model \
                          and the actual deployment topology is a recurring theme in this \
                          codebase; the architecture decision record referenced in the revert \
                          commit captures the team's revised thinking."
        }),
        "repeated_fix" => serde_json::json!({
            "title": "The file that kept breaking — and what finally stuck",
            "narration": "By the time this commit landed, the connection pool initialisation \
                          code had been patched three times in six months. Each fix addressed \
                          the immediate symptom but left the underlying cause intact: the \
                          pool configuration was read at startup rather than resolved lazily, \
                          which meant environment-variable substitution happened before the \
                          secrets manager had finished populating values.\n\n\
                          This commit is the one that broke the cycle. It moved configuration \
                          resolution to the first actual use of the pool, added a test that \
                          reproduces the race, and added a comment explaining why the lazy \
                          pattern is required here. The test has caught two regressions since \
                          this was merged."
        }),
        "incident_linked" => serde_json::json!({
            "title": "The incident that hardened the retry path",
            "narration": "INC-4821 was a forty-minute partial outage caused by a downstream \
                          service returning 429s during a traffic spike. The existing retry \
                          logic used a fixed one-second delay, which created a thundering-herd \
                          pattern that prolonged the outage rather than letting the downstream \
                          recover. This commit replaced the fixed delay with truncated \
                          exponential backoff and added per-host jitter.\n\n\
                          The incident review that preceded this commit is worth reading in \
                          full if you have access to it — it documents not just what went \
                          wrong but the reasoning behind the specific backoff parameters \
                          chosen here (base 200 ms, multiplier 2, cap at 16 s, jitter ±20%). \
                          Those numbers were derived from the observed recovery time of the \
                          downstream service during the incident, not from first principles."
        }),
        "architecture_shaping" => serde_json::json!({
            "title": "The refactor that drew the new boundary",
            "narration": "Before this commit, the authentication logic was split across three \
                          files with no clear owner: some of it lived in the HTTP middleware, \
                          some in the user service, and a surprising amount in a utility module \
                          that had grown organically. Every time authentication behaviour needed \
                          to change, at least two of those three files moved together.\n\n\
                          This refactor pulled all of that logic into a single \
                          `auth` module, defined a stable internal API, and updated the three \
                          callers to use it. The immediate benefit was a clean boundary; the \
                          longer-term benefit is visible in the commit history after this \
                          point — authentication changes became single-file commits, and the \
                          number of auth-related bug fixes dropped by roughly half in the \
                          following quarter."
        }),
        _ => serde_json::json!({
            "title": "A pivotal moment in the codebase's evolution",
            "narration": "This commit represents a decision point that shaped subsequent \
                          development in ways that weren't fully visible at the time. \
                          The change it introduced addressed an immediate practical problem, \
                          but it also established a pattern that the rest of the team \
                          followed — sometimes explicitly, sometimes by imitation.\n\n\
                          Looking at the commits that came after this one, you can trace how \
                          the approach it introduced spread through the codebase. Whether \
                          that spread was intentional or accidental is a question worth \
                          asking the person who authored this commit — the git history \
                          records what changed, but the context that made this the right \
                          change at the right moment lives in the team's memory."
        }),
    }
}

/// Extract the value of a line starting with `prefix:` from the given text.
fn extract_hint(text: &str, prefix: &str) -> Option<String> {
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            return Some(rest.trim().to_string());
        }
    }
    None
}

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
        // Check for MOCK_TRIGGER hint — narration path.
        if let Some(trigger) = extract_hint(&req.user, "MOCK_TRIGGER:") {
            // Check whether the prompt contains MOCK_FAIL_HASH.
            if req.user.contains(MOCK_FAIL_HASH) {
                return Err(format!(
                    "mock: deliberate failure for demo (hash: {})",
                    MOCK_FAIL_HASH
                ));
            }

            let v = narration_response_for_trigger(&trigger);
            let text = serde_json::to_string(&v).unwrap_or_default();
            return Ok(ModelResponse {
                text,
                parsed: Some(v),
                model_id: req.model_id,
                input_tokens: None,
                output_tokens: None,
            });
        }

        // Fall-through: generic schema-based placeholder logic (for ranking etc.)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ModelRequest;

    fn base_req() -> ModelRequest {
        ModelRequest {
            system: "narrate".to_string(),
            user: String::new(),
            schema: None,
            temperature: 0.3,
            model_id: "mock".to_string(),
            max_tokens: Some(512),
        }
    }

    #[tokio::test]
    async fn test_mock_narration_revert_trigger() {
        let mut req = base_req();
        req.user = "some context\nMOCK_TRIGGER: revert\nCOMMIT_HASH_FOR_MOCK: abc123".to_string();
        let resp = MockProvider.call(req).await.unwrap();
        let parsed = resp.parsed.unwrap();
        assert!(parsed.get("title").is_some());
        assert!(parsed.get("narration").is_some());
        let title = parsed["title"].as_str().unwrap();
        assert!(!title.is_empty(), "title should be non-empty");
    }

    #[tokio::test]
    async fn test_mock_narration_fail_hash() {
        let mut req = base_req();
        req.user = format!(
            "MOCK_TRIGGER: incident_linked\nCOMMIT_HASH_FOR_MOCK: {}",
            MOCK_FAIL_HASH
        );
        let result = MockProvider.call(req).await;
        assert!(result.is_err(), "MOCK_FAIL_HASH should always return Err");
        let err = result.unwrap_err();
        assert!(err.contains("deliberate failure"), "error should mention deliberate failure");
        assert!(err.contains(MOCK_FAIL_HASH));
    }

    #[tokio::test]
    async fn test_mock_narration_all_trigger_kinds() {
        for trigger in &["revert", "was_reverted", "repeated_fix", "incident_linked", "architecture_shaping", "unknown"] {
            let mut req = base_req();
            req.user = format!("MOCK_TRIGGER: {trigger}\nCOMMIT_HASH_FOR_MOCK: goodhash");
            let resp = MockProvider.call(req).await.unwrap();
            let parsed = resp.parsed.unwrap();
            assert!(parsed["title"].as_str().map(|s| !s.is_empty()).unwrap_or(false),
                "trigger '{trigger}' should produce a non-empty title");
        }
    }

    #[tokio::test]
    async fn test_mock_fallback_schema_path() {
        // When no MOCK_TRIGGER is present, falls through to the schema-based path.
        let req = ModelRequest {
            system: "rank".to_string(),
            user: "some prompt without trigger".to_string(),
            schema: Some(serde_json::json!({
                "type": "object",
                "required": ["candidates"],
                "properties": {"candidates": {"type": "array"}}
            })),
            temperature: 0.1,
            model_id: "mock".to_string(),
            max_tokens: Some(1024),
        };
        let resp = MockProvider.call(req).await.unwrap();
        let parsed = resp.parsed.unwrap();
        assert!(parsed.get("candidates").is_some(), "should fill 'candidates' from schema");
    }
}
