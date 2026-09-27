//! Test-only dummy response layer used by `MockProvider`.
//!
//! Every function here takes a `ModelRequest` and returns a `ModelResponse`
//! whose `parsed` value exactly matches the shape the calling stage expects:
//!
//! | Caller                    | Schema `required` fields                                    |
//! |---------------------------|-------------------------------------------------------------|
//! | `summarize_file`          | provides, externals, purpose, functionalities, notes        |
//! | `summarize_commit`        | what_changed, why, takeaway                                 |
//! | `build_project_summary`   | none (prose, no schema)                                     |
//! | `run_ranking` (stage 4)   | candidates (array of per-commit objects)                    |
//!
//! The ranking response is the only one that requires content awareness: the
//! provider must echo back the candidate hashes that were embedded in the
//! prompt so that `parse_ranking_response` and `post_process` have real
//! hashes to work with instead of an empty array.

use crate::provider::{ModelRequest, ModelResponse};

// ---------- public entry point ----------

/// Produce a dummy `ModelResponse` appropriate for `req`.
///
/// Dispatch logic — all branches keyed on the request's JSON schema so that
/// renaming a system-prompt string never silently misdirects the mock:
/// - ranking call   → schema `required` contains `"candidates"`
/// - file summary   → schema `required` contains `"provides"`
/// - commit summary → schema `required` contains `"what_changed"`
/// - prose call     → no schema (project summary subsystem / repo passes)
pub fn dummy_response(req: &ModelRequest) -> ModelResponse {
    if schema_requires(req, "candidates") {
        ranking_response(req)
    } else if schema_requires(req, "provides") {
        file_summary_response(req)
    } else if schema_requires(req, "what_changed") {
        commit_summary_response(req)
    } else {
        prose_response(req)
    }
}

// ---------- per-stage dummy builders ----------

/// Stage 4 — ranking response.
///
/// Parses every `- hash: <value>` line out of the prompt and returns one
/// scored candidate object per hash, so that `parse_ranking_response` and
/// `post_process` receive non-empty input rather than an empty array.
fn ranking_response(req: &ModelRequest) -> ModelResponse {
    let hashes = extract_hashes_from_prompt(&req.user);

    let candidates: Vec<serde_json::Value> = hashes
        .iter()
        .enumerate()
        .map(|(i, hash)| {
            // Scores descend from 0.9 so the first hash always ranks highest.
            let score = 0.9 - (i as f64 * 0.05).min(0.8);
            serde_json::json!({
                "commit_hash":          hash,
                "score":                score,
                "architecture_shaping": false,
                "rationale":            format!("[mock rationale for {}]", hash),
                "evidence_summary":     format!("[mock evidence for {}]", hash)
            })
        })
        .collect();

    let parsed = serde_json::json!({ "candidates": candidates });
    let text = serde_json::to_string(&parsed).unwrap_or_default();
    ModelResponse {
        text,
        parsed: Some(parsed),
        model_id: req.model_id.clone(),
        input_tokens: None,
        output_tokens: None,
    }
}

/// Stage 1 — file summary response.
///
/// Returns a `FileSummary`-shaped object: all string fields get placeholder
/// text; array fields get an empty array.
fn file_summary_response(req: &ModelRequest) -> ModelResponse {
    let parsed = serde_json::json!({
        "provides":        "[mock provides]",
        "externals":       [],
        "purpose":         "[mock purpose]",
        "functionalities": [],
        "notes":           "[mock notes]"
    });
    let text = serde_json::to_string(&parsed).unwrap_or_default();
    ModelResponse {
        text,
        parsed: Some(parsed),
        model_id: req.model_id.clone(),
        input_tokens: None,
        output_tokens: None,
    }
}

/// Stage 2 — commit summary response.
///
/// Returns a `CommitSummary`-shaped object.
fn commit_summary_response(req: &ModelRequest) -> ModelResponse {
    let parsed = serde_json::json!({
        "what_changed": "[mock what_changed]",
        "why":          "[mock why]",
        "takeaway":     "[mock takeaway]"
    });
    let text = serde_json::to_string(&parsed).unwrap_or_default();
    ModelResponse {
        text,
        parsed: Some(parsed),
        model_id: req.model_id.clone(),
        input_tokens: None,
        output_tokens: None,
    }
}

/// Stage 3 (subsystem + project summary) — plain prose response.
///
/// No schema is attached to these calls; the result is used as a raw string.
/// `parsed` is `None` here: callers that use the `parsed.or_else(text)` fallback
/// pattern would receive a bare `Value::String` and silently get `None` on any
/// `.get("field")` access, masking dispatch bugs. Setting it to `None` forces
/// such callers to fall through to the `serde_json::from_str(&resp.text)` path,
/// which also returns `None` for a plain prose string — the correct behaviour.
fn prose_response(req: &ModelRequest) -> ModelResponse {
    let text = format!(
        "[mock prose for {}]",
        req.user.lines().next().unwrap_or("unknown")
    );
    ModelResponse {
        text,
        parsed: None,
        model_id: req.model_id.clone(),
        input_tokens: None,
        output_tokens: None,
    }
}

// ---------- helpers ----------

/// Returns `true` if the request schema lists `field_name` in `"required"`.
fn schema_requires(req: &ModelRequest, field_name: &str) -> bool {
    req.schema
        .as_ref()
        .and_then(|s| s.get("required"))
        .and_then(|r| r.as_array())
        .map(|arr| arr.iter().any(|v| v.as_str() == Some(field_name)))
        .unwrap_or(false)
}

/// Extracts candidate commit hashes from a ranking prompt.
///
/// The prompt format written by `build_ranking_prompt` is:
/// ```text
/// - hash: <hash>
///   subject: ...
/// ```
/// This function grabs every `<hash>` value from those lines.
fn extract_hashes_from_prompt(prompt: &str) -> Vec<String> {
    prompt
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            trimmed
                .strip_prefix("- hash: ")
                .map(|h| h.trim().to_string())
        })
        .collect()
}

// ---------- tests ----------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ModelRequest;

    fn req(system: &str, user: &str, schema: Option<serde_json::Value>) -> ModelRequest {
        ModelRequest {
            system: system.into(),
            user: user.into(),
            schema,
            temperature: 0.0,
            model_id: "test-model".into(),
            max_tokens: None,
        }
    }

    #[test]
    fn file_summary_returns_all_required_fields() {
        let schema = serde_json::json!({
            "required": ["provides", "externals", "purpose", "functionalities", "notes"]
        });
        let resp = dummy_response(&req(
            "You summarise source files for a codebase tour. Return strict JSON.",
            "File: foo.rs\n",
            Some(schema),
        ));
        let p = resp.parsed.unwrap();
        assert!(p.get("provides").and_then(|v| v.as_str()).is_some());
        assert!(p.get("purpose").and_then(|v| v.as_str()).is_some());
        assert!(p.get("externals").and_then(|v| v.as_array()).is_some());
        assert!(p.get("functionalities").and_then(|v| v.as_array()).is_some());
        assert!(p.get("notes").and_then(|v| v.as_str()).is_some());
    }

    #[test]
    fn commit_summary_returns_all_required_fields() {
        let schema = serde_json::json!({
            "required": ["what_changed", "why", "takeaway"]
        });
        let resp = dummy_response(&req(
            "You summarise git commits for a new-hire onboarding tour. Return strict JSON.",
            "Commit: abc123\n",
            Some(schema),
        ));
        let p = resp.parsed.unwrap();
        assert!(p.get("what_changed").and_then(|v| v.as_str()).is_some());
        assert!(p.get("why").and_then(|v| v.as_str()).is_some());
        assert!(p.get("takeaway").and_then(|v| v.as_str()).is_some());
    }

    #[test]
    fn ranking_response_echoes_hashes_from_prompt() {
        let prompt = "CANDIDATES:\n- hash: abc\n  subject: fix\n- hash: def\n  subject: feat\n";
        let schema = serde_json::json!({ "required": ["candidates"] });
        let resp = dummy_response(&req(
            "You rank git commits for onboarding tours. Return strict JSON.",
            prompt,
            Some(schema),
        ));
        let p = resp.parsed.unwrap();
        let arr = p.get("candidates").and_then(|v| v.as_array()).unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(
            arr[0].get("commit_hash").and_then(|v| v.as_str()),
            Some("abc")
        );
        assert_eq!(
            arr[1].get("commit_hash").and_then(|v| v.as_str()),
            Some("def")
        );
    }

    #[test]
    fn ranking_response_scores_descend() {
        let prompt = "CANDIDATES:\n- hash: h1\n  subject: a\n- hash: h2\n  subject: b\n";
        let schema = serde_json::json!({ "required": ["candidates"] });
        let resp = dummy_response(&req(
            "You rank git commits for onboarding tours. Return strict JSON.",
            prompt,
            Some(schema),
        ));
        let arr = resp.parsed.unwrap();
        let arr = arr.get("candidates").and_then(|v| v.as_array()).unwrap();
        let s0 = arr[0].get("score").and_then(|v| v.as_f64()).unwrap();
        let s1 = arr[1].get("score").and_then(|v| v.as_f64()).unwrap();
        assert!(s0 > s1, "first candidate should have higher score");
    }

    #[test]
    fn prose_response_has_text_and_no_parsed_value() {
        // Prose calls carry no schema; parsed must be None so that callers using
        // the `parsed.or_else(from_str(text))` fallback don't receive a
        // Value::String and misinterpret it as a structured JSON object.
        let resp = dummy_response(&req(
            "You summarise code subsystems. Return plain prose, not JSON.",
            "Subsystem: auth\n",
            None,
        ));
        assert!(resp.text.contains("[mock prose"), "text should contain mock prose");
        assert!(resp.parsed.is_none(), "parsed must be None for prose responses");
    }

    #[test]
    fn extract_hashes_empty_when_no_candidates() {
        let hashes = extract_hashes_from_prompt("no candidate lines here");
        assert!(hashes.is_empty());
    }

    #[test]
    fn extract_hashes_ignores_non_hash_lines() {
        // Lines like `  subject: fix` must not be captured.
        let prompt = "- hash: abc123\n  subject: fix something\n- hash: def456\n";
        let hashes = extract_hashes_from_prompt(prompt);
        assert_eq!(hashes, vec!["abc123", "def456"]);
    }
}
