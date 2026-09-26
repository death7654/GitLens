use serde::{Deserialize, Serialize};

/// One of Person 1's subsystem partitions. Paths are matched by prefix against
/// each commit's changed-file list to decide which worker(s) claim a commit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubsystemDef {
    pub name: String,
    /// Path prefixes (relative to repo root) that belong to this subsystem,
    /// e.g. "src/frontend/", "crates/backend/", "migrations/".
    pub path_prefixes: Vec<String>,
}

/// Caps the mining window: whichever bound is hit first stops the walk for
/// that worker. Matches "1-2 years / N commits" from the spec.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiningWindow {
    pub max_age_days: Option<i64>,
    pub max_commits_per_subsystem: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiningConfig {
    pub repo_path: String,
    pub subsystems: Vec<SubsystemDef>,
    pub window: MiningWindow,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HeuristicFlags {
    pub is_revert: bool,
    /// Hash of the commit this one claims to revert, if parseable from the message.
    pub reverts_commit: Option<String>,
    /// True if this commit was itself later reverted by another commit in the window.
    pub was_reverted: bool,
    /// True if any file this commit touches was also touched by >= 2 other
    /// "fix"-flavored commits within the mining window (repeated-fix file).
    pub touches_repeated_fix_file: bool,
    pub repeated_fix_file_paths: Vec<String>,
    /// Bug/incident references pulled from the commit message, e.g. "INC-1234", "#987".
    pub incident_refs: Vec<String>,
}

impl HeuristicFlags {
    /// Union two flag sets for the same commit hash seen by different subsystem workers.
    pub fn merge(mut self, other: HeuristicFlags) -> Self {
        self.is_revert = self.is_revert || other.is_revert;
        self.reverts_commit = self.reverts_commit.or(other.reverts_commit);
        self.was_reverted = self.was_reverted || other.was_reverted;
        self.touches_repeated_fix_file =
            self.touches_repeated_fix_file || other.touches_repeated_fix_file;
        for p in other.repeated_fix_file_paths {
            if !self.repeated_fix_file_paths.contains(&p) {
                self.repeated_fix_file_paths.push(p);
            }
        }
        for r in other.incident_refs {
            if !self.incident_refs.contains(&r) {
                self.incident_refs.push(r);
            }
        }
        self
    }

    pub fn is_candidate(&self) -> bool {
        self.is_revert
            || self.was_reverted
            || self.touches_repeated_fix_file
            || !self.incident_refs.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub additions: usize,
    pub deletions: usize,
    /// "A" added, "M" modified, "D" deleted, "R" renamed, etc.
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitRecord {
    pub hash: String,
    pub parent_hashes: Vec<String>,
    pub author_name: String,
    pub author_email: String,
    pub timestamp_utc: String,
    pub message_summary: String,
    /// Every subsystem worker that claimed this commit (a commit can touch
    /// more than one subsystem's paths).
    pub subsystems: Vec<String>,
    pub files_changed: Vec<FileChange>,
    pub heuristics: HeuristicFlags,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiningOutput {
    pub repo_path: String,
    pub generated_at_utc: String,
    pub window: MiningWindow,
    pub subsystems_scanned: Vec<String>,
    pub total_commits_scanned: usize,
    pub candidate_count: usize,
    /// Deduped, merged candidate commits — the schema Person 3 consumes.
    /// NOTE: field names here are a proposed contract; confirm against
    /// Person 3's actual schema before wiring up downstream.
    pub commits: Vec<CommitRecord>,
}
