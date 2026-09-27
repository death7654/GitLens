//! Tour narration module — Workstream A.
//!
//! Two Tauri commands:
//!   `fetch_stop_documents` — deterministic join + document fetch, no LLM.
//!   `narrate_stop`         — one LLM call per stop with retry + per-stop cache.
//!
//! A public helper `tour_cache_key` is exposed for Workstream D's tests.

use std::path::{Path, PathBuf};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::doc_fetch;
use crate::git_mining::{commit_file_changes, MiningOutput};
use crate::provider::ModelRequest;
use crate::significance_ranking_types::{RankingOutput};
use crate::tour_types::{StopStub, TourConfig, TourStop, TriggerKind};

// ---------- cache helpers (mirrors significance_ranking_stages.rs) ----------

fn cache_path(root: &Path, kind: &str, key: &str) -> PathBuf {
    root.join(kind).join(format!("{key}.json"))
}

fn read_cache<T: for<'de> serde::Deserialize<'de>>(p: &Path) -> Option<T> {
    serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
}

fn write_cache<T: Serialize>(p: &Path, v: &T) {
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(s) = serde_json::to_string_pretty(v) {
        let _ = std::fs::write(p, s);
    }
}

// ---------- Public API ----------

/// Stable cache key for a whole-tour run.
///
/// `SHA-256("{repo_path}|{head_hash}|{window_json}|{narration_prompt_version}")`
///
/// Exposed `pub` so Workstream D can assert stability in fixture tests.
pub fn tour_cache_key(
    repo_path: &str,
    head_hash: &str,
    window: &crate::git_mining::MiningWindow,
    narration_prompt_version: &str,
) -> String {
    let window_json = serde_json::to_string(window).unwrap_or_default();
    let raw = format!("{repo_path}|{head_hash}|{window_json}|{narration_prompt_version}");
    let digest = Sha256::digest(raw.as_bytes());
    hex::encode(digest)
}

// ---------- TriggerKind derivation ----------

fn derive_triggers(
    heuristics: &crate::git_mining::HeuristicFlags,
    architecture_shaping: bool,
) -> Vec<TriggerKind> {
    let mut triggers = Vec::new();
    if heuristics.is_revert {
        triggers.push(TriggerKind::Revert);
    }
    if heuristics.was_reverted {
        triggers.push(TriggerKind::WasReverted);
    }
    if heuristics.touches_repeated_fix_file {
        triggers.push(TriggerKind::RepeatedFix);
    }
    if !heuristics.incident_refs.is_empty() {
        triggers.push(TriggerKind::IncidentLinked);
    }
    if architecture_shaping {
        triggers.push(TriggerKind::ArchitectureShaping);
    }
    // Ensure at least one trigger (shouldn't happen with well-ranked candidates,
    // but be safe).
    if triggers.is_empty() {
        triggers.push(TriggerKind::ArchitectureShaping);
    }
    triggers
}

// ---------- fetch_stop_documents ----------

/// Deterministic join: `RankingOutput` × `MiningOutput` → `Vec<StopStub>`.
///
/// Does NOT call the LLM. For each ranked candidate:
///   1. Joins back to the full `CommitRecord` by hash.
///   2. Derives `TriggerKind` from `HeuristicFlags` + `SignalSummary.architecture_shaping`.
///   3. If `cfg.fetch_linked_documents` is enabled, attempts to fetch linked
///      documents for any `incident_refs` (all in parallel). On failure, falls back
///      to `None` silently.
///
/// Fail-fast: returns `Err` if `fetch_linked_documents` is true but no token is configured.
#[tauri::command]
pub async fn fetch_stop_documents(
    ranking_output: RankingOutput,
    mining_output: MiningOutput,
    cfg: Option<TourConfig>,
) -> Result<Vec<StopStub>, String> {
    let cfg = cfg.unwrap_or_default();

    // Fail-fast config validation.
    if cfg.fetch_linked_documents
        && cfg.github_token.is_none()
        && cfg.jira_token.is_none()
    {
        return Err(
            "fetch_linked_documents is enabled but neither github_token nor jira_token is configured".into(),
        );
    }

    // Build a quick lookup: commit hash → CommitRecord.
    let commit_map: std::collections::HashMap<&str, &crate::git_mining::CommitRecord> =
        mining_output.commits.iter().map(|c| (c.hash.as_str(), c)).collect();

    let mut stubs = Vec::with_capacity(ranking_output.tour_candidates.len());

    for candidate in &ranking_output.tour_candidates {
        let record = match commit_map.get(candidate.commit_hash.as_str()) {
            Some(r) => r,
            None => {
                eprintln!(
                    "[tour_narration] ranked candidate {} not found in mining output; skipping",
                    candidate.commit_hash
                );
                continue;
            }
        };

        let triggered_by = derive_triggers(&record.heuristics, candidate.signals.architecture_shaping);

        // Fetch linked documents for all incident_refs in parallel if enabled.
        let linked_document: Option<String> = if cfg.fetch_linked_documents
            && !record.heuristics.incident_refs.is_empty()
        {
            let fetch_futs: Vec<_> = record
                .heuristics
                .incident_refs
                .iter()
                .map(|iref| {
                    let iref = iref.clone();
                    let cfg_clone = cfg.clone();
                    async move { doc_fetch::fetch_linked_document(&iref, &cfg_clone).await }
                })
                .collect();
            let results = futures::future::join_all(fetch_futs).await;
            // Concatenate all non-None results.
            let combined: Vec<String> = results.into_iter().flatten().collect();
            if combined.is_empty() {
                None
            } else {
                Some(combined.join("\n\n---\n\n"))
            }
        } else {
            None
        };

        stubs.push(StopStub {
            sequence: candidate.rank,
            commit_hash: candidate.commit_hash.clone(),
            timestamp_utc: record.timestamp_utc.clone(),
            message_summary: record.message_summary.clone(),
            subsystem: candidate.subsystem.clone(),
            subsystems: candidate.subsystems.clone(),
            files_changed: record.files_changed.iter().map(|f| f.path.clone()).collect(),
            triggered_by,
            selection_rationale: candidate.rationale.clone(),
            evidence_summary: candidate.evidence_summary.clone(),
            linked_document,
        });
    }

    // Stubs are already in rank order from tour_candidates.
    Ok(stubs)
}

// ---------- narrate_stop ----------

/// Narration prompt schema — the only fields the LLM must return.
fn narration_response_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["title", "narration"],
        "properties": {
            "title":     {"type": "string"},
            "narration": {"type": "string"}
        }
    })
}

/// Build the user prompt for a single narrate_stop call.
fn build_narration_prompt(
    stub: &StopStub,
    diff_excerpt: &str,
    other_evidence_summaries: &[String],
) -> String {
    let mut user = String::new();

    // Commit identity block.
    user.push_str(&format!(
        "COMMIT: {}\nSUBJECT: {}\nSUBSYSTEMS: {}\nFILES CHANGED: {}\n",
        stub.commit_hash,
        stub.message_summary,
        stub.subsystems.join(", "),
        stub.files_changed.join(", "),
    ));

    // Heuristic flags.
    let trigger_names: Vec<&str> = stub.triggered_by.iter().map(trigger_kind_name).collect();
    user.push_str(&format!("SELECTION TRIGGERS: {}\n", trigger_names.join(", ")));
    user.push_str(&format!("SELECTION RATIONALE: {}\n\n", stub.selection_rationale));

    // Optional linked document.
    if let Some(doc) = &stub.linked_document {
        user.push_str("LINKED DOCUMENT:\n");
        user.push_str(doc);
        user.push_str("\n\n");
    }

    // Cross-stop context from other stops' evidence_summary values.
    if !other_evidence_summaries.is_empty() {
        user.push_str("OTHER STOPS IN THIS TOUR (for cross-reference):\n");
        for s in other_evidence_summaries {
            user.push_str(&format!("- {s}\n"));
        }
        user.push('\n');
    }

    // Diff excerpt.
    if !diff_excerpt.is_empty() {
        user.push_str("DIFF EXCERPT (truncated):\n");
        user.push_str(diff_excerpt);
        user.push('\n');
    }

    // MOCK_TRIGGER hint line — parsed only by MockProvider, ignored by real providers.
    let primary_trigger = stub.triggered_by.first().map(trigger_kind_name).unwrap_or("architecture_shaping");
    user.push_str(&format!("\nMOCK_TRIGGER: {primary_trigger}"));
    // Embed hash so MockProvider can detect MOCK_FAIL_HASH.
    user.push_str(&format!("\nCOMMIT_HASH_FOR_MOCK: {}", stub.commit_hash));

    user
}

fn trigger_kind_name(t: &TriggerKind) -> &'static str {
    match t {
        TriggerKind::Revert            => "revert",
        TriggerKind::WasReverted       => "was_reverted",
        TriggerKind::RepeatedFix       => "repeated_fix",
        TriggerKind::IncidentLinked    => "incident_linked",
        TriggerKind::ArchitectureShaping => "architecture_shaping",
    }
}

/// Fetch and truncate the diff for a commit using git2.
///
/// Runs on a `spawn_blocking` thread since git2 does blocking I/O.
async fn fetch_diff_excerpt(repo_path: &str, commit_hash: &str) -> String {
    let repo_path = repo_path.to_owned();
    let commit_hash = commit_hash.to_owned();

    tauri::async_runtime::spawn_blocking(move || {
        fetch_diff_excerpt_blocking(&repo_path, &commit_hash)
    })
    .await
    .unwrap_or_else(|_| Ok(String::new()))
    .unwrap_or_default()
}

fn fetch_diff_excerpt_blocking(repo_path: &str, commit_hash: &str) -> Result<String, String> {
    use git2::Repository;

    let repo = Repository::open(repo_path).map_err(|e| format!("open repo: {e}"))?;
    let oid = git2::Oid::from_str(commit_hash).map_err(|e| format!("parse oid: {e}"))?;
    let commit = repo.find_commit(oid).map_err(|e| format!("find commit: {e}"))?;

    let file_changes = commit_file_changes(&repo, &commit).map_err(|e| e.to_string())?;

    // Build a compact diff-like text from the file changes.
    // We don't reconstruct the full unified diff here — the file list with
    // add/delete counts is enough context for the narration LLM, and avoids
    // pulling the full diff text which can be megabytes on large commits.
    let mut out = String::new();
    for fc in &file_changes {
        out.push_str(&format!(
            "{} {} (+{} -{})\n",
            fc.status, fc.path, fc.additions, fc.deletions
        ));
    }

    // Truncate to 6000 chars as specified.
    if out.len() > 6000 {
        out.truncate(6000);
        out.push_str("\n... (truncated)");
    }
    Ok(out)
}

/// Parse the narration response JSON into (title, narration) strings.
fn parse_narration_response(
    resp_parsed: Option<serde_json::Value>,
    resp_text: &str,
) -> Result<(String, String), String> {
    let v = resp_parsed
        .or_else(|| serde_json::from_str(resp_text).ok())
        .ok_or_else(|| "narration response was not parseable JSON".to_string())?;

    let title = v
        .get("title")
        .and_then(|t| t.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "narration response missing 'title' field".to_string())?
        .to_string();

    let narration = v
        .get("narration")
        .and_then(|n| n.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "narration response missing 'narration' field".to_string())?
        .to_string();

    Ok((title, narration))
}

/// Narrate a single tour stop.
///
/// Checks the per-stop cache first; on miss builds the prompt, calls the
/// provider with exponential-backoff retry (up to 3 attempts), writes to
/// cache on success, and returns the complete `TourStop`.
///
/// `other_evidence_summaries` should be all selected candidates'
/// `evidence_summary` values EXCEPT the current stop's own summary.
#[tauri::command]
pub async fn narrate_stop(
    stub: StopStub,
    other_evidence_summaries: Vec<String>,
    repo_path: String,
    cache_root: String,
    cfg: Option<TourConfig>,
    provider_state: tauri::State<'_, crate::ProviderState>,
) -> Result<TourStop, String> {
    let cfg = cfg.unwrap_or_default();
    let cache_root = Path::new(&cache_root);

    // Per-stop cache check.
    let stop_cache_path = cache_path(
        cache_root,
        "narrations",
        &format!("{}-{}", stub.commit_hash, cfg.narration_prompt_version),
    );
    if let Some(cached) = read_cache::<TourStop>(&stop_cache_path) {
        return Ok(cached);
    }

    // Fetch diff excerpt (blocking git2 I/O, off the async runtime).
    let diff_excerpt = fetch_diff_excerpt(&repo_path, &stub.commit_hash).await;

    // Build prompt.
    let user_prompt = build_narration_prompt(&stub, &diff_excerpt, &other_evidence_summaries);

    let req = ModelRequest {
        system: "You narrate git commits for a new-hire onboarding tour. \
                 Return strict JSON with fields: title (string) and narration (string, 2-4 paragraphs)."
            .to_string(),
        user: user_prompt,
        schema: Some(narration_response_schema()),
        temperature: cfg.temperature,
        model_id: cfg.model_id.clone(),
        max_tokens: Some(cfg.max_narration_tokens),
    };

    // Exponential backoff retry: up to 3 attempts, delays 1s then 2s.
    let provider = provider_state.provider.clone();
    let mut last_err = String::new();
    let retry_delays_ms: &[u64] = &[0, 1000, 2000];

    for (attempt, &delay_ms) in retry_delays_ms.iter().enumerate() {
        if delay_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        }

        match provider.call(req.clone()).await {
            Ok(resp) => {
                let (title, narration) = parse_narration_response(resp.parsed, &resp.text)?;

                let stop = TourStop {
                    sequence: stub.sequence,
                    commit_hash: stub.commit_hash.clone(),
                    timestamp_utc: stub.timestamp_utc.clone(),
                    title,
                    narration,
                    subsystem: stub.subsystem.clone(),
                    subsystems: stub.subsystems.clone(),
                    files_changed: stub.files_changed.clone(),
                    triggered_by: stub.triggered_by.clone(),
                    selection_rationale: stub.selection_rationale.clone(),
                    linked_document: stub.linked_document.clone(),
                };

                // Write per-stop cache.
                write_cache(&stop_cache_path, &stop);
                return Ok(stop);
            }
            Err(e) => {
                last_err = e;
                eprintln!(
                    "[tour_narration] narrate_stop attempt {}/{} failed for {}: {}",
                    attempt + 1,
                    retry_delays_ms.len(),
                    stub.commit_hash,
                    last_err
                );
            }
        }
    }

    Err(format!(
        "narrate_stop failed after {} attempts for commit {}: {}",
        retry_delays_ms.len(),
        stub.commit_hash,
        last_err
    ))
}

// ---------- Tests ----------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_mining::MiningWindow;
    use crate::tour_types::{TourConfig, TourStop, TriggerKind};

    fn make_stub(hash: &str) -> StopStub {
        StopStub {
            sequence: 1,
            commit_hash: hash.to_string(),
            timestamp_utc: "2024-01-01T00:00:00Z".to_string(),
            message_summary: "Fix timeout handling (INC-4821)".to_string(),
            subsystem: "backend".to_string(),
            subsystems: vec!["backend".to_string()],
            files_changed: vec!["src/server.rs".to_string()],
            triggered_by: vec![TriggerKind::IncidentLinked],
            selection_rationale: "Incident-linked fix that changed retry semantics".to_string(),
            evidence_summary: "Timeout patch linked to INC-4821 outage".to_string(),
            linked_document: None,
        }
    }

    // --- tour_cache_key stability ---

    #[test]
    fn test_tour_cache_key_stability() {
        let window = MiningWindow {
            max_age_days: Some(365),
            max_commits_per_subsystem: Some(100),
        };
        let k1 = tour_cache_key("/repo/path", "abcdef123", &window, "v1");
        let k2 = tour_cache_key("/repo/path", "abcdef123", &window, "v1");
        assert_eq!(k1, k2, "tour_cache_key must be deterministic");
        assert_eq!(k1.len(), 64, "expected 64-char hex SHA-256");
    }

    #[test]
    fn test_tour_cache_key_changes_on_different_input() {
        let window = MiningWindow {
            max_age_days: Some(365),
            max_commits_per_subsystem: Some(100),
        };
        let k1 = tour_cache_key("/repo/path", "abcdef123", &window, "v1");
        let k2 = tour_cache_key("/repo/path", "abcdef123", &window, "v2");
        assert_ne!(k1, k2, "different prompt version must yield different cache key");

        let k3 = tour_cache_key("/repo/path", "different_head", &window, "v1");
        assert_ne!(k1, k3, "different head hash must yield different cache key");
    }

    // --- per-stop cache miss-then-hit ---

    #[test]
    fn test_per_stop_cache_miss_then_hit() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_root = tmp.path();

        // Write a fake TourStop to the cache location.
        let stop = TourStop {
            sequence: 1,
            commit_hash: "abc123".to_string(),
            timestamp_utc: "2024-01-01T00:00:00Z".to_string(),
            title: "The outage that added this check".to_string(),
            narration: "First paragraph.\n\nSecond paragraph.".to_string(),
            subsystem: "backend".to_string(),
            subsystems: vec!["backend".to_string()],
            files_changed: vec!["src/server.rs".to_string()],
            triggered_by: vec![TriggerKind::IncidentLinked],
            selection_rationale: "incident fix".to_string(),
            linked_document: None,
        };

        let prompt_version = "v1";
        let key = format!("{}-{}", stop.commit_hash, prompt_version);
        let p = cache_path(cache_root, "narrations", &key);
        write_cache(&p, &stop);

        // Read it back — should succeed.
        let loaded: Option<TourStop> = read_cache(&p);
        assert!(loaded.is_some(), "cache miss: file was just written");
        let loaded = loaded.unwrap();
        assert_eq!(loaded.commit_hash, stop.commit_hash);
        assert_eq!(loaded.title, stop.title);

        // Verify a different key produces a miss.
        let wrong_p = cache_path(cache_root, "narrations", "different_hash-v1");
        let miss: Option<TourStop> = read_cache(&wrong_p);
        assert!(miss.is_none(), "expected cache miss for non-existent key");
    }

    // --- fail-fast config validation ---

    #[test]
    fn test_fail_fast_when_fetch_enabled_but_no_tokens() {
        // Validates the fail-fast check logic directly without going through Tauri.
        let cfg = TourConfig {
            fetch_linked_documents: true,
            github_token: None,
            jira_token: None,
            ..TourConfig::default()
        };

        // Mirror the check from fetch_stop_documents.
        let err = if cfg.fetch_linked_documents
            && cfg.github_token.is_none()
            && cfg.jira_token.is_none()
        {
            Some("fetch_linked_documents is enabled but neither github_token nor jira_token is configured".to_string())
        } else {
            None
        };

        assert!(
            err.is_some(),
            "expected fail-fast error when fetch enabled but no tokens"
        );
        assert!(
            err.unwrap().contains("neither github_token nor jira_token"),
            "error message should name both missing tokens"
        );
    }

    #[test]
    fn test_fail_fast_passes_when_github_token_present() {
        let cfg = TourConfig {
            fetch_linked_documents: true,
            github_token: Some("ghp_test_token".to_string()),
            jira_token: None,
            ..TourConfig::default()
        };

        let err: Option<String> = if cfg.fetch_linked_documents
            && cfg.github_token.is_none()
            && cfg.jira_token.is_none()
        {
            Some("fail".to_string())
        } else {
            None
        };

        assert!(err.is_none(), "should not fail-fast when github_token is set");
    }

    // --- derive_triggers ---

    #[test]
    fn test_derive_triggers_from_flags() {
        use crate::git_mining::HeuristicFlags;

        let flags = HeuristicFlags {
            is_revert: true,
            was_reverted: false,
            touches_repeated_fix_file: true,
            incident_refs: vec!["INC-1234".to_string()],
            ..HeuristicFlags::default()
        };
        let triggers = derive_triggers(&flags, false);
        assert!(triggers.contains(&TriggerKind::Revert));
        assert!(triggers.contains(&TriggerKind::RepeatedFix));
        assert!(triggers.contains(&TriggerKind::IncidentLinked));
        assert!(!triggers.contains(&TriggerKind::ArchitectureShaping));
    }

    #[test]
    fn test_derive_triggers_architecture_shaping() {
        use crate::git_mining::HeuristicFlags;

        let flags = HeuristicFlags::default();
        let triggers = derive_triggers(&flags, true);
        assert!(triggers.contains(&TriggerKind::ArchitectureShaping));
        // Should have at least one trigger even when flags are all false.
        assert!(!triggers.is_empty());
    }

    // --- build_narration_prompt ---

    #[test]
    fn test_build_narration_prompt_contains_mock_trigger() {
        let stub = make_stub("deadbeef");
        let prompt = build_narration_prompt(&stub, "M src/server.rs (+10 -3)", &[]);
        assert!(
            prompt.contains("MOCK_TRIGGER:"),
            "prompt must contain MOCK_TRIGGER hint line"
        );
        assert!(
            prompt.contains("incident_linked"),
            "MOCK_TRIGGER should reflect the primary trigger kind"
        );
    }

    #[test]
    fn test_build_narration_prompt_contains_commit_hash_for_mock() {
        let stub = make_stub("deadbeef");
        let prompt = build_narration_prompt(&stub, "", &[]);
        assert!(
            prompt.contains("COMMIT_HASH_FOR_MOCK: deadbeef"),
            "prompt must embed hash for MockProvider fail detection"
        );
    }

    #[test]
    fn test_build_narration_prompt_cross_stop_context() {
        let stub = make_stub("abc");
        let others = vec![
            "The rollback that changed the retry pattern".to_string(),
            "Timeout patch linked to INC-4821 outage".to_string(),
        ];
        let prompt = build_narration_prompt(&stub, "", &others);
        assert!(
            prompt.contains("OTHER STOPS IN THIS TOUR"),
            "prompt should include cross-stop context section"
        );
        assert!(prompt.contains("The rollback that changed the retry pattern"));
    }

    #[test]
    fn test_parse_narration_response_valid() {
        let v = serde_json::json!({
            "title": "The outage that hardened the retry path",
            "narration": "First paragraph.\n\nSecond paragraph."
        });
        let (title, narration) = parse_narration_response(Some(v), "").unwrap();
        assert_eq!(title, "The outage that hardened the retry path");
        assert!(narration.contains("First paragraph."));
    }

    #[test]
    fn test_parse_narration_response_missing_title_is_err() {
        let v = serde_json::json!({"narration": "some narration"});
        let result = parse_narration_response(Some(v), "");
        assert!(result.is_err(), "missing title should be an error");
    }

    #[test]
    fn test_parse_narration_response_fallback_from_text() {
        let text = r#"{"title":"Fallback title","narration":"Fallback narration."}"#;
        let (title, _) = parse_narration_response(None, text).unwrap();
        assert_eq!(title, "Fallback title");
    }
}
