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

// These re-exports are the module's public surface (config/result types a
// caller builds or reads); several aren't referenced by name inside this
// crate itself, which rustc otherwise flags as unused.
#[allow(unused_imports)]
pub use types::{
    CommitRecord, FileChange, HeuristicFlags, MiningConfig, MiningOutput, MiningWindow,
    SubsystemDef,
};

use std::collections::HashMap;

/// Runs the full extraction pipeline: one thread per subsystem (each with
/// its own `git2::Repository` handle onto the same on-disk clone — no
/// `git worktree` needed, since every step here is a `git log`/`git show`-
/// equivalent read against the object database, not a checkout), heuristic
/// pre-filtering within each worker, then a merge step that dedupes by
/// commit hash and unions subsystem tags / heuristic flags for any
/// cross-cutting commit that more than one worker claimed.
pub fn run_extraction(config: &MiningConfig) -> Result<MiningOutput, String> {
    let results: Vec<Result<(String, Vec<CommitRecord>, usize), String>> =
        std::thread::scope(|scope| {
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
                    for s in &record.subsystems {
                        if !existing.subsystems.contains(s) {
                            existing.subsystems.push(s.clone());
                        }
                    }
                    existing.heuristics = std::mem::take(&mut existing.heuristics)
                        .merge(record.heuristics.clone());
                    for f in &record.files_changed {
                        if !existing.files_changed.iter().any(|ef| ef.path == f.path) {
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
///
/// NOTE: the shape of `MiningConfig` / `MiningOutput` (see
/// `git_mining_types.rs`) is a proposed contract for Person 3's downstream
/// consumer — confirm field names against Person 3's actual schema and adjust
/// `types.rs` (just the two struct defs; nothing else needs to change) if it
/// drifts. Likewise, this is one plausible IPC shape — swap the signature to
/// match whatever Person 5 settles on if it differs.
#[tauri::command]
pub async fn extract_git_history(config: MiningConfig) -> Result<MiningOutput, String> {
    // git2 is blocking I/O; run it on a blocking thread so it doesn't stall
    // the async runtime Tauri commands execute on.
    tauri::async_runtime::spawn_blocking(move || run_extraction(&config))
        .await
        .map_err(|e| format!("extraction task panicked: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// Builds a throwaway repo with commits deliberately shaped to exercise
    /// every heuristic: a plain feature commit, two fix commits touching the
    /// same file (repeated-fix), a commit referencing an incident ID, and a
    /// revert pair — split across frontend/backend/db-schema paths.
    fn build_fixture_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let run = |args: &[&str]| {
            let status = Command::new("git")
                .args(args)
                .current_dir(path)
                .env("GIT_AUTHOR_NAME", "Test")
                .env("GIT_AUTHOR_EMAIL", "test@example.com")
                .env("GIT_COMMITTER_NAME", "Test")
                .env("GIT_COMMITTER_EMAIL", "test@example.com")
                .status()
                .expect("git command failed to run");
            assert!(status.success(), "git {:?} failed", args);
        };
        let write = |rel: &str, content: &str| {
            let p = path.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        };

        run(&["init", "-q", "-b", "main"]);

        write("frontend/app.js", "console.log('v1');\n");
        write("backend/server.rs", "fn main() {}\n");
        write("migrations/001_init.sql", "CREATE TABLE t (id INT);\n");
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "Initial project scaffold"]);

        write("frontend/app.js", "console.log('v2'); // add nav\n");
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "Add nav bar to frontend"]);

        write("frontend/app.js", "console.log('v3'); // fix\n");
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "Fix nav bar rendering glitch"]);

        write("frontend/app.js", "console.log('v4'); // fix again\n");
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "Fix nav bar rendering glitch again"]);

        write("backend/server.rs", "fn main() { /* patched */ }\n");
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "Patch backend timeout handling (INC-4821)"]);

        write("migrations/001_init.sql", "CREATE TABLE t (id INT, bad_col INT);\n");
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "Add bad_col to schema"]);

        let bad_hash_output = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(path)
            .output()
            .unwrap();
        let bad_hash = String::from_utf8_lossy(&bad_hash_output.stdout).trim().to_string();

        // NOTE: unlike most git porcelain commands, `git revert` on the git
        // version this was validated against (2.43.0) has no -q/--quiet flag.
        run(&["revert", "--no-edit", &bad_hash]);

        dir
    }

    #[test]
    fn end_to_end_mining_flags_expected_heuristics() {
        let dir = build_fixture_repo();
        let config = MiningConfig {
            repo_path: dir.path().to_string_lossy().to_string(),
            subsystems: vec![
                SubsystemDef {
                    name: "frontend".to_string(),
                    path_prefixes: vec!["frontend/".to_string()],
                },
                SubsystemDef {
                    name: "backend".to_string(),
                    path_prefixes: vec!["backend/".to_string()],
                },
                SubsystemDef {
                    name: "db-schema".to_string(),
                    path_prefixes: vec!["migrations/".to_string()],
                },
            ],
            window: MiningWindow {
                max_age_days: Some(730),
                max_commits_per_subsystem: Some(100),
            },
        };

        let output = run_extraction(&config).expect("extraction should succeed");

        assert_eq!(output.subsystems_scanned.len(), 3);

        let repeated_fix_hit = output.commits.iter().any(|c| {
            c.heuristics.touches_repeated_fix_file
                && c
                    .heuristics
                    .repeated_fix_file_paths
                    .contains(&"frontend/app.js".to_string())
        });
        assert!(repeated_fix_hit, "expected a repeated-fix-file candidate for frontend/app.js");

        let incident_hit = output
            .commits
            .iter()
            .any(|c| c.heuristics.incident_refs.contains(&"INC-4821".to_string()));
        assert!(incident_hit, "expected an INC-4821 candidate");

        let revert_commit = output.commits.iter().find(|c| c.heuristics.is_revert);
        assert!(revert_commit.is_some(), "expected a commit flagged is_revert");
        assert!(revert_commit.unwrap().heuristics.reverts_commit.is_some());

        let reverted_hit = output.commits.iter().any(|c| c.heuristics.was_reverted);
        assert!(reverted_hit, "expected the original bad_col commit flagged was_reverted");

        let mut hashes: Vec<&str> = output.commits.iter().map(|c| c.hash.as_str()).collect();
        hashes.sort();
        let mut deduped = hashes.clone();
        deduped.dedup();
        assert_eq!(hashes.len(), deduped.len(), "merged output must not contain duplicate hashes");

        let has_plain = output
            .commits
            .iter()
            .any(|c| c.message_summary == "Add nav bar to frontend");
        assert!(!has_plain, "plain commits with no heuristic hit should be filtered out");
    }
}