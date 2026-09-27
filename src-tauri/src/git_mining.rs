//! Person 1 — Git History Extraction.
//!
//! Partitions the target repo into subsystems (frontend / backend /
//! db-schema, or whatever the caller configures) and mines each one
//! concurrently for candidate commits, using a heuristic pre-filter (revert
//! linkage, repeated-fix files, incident/bug-number references). Workers'
//! results are merged and deduped by commit hash before being handed back
//! across the Tauri IPC boundary as JSON.
//!
//! This logic was developed and validated as a standalone crate (unit tests
//! + an end-to-end test against a throwaway fixture repo covering every
//! heuristic) before being wired in here; see the `tests` module at the
//! bottom of this file for the same coverage running in-tree.

#[path = "git_mining_types.rs"]
mod types;
#[path = "git_mining_heuristics.rs"]
mod heuristics;
#[path = "git_mining_worker.rs"]
mod worker;
#[path = "git_mining_discovery.rs"]
mod discovery;

// These re-exports are the module's public surface (config/result types a
// caller builds or reads); several aren't referenced by name inside this
// crate itself, which rustc otherwise flags as unused.
#[allow(unused_imports)]
pub use types::{
    CommitRecord, FileChange, HeuristicFlags, MiningConfig, MiningOutput, MiningWindow,
    RepoStatus, SubsystemDef,
};
#[allow(unused_imports)]
pub use discovery::discover_subsystems;
#[allow(unused_imports)]
pub(crate) use worker::commit_file_changes;

use std::collections::{HashMap, HashSet};

/// Runs the full extraction pipeline: one thread per subsystem (each with
/// its own `git2::Repository` handle onto the same on-disk clone — no
/// `git worktree` needed, since every step here is a `git log`/`git show`-
/// equivalent read against the object database, not a checkout), heuristic
/// pre-filtering within each worker, then a merge step that dedupes by
/// commit hash and unions subsystem tags / heuristic flags for any
/// cross-cutting commit that more than one worker claimed.
type SubsystemResult = Result<(String, Vec<CommitRecord>, usize), String>;

pub fn run_extraction(config: &MiningConfig) -> Result<MiningOutput, String> {
    let results: Vec<SubsystemResult> = std::thread::scope(|scope| {
        let handles: Vec<_> = config
            .subsystems
            .iter()
            .map(|subsystem| {
                let repo_path = config.repo_path.clone();
                let window = config.window.clone();
                let subsystem = subsystem.clone();
                scope.spawn(move || {
                    let raw = worker::mine_subsystem(&repo_path, &subsystem, &window)?;
                    let scanned = raw.len();
                    let candidates = worker::filter_to_candidates(raw, &subsystem.name);
                    Ok((subsystem.name, candidates, scanned))
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_else(|_| Err("worker thread panicked".to_string())))
            .collect()
    });

    let mut merged: HashMap<String, CommitRecord> = HashMap::new();
    let mut subsystems_scanned = Vec::new();
    let mut total_scanned = 0usize;

    for res in results {
        let (subsystem_name, candidates, scanned) = res?;
        subsystems_scanned.push(subsystem_name);
        total_scanned += scanned;
        for record in candidates {
            merged
                .entry(record.hash.clone())
                .and_modify(|existing| {
                    // Dedup subsystems and files by materialising the seen
                    // sets into owned `HashSet<String>`s first.  If we held
                    // borrowing `HashSet<&str>` views into `existing`, the
                    // immutable borrow would outlive the `.push()` calls
                    // below and the borrow checker would reject the
                    // mutation.  Owning the sets ends the borrow at
                    // `.collect()`.
                    let existing_subs: HashSet<String> =
                        existing.subsystems.iter().cloned().collect();
                    for s in &record.subsystems {
                        if !existing_subs.contains(s) {
                            existing.subsystems.push(s.clone());
                        }
                    }

                    existing.heuristics = std::mem::take(&mut existing.heuristics)
                        .merge(record.heuristics.clone());

                    let existing_paths: HashSet<String> =
                        existing.files_changed.iter().map(|f| f.path.clone()).collect();
                    for f in &record.files_changed {
                        if !existing_paths.contains(&f.path) {
                            existing.files_changed.push(f.clone());
                        }
                    }
                })
                .or_insert(record);
        }
    }

    let mut commits: Vec<CommitRecord> = merged.into_values().collect();
    commits.sort_by(|a, b| b.timestamp_utc.cmp(&a.timestamp_utc));

    Ok(MiningOutput {
        repo_path: config.repo_path.clone(),
        generated_at_utc: chrono::Utc::now().to_rfc3339(),
        window: config.window.clone(),
        subsystems_scanned,
        total_commits_scanned: total_scanned,
        candidate_count: commits.len(),
        commits,
    })
}

/// The Tauri IPC command Person 5's frontend interface calls into.
#[tauri::command]
pub async fn extract_git_history(config: MiningConfig) -> Result<MiningOutput, String> {
    tauri::async_runtime::spawn_blocking(move || run_extraction(&config))
        .await
        .map_err(|e| format!("extraction task panicked: {e}"))?
}

/// Auto-discovers a repo's top-level directory layout as candidate subsystems.
#[tauri::command]
pub async fn discover_repo_subsystems(repo_path: String) -> Result<Vec<SubsystemDef>, String> {
    tauri::async_runtime::spawn_blocking(move || discovery::discover_subsystems(&repo_path))
        .await
        .map_err(|e| format!("discovery task panicked: {e}"))?
}

/// Reads HEAD's branch name, working-tree cleanliness, and latest commit.
fn read_repo_status(repo_path: &str) -> Result<RepoStatus, String> {
    let repo = git2::Repository::open(repo_path).map_err(|e| format!("open repo: {e}"))?;

    let head = repo.head().map_err(|e| format!("resolve HEAD: {e}"))?;
    let branch = head.shorthand().unwrap_or("HEAD").to_string();
    let commit = head
        .peel_to_commit()
        .map_err(|e| format!("peel HEAD to commit: {e}"))?;
    let full_hash = commit.id().to_string();
    let latest_commit_hash = full_hash.chars().take(7).collect();
    let latest_commit_summary = commit.summary().unwrap_or("").to_string();

    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true).include_ignored(false);
    let statuses = repo
        .statuses(Some(&mut opts))
        .map_err(|e| format!("git status: {e}"))?;
    let is_clean = statuses.is_empty();

    Ok(RepoStatus {
        branch,
        is_clean,
        latest_commit_hash,
        latest_commit_summary,
    })
}

#[tauri::command]
pub async fn get_repo_status(repo_path: String) -> Result<RepoStatus, String> {
    tauri::async_runtime::spawn_blocking(move || read_repo_status(&repo_path))
        .await
        .map_err(|e| format!("status task panicked: {e}"))?
}

#[cfg(test)]
mod tests {
    // ... unchanged from your current file ...
}