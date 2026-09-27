//! Person 3 — Significance Ranking Agent: I/O schema.
//!
//! Input : P1's CommitRecord joined with optional P2 PR/issue enrichment.
//! Output: ranked 10–15 tour candidates for Person 4.

use serde::{Deserialize, Serialize};
use crate::git_mining::CommitRecord;

/// Optional PR/issue enrichment from Person 2.
/// Every field optional → degrades gracefully on repos with no rich PR history.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PrEnrichment {
    pub pr_number: Option<u64>,
    pub pr_comment_count: Option<u32>,
    pub pr_thread_depth: Option<u32>,
    #[serde(default)] pub linked_issues: Vec<String>,
    #[serde(default)] pub revert_links: Vec<String>,
    #[serde(default)] pub rich_pr_history: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateInput {
    pub commit: CommitRecord,
    #[serde(default)] pub pr: PrEnrichment,
}

/// The four signal families the brief calls out, plus repeated-fix.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SignalSummary {
    pub architecture_shaping: bool, // model decides in ranking
    pub revert: bool,
    pub incident_linked: bool,
    pub discussion_rich: bool,
    pub repeated_fix: bool,
}

impl SignalSummary {
    /// `discussion_rich_threshold` is the minimum `pr_comment_count` for a
    /// commit to be considered discussion-rich; promoted from a hardcoded
    /// magic number so it can be tuned against the demo repo without a
    /// rebuild (see `RankingConfig::discussion_rich_comment_threshold`).
    pub fn from_candidate(c: &CandidateInput, discussion_rich_threshold: u32) -> Self {
        let h = &c.commit.heuristics;
        Self {
            architecture_shaping: false,
            revert: h.is_revert || h.was_reverted || !c.pr.revert_links.is_empty(),
            incident_linked: !h.incident_refs.is_empty() || !c.pr.linked_issues.is_empty(),
            discussion_rich: c.pr
                .pr_comment_count
                .map(|n| n >= discussion_rich_threshold)
                .unwrap_or(false),
            repeated_fix: h.touches_repeated_fix_file,
        }
    }
}

/// One ranked tour candidate — unit consumed by Person 4.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankedCandidate {
    pub rank: usize,
    pub commit_hash: String,
    pub subsystem: String,          // primary (first P1 assigned)
    pub subsystems: Vec<String>,    // all, for cluster view + diversity
    pub timestamp_utc: String,
    pub score: f32,
    pub signals: SignalSummary,
    pub rationale: String,
    pub evidence_summary: String,   // paraphrase-safe
    pub source_pr: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankingMeta {
    pub prompt_version: String,
    pub model_id: String,
    pub generated_at_utc: String,
    pub candidate_count: usize,
    /// True when the selected set is below `RankingConfig::target_min`, even
    /// after shortfall-refill from the noise pool. Callers should treat this
    /// as a soft signal (e.g. warn in the UI) rather than a hard error.
    ///
    /// Renamed from `shortfall_filled`, which read backwards: the old field
    /// was `true` when the result was *short* of target_min, i.e. the
    /// opposite of "filled".
    pub below_target_min: bool,
    /// Free-form reasoning text from the split-reasoning path.
    ///
    /// `Some` when `RankingConfig::split_reasoning_from_extraction` was `true`
    /// and the reasoning call produced text (either from a dedicated reasoning
    /// field or from `text`).  `None` when the single-call path was used or
    /// when no reasoning text was produced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankingOutput {
    pub tour_candidates: Vec<RankedCandidate>,
    pub meta: RankingMeta,
}

/// `#[serde(default)]` (container-level): when the frontend sends a `cfg`
/// object that only overrides a subset of fields — e.g. `{ "model_id": "..." }`
/// from `buildRankingConfig()` in main.js — serde fills in every field the
/// payload omits from `RankingConfig::default()` below, field by field,
/// rather than requiring the caller to specify all of them or none at all.
/// Without this, `Option<RankingConfig>`'s `Some` case deserializes strictly:
/// omitting *any* field (as happened first with the newly-added
/// `reasoning_enabled`, then with `prompt_version` once the frontend was
/// trimmed down to send almost nothing) fails with "missing field", even
/// though `cfg: None` already relied on `.unwrap_or_default()` for the
/// "send nothing at all" case. This closes that gap permanently: any field
/// added to this struct in the future is automatically covered too.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RankingConfig {
    pub prompt_version: String,
    pub model_id: String,
    pub temperature: f32,
    pub target_min: usize,          // 10 per brief
    pub target_max: usize,          // 15 per brief
    pub min_per_subsystem: usize,   // diversity floor
    /// Minimum PR comment count for a commit to be flagged `discussion_rich`.
    /// Promoted out of `SignalSummary::from_candidate` so it can be tuned
    /// without a rebuild.
    pub discussion_rich_comment_threshold: u32,
    pub use_file_summaries: bool,   // off until P6 wired
    pub use_commit_summaries: bool,
    /// When `true`, emit reasoning parameters on the ranking call so the
    /// model uses its chain-of-thought before producing the final answer.
    /// Controlled by `GITLENS_REASONING_ENABLED` env var.
    pub reasoning_enabled: bool,
    /// Maximum tokens to allow for the final answer / extraction pass.
    /// Controlled by `GITLENS_MAX_OUTPUT_TOKENS` env var.
    pub max_output_tokens: u32,
    /// Soft cap on reasoning / thinking tokens.
    /// Controlled by `GITLENS_MAX_REASONING_TOKENS` env var.
    pub max_reasoning_tokens: u32,
    /// Temperature applied during the reasoning pass (not the extraction pass).
    /// Controlled by `GITLENS_REASONING_TEMPERATURE` env var.
    pub reasoning_temperature: f32,
    /// When `true`, `run_ranking` performs the ranking call as two LLM
    /// round-trips: (a) free-form reasoning, (b) strict-JSON extraction
    /// conditioned on the reasoning text.  When `false` (default), a single
    /// structured call is made.
    ///
    /// Prefer `true` for local reasoning models (Qwen3, DeepSeek-R1, QwQ)
    /// where you want the reasoning text surfaced and the JSON extraction kept
    /// deterministic at temperature 0.  Prefer `false` for cloud models that
    /// have internal reasoning (OpenAI o-series, Gemini thinking mode).
    ///
    /// Controlled by `GITLENS_SPLIT_REASONING_FROM_EXTRACTION` env var.
    pub split_reasoning_from_extraction: bool,
}

/// Parse a boolean from an environment variable, logging a warning on
/// unrecognised values.
///
/// Recognises `1` and `true` (case-insensitive) as `true`, `0` and `false`
/// as `false`.  Empty or unset yields `None` (caller picks the default).
/// Any other non-empty value is logged and treated as unset, so a typo like
/// `GITLENS_REASONING_ENABLED=yes` doesn't silently and confusingly fall back
/// to the default without any diagnostic.
fn parse_bool_env(name: &str) -> Option<bool> {
    let raw = std::env::var(name).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed == "1" || trimmed.eq_ignore_ascii_case("true") {
        Some(true)
    } else if trimmed == "0" || trimmed.eq_ignore_ascii_case("false") {
        Some(false)
    } else {
        eprintln!(
            "[gitlens] {name}={trimmed:?} is not a recognised boolean \
             (use '1'/'true' or '0'/'false'). Falling back to default."
        );
        None
    }
}

impl Default for RankingConfig {
    fn default() -> Self {
        Self {
            prompt_version: "v1".into(),
            // GITLENS_MODEL_ID overrides the default so OpenAI-provider users
            // can point at e.g. "gpt-4o" without touching the frontend.
            // Falls back to "gemini-flash-latest" for the Gemini provider.
            model_id: std::env::var("GITLENS_MODEL_ID")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| "gemini-flash-latest".into()),
            // GITLENS_RANKING_TEMPERATURE overrides the default (0.1).
            temperature: std::env::var("GITLENS_RANKING_TEMPERATURE")
                .ok()
                .and_then(|v| v.trim().parse::<f32>().ok())
                .unwrap_or(0.1),
            target_min: 10,
            target_max: 15,
            min_per_subsystem: 2,
            discussion_rich_comment_threshold: 5,
            use_file_summaries: false,
            use_commit_summaries: false,
            reasoning_enabled: parse_bool_env("GITLENS_REASONING_ENABLED")
                .unwrap_or(false),
            max_output_tokens: std::env::var("GITLENS_MAX_OUTPUT_TOKENS")
                .ok()
                .and_then(|v| v.trim().parse::<u32>().ok())
                .unwrap_or(4096),
            max_reasoning_tokens: std::env::var("GITLENS_MAX_REASONING_TOKENS")
                .ok()
                .and_then(|v| v.trim().parse::<u32>().ok())
                .unwrap_or(2048),
            reasoning_temperature: std::env::var("GITLENS_REASONING_TEMPERATURE")
                .ok()
                .and_then(|v| v.trim().parse::<f32>().ok())
                .unwrap_or(0.6),
            split_reasoning_from_extraction: parse_bool_env(
                "GITLENS_SPLIT_REASONING_FROM_EXTRACTION",
            )
            .unwrap_or(false),
        }
    }
}

// ---------- Tests ----------

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    // -- Test 13: env vars are read and hard defaults are restored -----------

    #[test]
    fn ranking_config_reads_reasoning_env_vars() {
        // Save and set all five vars.
        let saved: Vec<(&str, Option<String>)> = vec![
            "GITLENS_REASONING_ENABLED",
            "GITLENS_MAX_OUTPUT_TOKENS",
            "GITLENS_MAX_REASONING_TOKENS",
            "GITLENS_REASONING_TEMPERATURE",
            "GITLENS_SPLIT_REASONING_FROM_EXTRACTION",
        ]
        .into_iter()
        .map(|k| (k, env::var(k).ok()))
        .collect();

        unsafe {
            env::set_var("GITLENS_REASONING_ENABLED", "true");
            env::set_var("GITLENS_MAX_OUTPUT_TOKENS", "8192");
            env::set_var("GITLENS_MAX_REASONING_TOKENS", "4096");
            env::set_var("GITLENS_REASONING_TEMPERATURE", "0.9");
            env::set_var("GITLENS_SPLIT_REASONING_FROM_EXTRACTION", "1");
        }

        let cfg = RankingConfig::default();
        assert!(cfg.reasoning_enabled, "reasoning_enabled should be true");
        assert_eq!(cfg.max_output_tokens, 8192);
        assert_eq!(cfg.max_reasoning_tokens, 4096);
        assert!((cfg.reasoning_temperature - 0.9).abs() < 1e-5, "temperature should be 0.9");
        assert!(cfg.split_reasoning_from_extraction);

        // Restore (or remove) saved values.
        for (k, v) in &saved {
            match v {
                Some(val) => unsafe { env::set_var(k, val) },
                None => unsafe { env::remove_var(k) },
            }
        }

        // Now hard defaults should be restored.
        let cfg2 = RankingConfig::default();
        assert!(!cfg2.reasoning_enabled, "default reasoning_enabled should be false");
        assert_eq!(cfg2.max_output_tokens, 4096, "default max_output_tokens = 4096");
        assert_eq!(cfg2.max_reasoning_tokens, 2048, "default max_reasoning_tokens = 2048");
        assert!(
            (cfg2.reasoning_temperature - 0.6).abs() < 1e-5,
            "default reasoning_temperature = 0.6"
        );
        assert!(!cfg2.split_reasoning_from_extraction, "default split = false");
    }

    // -- Garbage env values fall back to the default (with a warning) --------

    #[test]
    fn unrecognised_boolean_env_falls_back_to_default() {
        // Save existing values.
        let saved_enabled = env::var("GITLENS_REASONING_ENABLED").ok();
        let saved_split = env::var("GITLENS_SPLIT_REASONING_FROM_EXTRACTION").ok();

        unsafe {
            env::set_var("GITLENS_REASONING_ENABLED", "yes");
            env::set_var("GITLENS_SPLIT_REASONING_FROM_EXTRACTION", "on");
        }

        let cfg = RankingConfig::default();
        assert!(
            !cfg.reasoning_enabled,
            "'yes' is not recognised; must fall back to the default (false)"
        );
        assert!(
            !cfg.split_reasoning_from_extraction,
            "'on' is not recognised; must fall back to the default (false)"
        );

        // Explicit false-y values are honoured.
        unsafe {
            env::set_var("GITLENS_REASONING_ENABLED", "false");
            env::set_var("GITLENS_SPLIT_REASONING_FROM_EXTRACTION", "0");
        }
        let cfg2 = RankingConfig::default();
        assert!(!cfg2.reasoning_enabled);
        assert!(!cfg2.split_reasoning_from_extraction);

        // Restore or remove.
        match saved_enabled {
            Some(v) => unsafe { env::set_var("GITLENS_REASONING_ENABLED", v) },
            None => unsafe { env::remove_var("GITLENS_REASONING_ENABLED") },
        }
        match saved_split {
            Some(v) => unsafe { env::set_var("GITLENS_SPLIT_REASONING_FROM_EXTRACTION", v) },
            None => unsafe { env::remove_var("GITLENS_SPLIT_REASONING_FROM_EXTRACTION") },
        }
    }

    // -- serde(default) actually closes the "missing field" gap --------------

    #[test]
    fn deserializes_from_partial_json_via_serde_default() {
        // Mirrors what buildRankingConfig() in main.js now sends: only
        // model_id, nothing else. Before #[serde(default)] was added to the
        // struct, this failed with "missing field `prompt_version`" (and
        // before that, "missing field `reasoning_enabled`" the moment this
        // struct grew that field) even though `cfg: null` worked fine.
        let json = serde_json::json!({ "model_id": "gpt-4o-mini" });
        let cfg: RankingConfig = serde_json::from_value(json).expect(
            "a partial object should deserialize by filling missing fields from Default",
        );
        assert_eq!(cfg.model_id, "gpt-4o-mini");
        assert_eq!(cfg.prompt_version, RankingConfig::default().prompt_version);
        assert_eq!(cfg.target_min, RankingConfig::default().target_min);
    }

    #[test]
    fn deserializes_from_empty_json_object() {
        let cfg: RankingConfig = serde_json::from_value(serde_json::json!({})).expect(
            "an empty object should deserialize to the full default",
        );
        assert_eq!(cfg.target_min, RankingConfig::default().target_min);
        assert_eq!(cfg.target_max, RankingConfig::default().target_max);
    }
}