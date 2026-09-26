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
    SubsystemDef,
};
#[allow(unused_imports)]
pub use discovery::discover_subsystems;
#[allow(unused_imports)]
pub(crate) use worker::commit_file_changes;

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

/// The Tauri IPC command Person 5's frontend calls to auto-discover a
/// repo's top-level directory layout as candidate subsystems, so the user
/// doesn't have to hand-type `path_prefixes` against paths they'd otherwise
/// have to go check for themselves. Meant to be called right after the
/// repo path field is filled in, before the user edits the subsystems JSON
/// by hand.
#[tauri::command]
pub async fn discover_repo_subsystems(repo_path: String) -> Result<Vec<SubsystemDef>, String> {
    // Same rationale as extract_git_history: git2 does blocking I/O, so run
    // it off the async runtime Tauri commands execute on.
    tauri::async_runtime::spawn_blocking(move || discovery::discover_subsystems(&repo_path))
        .await
        .map_err(|e| format!("discovery task panicked: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    // -----------------------------------------------------------------------
    // Helpers shared between fixture builders
    // -----------------------------------------------------------------------

    /// Returns a deterministic ISO-8601 timestamp offset by `minute` minutes
    /// from a fixed base, so every commit in a pinned-date fixture has a
    /// unique timestamp and therefore a unique git object hash.
    fn git_date(minute: u32) -> String {
        format!("2024-01-15T10:{:02}:00Z", minute)
    }

    // -----------------------------------------------------------------------
    // build_fixture_repo — original heuristic coverage fixture (unchanged)
    // -----------------------------------------------------------------------

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

    // -----------------------------------------------------------------------
    // build_ranking_fixture_repo — ranking / noise-filter fixture
    // -----------------------------------------------------------------------

    /// Builds a throwaway repo shaped so the ranking pipeline has an
    /// assertable "right answer":
    ///
    /// * 8 noise commits that must be dropped by the heuristic pre-filter or
    ///   `is_noise_commit`.
    /// * 3 signal commits that must survive as tour candidates:
    ///   - Fix A  (repeated-fix pair, first touch of `backend/server.rs`)
    ///   - Incident (SECURITY-1042 reference, `backend/auth.rs`)
    ///   - Revert  (reverts the experimental caching layer commit)
    /// * 1 "bad" commit that gets reverted (also a signal — `was_reverted`)
    /// * 1 fail-trigger commit: message embeds `MOCK_FAIL_HASH` so that
    ///   when the narration step builds its prompt, `MockProvider::call`
    ///   returns `Err` — enabling the partial-failure UI state on camera.
    ///   This commit has a fix-style subject so it is a candidate that can
    ///   reach the narration step.
    ///
    /// Returns `(temp_dir, expected_signal_hashes)` where
    /// `expected_signal_hashes` holds the hashes of Fix A, Incident, and
    /// Revert (commits 9, 11, 13 in the sequence).
    fn build_ranking_fixture_repo() -> (tempfile::TempDir, Vec<String>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();

        // Closure: run a git command with fully pinned author/committer
        // identity and timestamp.  `minute` is the per-commit offset so
        // every object hash is unique and stable across machines.
        let run_at = |args: &[&str], minute: u32| {
            let date = git_date(minute);
            let status = Command::new("git")
                .args(args)
                .current_dir(path)
                .env("GIT_AUTHOR_NAME", "Test")
                .env("GIT_AUTHOR_EMAIL", "test@example.com")
                .env("GIT_COMMITTER_NAME", "Test")
                .env("GIT_COMMITTER_EMAIL", "test@example.com")
                .env("GIT_AUTHOR_DATE", &date)
                .env("GIT_COMMITTER_DATE", &date)
                .status()
                .expect("git command failed to run");
            assert!(status.success(), "git {:?} failed", args);
        };

        let write = |rel: &str, content: &str| {
            let p = path.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        };

        let rev_parse = || -> String {
            let out = Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(path)
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };

        // -- Commit 0: init -------------------------------------------------
        run_at(&["init", "-q", "-b", "main"], 0);

        // -- Commit 1: initial setup (noise — no heuristic signals) ---------
        write("frontend/app.js", "console.log('v1');\n");
        write("backend/server.rs", "fn main() {}\n");
        write("migrations/001_init.sql", "CREATE TABLE t (id INT);\n");
        write("Cargo.lock", "# generated\n");
        run_at(&["add", "."], 1);
        run_at(&["commit", "-q", "-m", "Initial setup"], 1);

        // -- Commit 2: chore (noise) ----------------------------------------
        write("backend/server.rs", "fn main() { /* tidy */ }\n");
        run_at(&["add", "."], 2);
        run_at(&["commit", "-q", "-m", "chore: tidy imports"], 2);

        // -- Commit 3: style (noise) ----------------------------------------
        write("frontend/app.js", "console.log('v2');\n");
        run_at(&["add", "."], 3);
        run_at(&["commit", "-q", "-m", "style: fix formatting"], 3);

        // -- Commit 4: deps / lockfile-only (noise) -------------------------
        write("Cargo.lock", "# updated\n");
        run_at(&["add", "."], 4);
        run_at(&["commit", "-q", "-m", "deps: bump versions"], 4);

        // -- Commit 5: chore lint config (noise) ----------------------------
        write(".eslintrc.json", "{}\n");
        run_at(&["add", "."], 5);
        run_at(&["commit", "-q", "-m", "chore: update lint config"], 5);

        // -- Commit 6: format (noise) ---------------------------------------
        write("frontend/app.js", "console.log('v3');\n");
        run_at(&["add", "."], 6);
        run_at(&["commit", "-q", "-m", "format: run prettier"], 6);

        // -- Commit 7: chore CI (noise) -------------------------------------
        write(".github/workflows/ci.yml", "on: push\n");
        run_at(&["add", "."], 7);
        run_at(&["commit", "-q", "-m", "chore: CI pipeline update"], 7);

        // -- Commit 8: bump version (noise) ---------------------------------
        write("backend/server.rs", "fn main() { /* v0.2.0 */ }\n");
        run_at(&["add", "."], 8);
        run_at(&["commit", "-q", "-m", "bump version 0.2.0"], 8);

        // -- Commit 9: Fix A (SIGNAL — repeated-fix, first touch) -----------
        write("backend/server.rs", "fn main() { /* session timeout fix 1 */ }\n");
        run_at(&["add", "."], 9);
        run_at(&["commit", "-q", "-m", "Fix session timeout not resetting on activity"], 9);
        let hash_fix_a = rev_parse();

        // -- Commit 10: Fix B (repeated-fix, second touch — pairs with A) ---
        write("backend/server.rs", "fn main() { /* session timeout fix 2 */ }\n");
        run_at(&["add", "."], 10);
        run_at(&["commit", "-q", "-m", "Fix session timeout regression in middleware"], 10);

        // -- Commit 11: Incident (SIGNAL — SECURITY-1042 ref) ---------------
        write("backend/auth.rs", "// auth patch\n");
        run_at(&["add", "."], 11);
        run_at(&["commit", "-q", "-m", "Patch authentication bypass (SECURITY-1042)"], 11);
        let hash_incident = rev_parse();

        // -- Commit 12: bad commit (to be reverted) -------------------------
        write("backend/cache.rs", "// experimental caching layer\n");
        run_at(&["add", "."], 12);
        run_at(&["commit", "-q", "-m", "Add experimental caching layer"], 12);
        let hash_bad = rev_parse();

        // -- Commit 13: Revert of 12 (SIGNAL — is_revert) ------------------
        // `git revert` creates a new commit; we must also pin its dates.
        {
            let date = git_date(13);
            let status = Command::new("git")
                .args(["revert", "--no-edit", &hash_bad])
                .current_dir(path)
                .env("GIT_AUTHOR_NAME", "Test")
                .env("GIT_AUTHOR_EMAIL", "test@example.com")
                .env("GIT_COMMITTER_NAME", "Test")
                .env("GIT_COMMITTER_EMAIL", "test@example.com")
                .env("GIT_AUTHOR_DATE", &date)
                .env("GIT_COMMITTER_DATE", &date)
                .status()
                .expect("git revert failed to run");
            assert!(status.success(), "git revert failed");
        }
        let hash_revert = rev_parse();

        // -- Commit 14: fail-trigger (for MockProvider partial-failure demo)
        //
        // This commit's message embeds MOCK_FAIL_HASH so that when
        // `build_narration_prompt` serialises the message_summary into the
        // prompt, MockProvider::call detects MOCK_FAIL_HASH and returns Err.
        // The fix-style subject ensures it is a heuristic candidate that can
        // reach the narration step.
        #[allow(unused_imports)]
        use crate::mock_provider::MOCK_FAIL_HASH;
        write("backend/fail_trigger.rs", "// fail-trigger file\n");
        run_at(&["add", "."], 14);
        run_at(
            &["commit", "-q", "-m", &format!("Fix critical regression (ref: {})", MOCK_FAIL_HASH)],
            14,
        );

        let expected = vec![hash_fix_a, hash_incident, hash_revert];
        (dir, expected)
    }

    // -----------------------------------------------------------------------
    // Tests using build_ranking_fixture_repo
    // -----------------------------------------------------------------------

    #[test]
    fn ranking_post_process_selects_expected_commits() {
        use crate::significance_ranking::{post_process, ModelScoredCandidate};
        use crate::significance_ranking_types::{CandidateInput, PrEnrichment, RankingConfig};

        let (dir, expected_hashes) = build_ranking_fixture_repo();
        let config = MiningConfig {
            repo_path: dir.path().to_string_lossy().to_string(),
            subsystems: vec![
                SubsystemDef {
                    name: "backend".to_string(),
                    path_prefixes: vec!["backend/".to_string()],
                },
                SubsystemDef {
                    name: "frontend".to_string(),
                    path_prefixes: vec!["frontend/".to_string()],
                },
                SubsystemDef {
                    name: "db-schema".to_string(),
                    path_prefixes: vec!["migrations/".to_string()],
                },
            ],
            window: MiningWindow {
                max_age_days: None,
                max_commits_per_subsystem: Some(100),
            },
        };

        let output = run_extraction(&config).expect("extraction should succeed");

        // Build CandidateInput for all mined candidates.
        let candidates: Vec<CandidateInput> = output
            .commits
            .iter()
            .map(|c| CandidateInput {
                commit: c.clone(),
                pr: PrEnrichment::default(),
            })
            .collect();

        // Simulate ranking: give expected hashes score 0.9, all others 0.1.
        let scored: Vec<ModelScoredCandidate> = candidates
            .iter()
            .map(|c| {
                let score = if expected_hashes.contains(&c.commit.hash) {
                    0.9
                } else {
                    0.1
                };
                ModelScoredCandidate {
                    commit_hash: c.commit.hash.clone(),
                    score,
                    architecture_shaping: false,
                    rationale: "test".into(),
                    evidence_summary: "test evidence".into(),
                }
            })
            .collect();

        let (ranked, _shortfall) = post_process(scored, &candidates, &RankingConfig::default());

        for expected_hash in &expected_hashes {
            assert!(
                ranked.iter().any(|r| &r.commit_hash == expected_hash),
                "expected hash {} to be in post_process ranked output",
                expected_hash
            );
        }
    }

    #[test]
    fn noise_filter_drops_all_fixture_noise_commits() {
        use crate::significance_ranking::is_noise_commit;
        use crate::significance_ranking_types::{CandidateInput, PrEnrichment};

        let (dir, _) = build_ranking_fixture_repo();
        let config = MiningConfig {
            repo_path: dir.path().to_string_lossy().to_string(),
            subsystems: vec![
                SubsystemDef {
                    name: "root".to_string(),
                    path_prefixes: vec![
                        "Cargo.lock".to_string(),
                        ".eslintrc.json".to_string(),
                        ".github/".to_string(),
                    ],
                },
                SubsystemDef {
                    name: "backend".to_string(),
                    path_prefixes: vec!["backend/".to_string()],
                },
                SubsystemDef {
                    name: "frontend".to_string(),
                    path_prefixes: vec!["frontend/".to_string()],
                },
            ],
            window: MiningWindow {
                max_age_days: None,
                max_commits_per_subsystem: Some(200),
            },
        };

        let output = run_extraction(&config).expect("extraction should succeed");

        // Subjects that must not survive as tour-quality candidates.
        // Either the heuristic pre-filter drops them (never enter
        // MiningOutput.commits) or is_noise_commit catches them.
        let noise_subjects = [
            "chore: tidy imports",
            "style: fix formatting",
            "deps: bump versions",
            "chore: update lint config",
            "format: run prettier",
            "chore: CI pipeline update",
            "bump version 0.2.0",
        ];

        for subject in &noise_subjects {
            let found = output.commits.iter().find(|c| c.message_summary == *subject);
            if let Some(c) = found {
                // If a noise commit slipped through the heuristic pre-filter,
                // is_noise_commit must catch it before ranking.
                let candidate = CandidateInput {
                    commit: c.clone(),
                    pr: PrEnrichment::default(),
                };
                assert!(
                    is_noise_commit(&candidate),
                    "noise commit '{}' slipped through heuristic filter \
                     AND is_noise_commit returned false — it would reach ranking",
                    subject
                );
            }
            // If the commit is absent from output entirely, the heuristic
            // pre-filter already did its job — that's also correct.
        }
    }

    #[test]
    fn tour_cache_key_changes_on_head_change() {
        use crate::tour_narration::tour_cache_key;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();

        let run_at = |args: &[&str], minute: u32| {
            let date = git_date(minute);
            let status = Command::new("git")
                .args(args)
                .current_dir(path)
                .env("GIT_AUTHOR_NAME", "Test")
                .env("GIT_AUTHOR_EMAIL", "test@example.com")
                .env("GIT_COMMITTER_NAME", "Test")
                .env("GIT_COMMITTER_EMAIL", "test@example.com")
                .env("GIT_AUTHOR_DATE", &date)
                .env("GIT_COMMITTER_DATE", &date)
                .status()
                .expect("git command failed to run");
            assert!(status.success(), "git {:?} failed", args);
        };

        let write = |rel: &str, content: &str| {
            let p = path.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        };

        let rev_parse = || -> String {
            let out = Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(path)
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };

        run_at(&["init", "-q", "-b", "main"], 0);

        write("README.md", "v1\n");
        run_at(&["add", "."], 1);
        run_at(&["commit", "-q", "-m", "first commit"], 1);
        let hash1 = rev_parse();

        write("README.md", "v2\n");
        run_at(&["add", "."], 2);
        run_at(&["commit", "-q", "-m", "second commit"], 2);
        let hash2 = rev_parse();

        let window = MiningWindow {
            max_age_days: Some(365),
            max_commits_per_subsystem: Some(50),
        };

        let key1 = tour_cache_key(path.to_str().unwrap(), &hash1, &window, "v1");
        let key2 = tour_cache_key(path.to_str().unwrap(), &hash2, &window, "v1");

        assert_ne!(key1, key2, "cache key must change when HEAD hash changes");

        // Stability: same inputs → same key.
        let key1_again = tour_cache_key(path.to_str().unwrap(), &hash1, &window, "v1");
        assert_eq!(key1, key1_again, "cache key must be stable for identical inputs");

        // Prompt-version change also invalidates the key.
        let key1_v2 = tour_cache_key(path.to_str().unwrap(), &hash1, &window, "v2");
        assert_ne!(key1, key1_v2, "cache key must change when prompt version changes");
    }

    // -----------------------------------------------------------------------
    // Original heuristic end-to-end test (unchanged)
    // -----------------------------------------------------------------------

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