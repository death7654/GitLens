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
    /// Minimum PR comment count for a commit to be flagged `discussion_rich`.
    /// Promoted out of `SignalSummary::from_candidate` so it can be tuned
    /// without a rebuild.
    pub discussion_rich_comment_threshold: u32,
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
            discussion_rich_comment_threshold: 5,
            use_file_summaries: false,
            use_commit_summaries: false,
        }
    }
}