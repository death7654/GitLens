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
    pub fn from_candidate(c: &CandidateInput) -> Self {
        let h = &c.commit.heuristics;
        Self {
            architecture_shaping: false,
            revert: h.is_revert || h.was_reverted || !c.pr.revert_links.is_empty(),
            incident_linked: !h.incident_refs.is_empty() || !c.pr.linked_issues.is_empty(),
            discussion_rich: c.pr.pr_comment_count.map(|n| n >= 5).unwrap_or(false),
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
    pub shortfall_filled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankingOutput {
    pub tour_candidates: Vec<RankedCandidate>,
    pub meta: RankingMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankingConfig {
    pub prompt_version: String,
    pub model_id: String,
    pub temperature: f32,
    pub target_min: usize,          // 10 per brief
    pub target_max: usize,          // 15 per brief
    pub min_per_subsystem: usize,   // diversity floor
    pub use_file_summaries: bool,   // off until P6 wired
    pub use_commit_summaries: bool,
}

impl Default for RankingConfig {
    fn default() -> Self {
        Self {
            prompt_version: "v1".into(),
            model_id: "gemini-1.5-pro".into(),
            temperature: 0.1,
            target_min: 10,
            target_max: 15,
            min_per_subsystem: 2,
            use_file_summaries: false,
            use_commit_summaries: false,
        }
    }
}