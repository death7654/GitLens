//! Test-only [`ModelProvider`] implementation.
//!
//! Delegates generic schema/prose requests to [`testing_api::dummy_response`],
//! which covers all request shapes (file summaries, commit summaries, ranking
//! with real hash echoing, and prose). Narration requests are handled by the
//! `MOCK_TRIGGER:` hint path below.
//!
//! ## Hint lines (all optional, embedded anywhere in `req.user`)
//!
//! | Hint                   | Effect                                                         |
//! |------------------------|----------------------------------------------------------------|
//! | `MOCK_TRIGGER: <kind>` | Returns canned narration JSON for the given `TriggerKind`.    |
//! | `MOCK_FAIL_HASH`       | Always returns `Err` — exercises the partial-failure UI path. |
//! | `MOCK_LATENCY_MS: <n>` | Sleeps `n` ms before responding — exercises loading states.   |
//!
//! `MOCK_FAIL_HASH` is checked first so it fires regardless of which other
//! hints are present. `MOCK_TRIGGER:` parsing and canned responses are
//! confined to this file and do not touch `TriggerKind` or production types.
//!
//! Activate at runtime by setting `GITLENS_MOCK_PROVIDER=1` (see `lib.rs`).

use crate::provider::{ModelProvider, ModelRequest, ModelResponse};
use crate::testing_api;

/// Hardcoded commit hash that always causes [`MockProvider::call`] to return
/// `Err`, exercising the partial-failure UI path.
///
/// The value is an all-zeros SHA-1 suffix with a `1` at the end, which is
/// not a valid git object hash in any real repository. Tests that deliberately
/// trigger errors should embed this string anywhere in `req.user`.
pub const MOCK_FAIL_HASH: &str = "0000000000000000000000000000000000000001";

pub struct MockProvider;

// ---------------------------------------------------------------------------
// Canned narration responses
// ---------------------------------------------------------------------------

/// Returns a canned-but-realistic `{"title": ..., "narration": ...}` JSON
/// value for the given trigger kind (snake_case). Used only by `MockProvider`.
fn narration_for_trigger(trigger: &str) -> serde_json::Value {
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
        // Explicit catch-all: unknown trigger kinds still return a valid shape
        // so downstream JSON parsing never fails, but the title signals the miss.
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

// ---------------------------------------------------------------------------
// Hint extraction
// ---------------------------------------------------------------------------

/// Extract the value after `<prefix>` on any line of `text`.
///
/// Trims leading/trailing whitespace from both the prefix match and the value.
/// Returns `None` if no line starts with `prefix`.
fn extract_hint<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    text.lines().find_map(|line| {
        line.trim()
            .strip_prefix(prefix)
            .map(str::trim)
    })
}

// ---------------------------------------------------------------------------
// ModelProvider impl
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
impl ModelProvider for MockProvider {
    async fn call(&self, req: ModelRequest) -> Result<ModelResponse, String> {
        // ── Guard: deliberate error path ─────────────────────────────────────
        // Checked first so it fires regardless of which other hints are present.
        if req.user.contains(MOCK_FAIL_HASH) {
            return Err(format!(
                "mock: deliberate failure for demo (hash: {MOCK_FAIL_HASH})"
            ));
        }

        // ── Optional latency simulation ──────────────────────────────────────
        if let Some(ms_str) = extract_hint(&req.user, "MOCK_LATENCY_MS:") {
            if let Ok(ms) = ms_str.parse::<u64>() {
                tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
            }
        }

        // ── Narration path ───────────────────────────────────────────────────
        if let Some(trigger) = extract_hint(&req.user, "MOCK_TRIGGER:") {
            let v = narration_for_trigger(trigger);
            let text = serde_json::to_string(&v).unwrap_or_default();
            return Ok(ModelResponse {
                text,
                parsed: Some(v),
                model_id: req.model_id,
                input_tokens: None,
                output_tokens: None,
            });
        }

        // ── Generic schema / prose path ──────────────────────────────────────
        // Delegates to `testing_api::dummy_response`, which handles all schema
        // variants (ranking with hash echoing, file summaries, commit summaries,
        // prose) in a single authoritative place.
        let resp = testing_api::dummy_response(&req);
        Ok(resp)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

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

    // -- Narration triggers --------------------------------------------------

    #[tokio::test]
    async fn narration_revert_trigger_has_title_and_narration() {
        let mut req = base_req();
        req.user = "context\nMOCK_TRIGGER: revert\nCOMMIT_HASH: abc123".to_string();
        let resp = MockProvider.call(req).await.unwrap();
        let p = resp.parsed.unwrap();
        let title = p["title"].as_str().unwrap_or("");
        let narration = p["narration"].as_str().unwrap_or("");
        assert!(!title.is_empty(), "title must be non-empty");
        assert!(!narration.is_empty(), "narration must be non-empty");
    }

    #[tokio::test]
    async fn narration_all_known_trigger_kinds_produce_valid_shapes() {
        let triggers = [
            "revert",
            "was_reverted",
            "repeated_fix",
            "incident_linked",
            "architecture_shaping",
        ];
        for trigger in triggers {
            let mut req = base_req();
            req.user = format!("MOCK_TRIGGER: {trigger}");
            let resp = MockProvider.call(req).await.unwrap_or_else(|e| {
                panic!("trigger '{trigger}' returned Err: {e}")
            });
            let p = resp.parsed.unwrap();
            assert!(
                p["title"].as_str().map(|s| !s.is_empty()).unwrap_or(false),
                "trigger '{trigger}' produced empty or missing title"
            );
            assert!(
                p["narration"].as_str().map(|s| !s.is_empty()).unwrap_or(false),
                "trigger '{trigger}' produced empty or missing narration"
            );
        }
    }

    #[tokio::test]
    async fn narration_unknown_trigger_falls_to_catch_all() {
        let mut req = base_req();
        req.user = "MOCK_TRIGGER: completely_unknown_kind".to_string();
        let resp = MockProvider.call(req).await.unwrap();
        let p = resp.parsed.unwrap();
        // The catch-all must still return a valid narration shape.
        assert!(p["title"].as_str().is_some());
        assert!(p["narration"].as_str().is_some());
    }

    #[tokio::test]
    async fn narration_text_field_matches_serialised_parsed() {
        let mut req = base_req();
        req.user = "MOCK_TRIGGER: revert".to_string();
        let resp = MockProvider.call(req).await.unwrap();
        // `text` must be a valid re-parse of `parsed`.
        let reparsed: serde_json::Value =
            serde_json::from_str(&resp.text).expect("resp.text must be valid JSON");
        assert_eq!(reparsed, resp.parsed.unwrap());
    }

    // -- Fail-hash guard -----------------------------------------------------

    #[tokio::test]
    async fn fail_hash_returns_err_with_trigger() {
        let mut req = base_req();
        req.user = format!("MOCK_TRIGGER: incident_linked\nCOMMIT_HASH: {MOCK_FAIL_HASH}");
        let err = MockProvider.call(req).await.unwrap_err();
        assert!(err.contains("deliberate failure"), "error must mention deliberate failure");
        assert!(err.contains(MOCK_FAIL_HASH));
    }

    #[tokio::test]
    async fn fail_hash_returns_err_without_trigger() {
        // MOCK_FAIL_HASH fires even when no MOCK_TRIGGER: hint is present.
        let mut req = base_req();
        req.user = format!("COMMIT_HASH: {MOCK_FAIL_HASH}");
        let err = MockProvider.call(req).await.unwrap_err();
        assert!(err.contains("deliberate failure"));
    }

    // -- Latency hint --------------------------------------------------------

    #[tokio::test]
    async fn latency_hint_delays_response() {
        let mut req = base_req();
        req.user = "MOCK_LATENCY_MS: 50\nMOCK_TRIGGER: revert".to_string();
        let start = std::time::Instant::now();
        MockProvider.call(req).await.unwrap();
        assert!(
            start.elapsed() >= std::time::Duration::from_millis(40),
            "should have slept at least 40 ms"
        );
    }

    #[tokio::test]
    async fn invalid_latency_hint_is_ignored() {
        // A non-numeric latency value must not panic or error.
        let mut req = base_req();
        req.user = "MOCK_LATENCY_MS: not_a_number\nMOCK_TRIGGER: revert".to_string();
        MockProvider.call(req).await.unwrap();
    }

    // -- Generic schema / prose delegation -----------------------------------

    #[tokio::test]
    async fn schema_path_fills_candidates_with_hash_echoing() {
        // Without MOCK_TRIGGER:, ranking requests must echo hashes from the prompt.
        let req = ModelRequest {
            system: "rank".to_string(),
            user: "CANDIDATES:\n- hash: abc\n  subject: fix\n- hash: def\n  subject: feat\n"
                .to_string(),
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
        let p = resp.parsed.unwrap();
        let arr = p["candidates"].as_array().expect("candidates must be an array");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["commit_hash"].as_str(), Some("abc"));
        assert_eq!(arr[1]["commit_hash"].as_str(), Some("def"));
    }

    #[tokio::test]
    async fn schema_path_fills_file_summary_fields() {
        let req = ModelRequest {
            system: "summarise".to_string(),
            user: "File: foo.rs".to_string(),
            schema: Some(serde_json::json!({
                "required": ["provides", "externals", "purpose", "functionalities", "notes"]
            })),
            temperature: 0.0,
            model_id: "mock".to_string(),
            max_tokens: None,
        };
        let resp = MockProvider.call(req).await.unwrap();
        let p = resp.parsed.unwrap();
        assert!(p["provides"].as_str().is_some());
        assert!(p["purpose"].as_str().is_some());
        assert!(p["externals"].as_array().is_some());
    }

    #[tokio::test]
    async fn prose_path_has_text_and_no_parsed() {
        let req = ModelRequest {
            system: "prose".to_string(),
            user: "Subsystem: auth".to_string(),
            schema: None,
            temperature: 0.0,
            model_id: "mock".to_string(),
            max_tokens: None,
        };
        let resp = MockProvider.call(req).await.unwrap();
        assert!(!resp.text.is_empty());
        assert!(resp.parsed.is_none(), "prose responses must have parsed = None");
    }

    #[tokio::test]
    async fn model_id_is_propagated() {
        let mut req = base_req();
        req.model_id = "test-model-xyz".to_string();
        req.user = "MOCK_TRIGGER: revert".to_string();
        let resp = MockProvider.call(req).await.unwrap();
        assert_eq!(resp.model_id, "test-model-xyz");
    }
}
