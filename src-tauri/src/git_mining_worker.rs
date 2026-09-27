use super::heuristics::{extract_incident_refs, extract_reverted_hash, is_fix_message, is_revert_message};
use super::types::{CommitRecord, FileChange, HeuristicFlags, MiningWindow, SubsystemDef};
use chrono::{TimeZone, Utc};
use git2::{Diff, DiffOptions, Repository};
use std::collections::{HashMap, HashSet};

/// Threshold at which a file is considered a "repeated-fix" file: it must be
/// touched by at least this many distinct fix-flavored commits in the window.
const REPEATED_FIX_THRESHOLD: usize = 2;

pub(crate) struct RawCommit {
    hash: String,
    parent_hashes: Vec<String>,
    author_name: String,
    author_email: String,
    timestamp_utc: String,
    subject: String,
    /// Kept for debuggability even though only its derived fields
    /// (incident refs, revert target) are read downstream.
    #[allow(dead_code)]
    full_message: String,
    files: Vec<FileChange>,
    is_revert: bool,
    reverts_hash: Option<String>,
    is_fix: bool,
    incident_refs: Vec<String>,
}

/// Computes the file-level changes for `commit` (diffed against its first
/// parent, or the empty tree for a root commit) without leaking a borrowed
/// `Diff<'_>` past this call — everything is collected into owned
/// `FileChange` values before returning.
pub(crate) fn commit_file_changes(repo: &Repository, commit: &git2::Commit) -> Result<Vec<FileChange>, git2::Error> {
    let tree = commit.tree()?;
    let mut opts = DiffOptions::new();
    opts.include_typechange(true);
    let diff = if commit.parent_count() == 0 {
        repo.diff_tree_to_tree(None, Some(&tree), Some(&mut opts))?
    } else {
        let parent = commit.parent(0)?;
        let parent_tree = parent.tree()?;
        repo.diff_tree_to_tree(Some(&parent_tree), Some(&tree), Some(&mut opts))?
    };
    Ok(collect_file_changes(&diff))
}

fn collect_file_changes(diff: &Diff) -> Vec<FileChange> {
    let mut files = Vec::new();
    let deltas: Vec<_> = diff.deltas().collect();
    for (idx, delta) in deltas.iter().enumerate() {
        let path = delta
            .new_file()
            .path()
            .or_else(|| delta.old_file().path())
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        if path.is_empty() {
            continue;
        }
        let status = match delta.status() {
            git2::Delta::Added => "A",
            git2::Delta::Deleted => "D",
            git2::Delta::Modified => "M",
            git2::Delta::Renamed => "R",
            git2::Delta::Copied => "C",
            git2::Delta::Typechange => "T",
            _ => "?",
        }
        .to_string();
        let (mut additions, mut deletions) = (0usize, 0usize);
        if let Ok(patch) = git2::Patch::from_diff(diff, idx) {
            if let Some(patch) = patch {
                if let Ok((_ctx, adds, dels)) = patch.line_stats() {
                    additions = adds;
                    deletions = dels;
                }
            }
        }
        files.push(FileChange {
            path,
            additions,
            deletions,
            status,
        });
    }
    files
}

fn path_matches_subsystem(path: &str, subsystem: &SubsystemDef) -> bool {
    subsystem
        .path_prefixes
        .iter()
        .any(|prefix| path.starts_with(prefix.as_str()))
}

/// Walks the full commit history once, applying the age/count cap, and
/// returns the raw commits that touch this subsystem's paths. This is
/// Person 1's "write concurrent extraction workers ... that each run
/// git log/show/blame against their subsystem slice" step — one call of
/// this function is one worker's unit of work, intended to run on its own
/// thread with its own `Repository` handle (git2 Repository is not Sync).
pub fn mine_subsystem(
    repo_path: &str,
    subsystem: &SubsystemDef,
    window: &MiningWindow,
) -> Result<Vec<RawCommit>, String> {
    let repo = Repository::open(repo_path).map_err(|e| format!("open repo: {e}"))?;
    let mut revwalk = repo.revwalk().map_err(|e| e.to_string())?;
    revwalk.push_head().map_err(|e| e.to_string())?;
    revwalk
        .set_sorting(git2::Sort::TIME | git2::Sort::TOPOLOGICAL)
        .map_err(|e| e.to_string())?;

    let cutoff_epoch = window.max_age_days.map(|days| {
        let now = Utc::now();
        (now - chrono::Duration::days(days)).timestamp()
    });

    let mut out = Vec::new();
    for oid_res in revwalk {
        if let Some(cap) = window.max_commits_per_subsystem {
            if out.len() >= cap {
                break;
            }
        }
        let oid = oid_res.map_err(|e| e.to_string())?;
        let commit = repo.find_commit(oid).map_err(|e| e.to_string())?;
        let commit_time = commit.time().seconds();
        if let Some(cutoff) = cutoff_epoch {
            if commit_time < cutoff {
                break; // TIME-sorted walk: everything after this is even older
            }
        }

        let files = commit_file_changes(&repo, &commit).map_err(|e| e.to_string())?;
        if !files.iter().any(|f| path_matches_subsystem(&f.path, subsystem)) {
            continue; // doesn't touch this subsystem's slice
        }

        let subject = commit.summary().unwrap_or("").to_string();
        let full_message = commit.message().unwrap_or("").to_string();
        let author = commit.author();
        let ts = Utc
            .timestamp_opt(commit_time, 0)
            .single()
            .unwrap_or_else(Utc::now)
            .to_rfc3339();

        out.push(RawCommit {
            hash: commit.id().to_string(),
            parent_hashes: (0..commit.parent_count())
                .filter_map(|i| commit.parent_id(i).ok())
                .map(|id| id.to_string())
                .collect(),
            author_name: author.name().unwrap_or("").to_string(),
            author_email: author.email().unwrap_or("").to_string(),
            timestamp_utc: ts,
            is_revert: is_revert_message(&subject),
            reverts_hash: extract_reverted_hash(&full_message),
            is_fix: is_fix_message(&subject),
            incident_refs: extract_incident_refs(&full_message),
            subject,
            full_message,
            files,
        });
    }
    Ok(out)
}

/// Pass 2/3 of the heuristic pre-filter, scoped to one worker's raw commit
/// list: builds the repeated-fix-file map and reverted-hash set, then emits
/// only commits that pass at least one heuristic (revert linkage,
/// repeated-fix-file membership, or an incident/bug reference).
pub fn filter_to_candidates(
    raw: Vec<RawCommit>,
    subsystem_name: &str,
) -> Vec<CommitRecord> {
    let mut fix_counts_per_file: HashMap<String, usize> = HashMap::new();
    for c in &raw {
        if c.is_fix {
            for f in &c.files {
                *fix_counts_per_file.entry(f.path.clone()).or_insert(0) += 1;
            }
        }
    }
    let reverted_hashes: HashSet<String> = raw
        .iter()
        .filter_map(|c| c.reverts_hash.clone())
        .collect();

    let mut out = Vec::new();
    for c in raw {
        // Only flag this commit for the repeated-fix-file heuristic if it is
        // ITSELF one of the fix commits hitting that file at/above the
        // threshold — not any unrelated commit that happens to touch the
        // same path (e.g. the file's initial add, or an unrelated feature
        // change). Otherwise every commit ever touching a hot file would be
        // swept in as a "candidate", which defeats the point of the filter.
        let repeated_files: Vec<String> = if c.is_fix {
            c.files
                .iter()
                .filter(|f| {
                    fix_counts_per_file
                        .get(&f.path)
                        .copied()
                        .unwrap_or(0)
                        >= REPEATED_FIX_THRESHOLD
                })
                .map(|f| f.path.clone())
                .collect()
        } else {
            Vec::new()
        };

        // Match full or abbreviated hash against any recorded "This reverts commit <sha>"
        let was_reverted = reverted_hashes
            .iter()
            .any(|rh| c.hash.starts_with(rh.as_str()) || rh.starts_with(c.hash.as_str()));

        let flags = HeuristicFlags {
            is_revert: c.is_revert,
            reverts_commit: c.reverts_hash.clone(),
            was_reverted,
            touches_repeated_fix_file: !repeated_files.is_empty(),
            repeated_fix_file_paths: repeated_files,
            incident_refs: c.incident_refs.clone(),
        };

        if !flags.is_candidate() {
            continue;
        }

        out.push(CommitRecord {
            hash: c.hash,
            parent_hashes: c.parent_hashes,
            author_name: c.author_name,
            author_email: c.author_email,
            timestamp_utc: c.timestamp_utc,
            message_summary: c.subject,
            subsystems: vec![subsystem_name.to_string()],
            files_changed: c.files,
            heuristics: flags,
        });
    }
    out
}