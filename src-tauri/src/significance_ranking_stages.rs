//! Person 3 — stages 1–3 of the ranking pipeline.
//!
//! Every stage is disk-cached. Cache keys:
//!   Stage 1 (file summaries):   SHA-256 of file *content*
//!                               → summaries survive renames and re-commits
//!   Stage 2 (commit summaries): commit hash
//!   Stage 3 (project summary):  repo_path + prompt_version + subsystem set
//!
//! All three take an injected `ModelProvider` (P6) and are no-ops when the
//! caller hasn't enabled them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use sha2::{Digest, Sha256};

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::git_mining::SubsystemDef;
use crate::provider::{ModelProvider, ModelRequest};
use crate::significance_ranking_types::CandidateInput;

// ---------- cache plumbing ----------

fn hash_bytes(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

fn cache_path(root: &Path, kind: &str, key: &str) -> PathBuf {
    root.join(kind).join(format!("{key}.json"))
}

fn read_cache<T: for<'de> Deserialize<'de>>(p: &Path) -> Option<T> {
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

// ---------- Stage 1 — file summaries ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSummary {
    pub provides: String,
    pub externals: Vec<String>,
    pub purpose: String,
    pub functionalities: Vec<String>,
    pub notes: String,
}

fn file_summary_schema() -> serde_json::Value {
    serde_json::json!({
        "type":"object",
        "required":["provides","externals","purpose","functionalities","notes"],
        "properties":{
            "provides":         {"type":"string"},
            "externals":        {"type":"array","items":{"type":"string"}},
            "purpose":          {"type":"string"},
            "functionalities":  {"type":"array","items":{"type":"string"}},
            "notes":            {"type":"string"}
        }
    })
}

pub async fn summarize_file(
    repo_path: &Path,
    rel_path: &str,
    cache_root: &Path,
    provider: &dyn ModelProvider,
    model_id: &str,
) -> Result<FileSummary, String> {
    let abs = repo_path.join(rel_path);
    let bytes = std::fs::read(&abs).map_err(|e| format!("read {}: {e}", abs.display()))?;
    let key = hash_bytes(&bytes);
    let cache = cache_path(cache_root, "file_summaries", &key);
    if let Some(s) = read_cache::<FileSummary>(&cache) {
        return Ok(s);
    }

    // Cap the excerpt so one huge file doesn't blow the context budget.
    let content: String = String::from_utf8_lossy(&bytes).chars().take(8000).collect();

    let resp = provider.call(ModelRequest {
        system: "You summarise source files for a codebase tour. Return strict JSON.".into(),
        user: format!(
            "File: {rel_path}\n\n--- content ---\n{content}\n--- end ---\n\n\
             Summarise: what it provides, what external things it uses, its purpose, \
             the concrete functionalities it implements, and any special notes."
        ),
        schema: Some(file_summary_schema()),
        temperature: 0.0,
        model_id: model_id.to_string(),
        max_tokens: Some(1024),
    }).await?;

    let parsed = resp.parsed
        .or_else(|| serde_json::from_str(&resp.text).ok())
        .ok_or_else(|| "unparseable file summary".to_string())?;
    let summary: FileSummary = serde_json::from_value(parsed)
        .map_err(|e| format!("bad file summary shape: {e}"))?;
    write_cache(&cache, &summary);
    Ok(summary)
}

/// Summarises the union of files touched by the candidate set. This is the
/// only file set that gets summarised — nothing else in the repo is touched.
pub async fn summarize_candidate_files(
    repo_path: &Path,
    candidates: &[CandidateInput],
    cache_root: &Path,
    provider: &dyn ModelProvider,
    model_id: &str,
) -> HashMap<String, FileSummary> {
    let mut unique: Vec<String> = candidates.iter()
        .flat_map(|c| c.commit.files_changed.iter().map(|f| f.path.clone()))
        .collect();
    unique.sort();
    unique.dedup();

    let mut out = HashMap::new();
    for path in unique {
        match summarize_file(repo_path, &path, cache_root, provider, model_id).await {
            Ok(s) => { out.insert(path, s); }
            Err(e) => eprintln!("[p3] file summary skipped {path}: {e}"),
        }
    }
    out
}

// ---------- Stage 2 — commit summaries ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitSummary {
    pub what_changed: String,
    pub why: String,
    pub takeaway: String,
    pub expanded: bool,     // true if the vague-trigger fired and we used diff/blame evidence
}

/// Vague-trigger from the plan. Returns `true` when a commit message is too
/// terse to stand on its own, signalling that the summarise-commit prompt
/// should ask the model to infer intent from diff/file evidence.
///
/// A message is considered *not* vague only when it satisfies all three of:
///   1. At least 40 characters long (not a one-liner stub).
///   2. Starts with a conventional commit prefix (`feat`, `fix`, …) so the
///      message declares its intent explicitly.
///   3. Contains an issue/incident reference (`#123`, `INC-4821`, …) that
///      links it to external context a reader can follow.
///
/// Free-form descriptive sentences (e.g. "Rewrite the auth session store to
/// use Redis") are long enough but lack a structured prefix and reference, so
/// they *do* fire the trigger — the model gets the extra evidence hint, which
/// is appropriate because the message, while descriptive, gives no structured
/// traceability signal.
pub fn vague_trigger(message: &str) -> bool {
    let m = message.trim();

    if m.chars().count() < 40 {
        return true;
    }
    let prefix: &'static Regex = {
        static R: OnceLock<Regex> = OnceLock::new();
        R.get_or_init(|| Regex::new(r"(?i)^(feat|fix|refactor|chore|docs|test|perf|build|ci|style)\b").unwrap())
    };
    if !prefix.is_match(m) {
        return true;
    }
    let reference: &'static Regex = {
        static R: OnceLock<Regex> = OnceLock::new();
        R.get_or_init(|| Regex::new(r"#\d+|[A-Z]{2,10}-\d+").unwrap())
    };
    if !reference.is_match(m) {
        return true;
    }
    false
}

fn commit_summary_schema() -> serde_json::Value {
    serde_json::json!({
        "type":"object",
        "required":["what_changed","why","takeaway"],
        "properties":{
            "what_changed": {"type":"string"},
            "why":          {"type":"string"},
            "takeaway":     {"type":"string"}
        }
    })
}

pub async fn summarize_commit(
    c: &CandidateInput,
    file_summaries: &HashMap<String, FileSummary>,
    cache_root: &Path,
    provider: &dyn ModelProvider,
    model_id: &str,
) -> Result<CommitSummary, String> {
    let cache = cache_path(cache_root, "commit_summaries", &c.commit.hash);
    if let Some(s) = read_cache::<CommitSummary>(&cache) {
        return Ok(s);
    }

    let expanded = vague_trigger(&c.commit.message_summary);

    // Baseline: message + file list + (any) file summaries.
    let mut user = format!(
        "Commit: {}\nSubject: {}\nSubsystems: {:?}\n",
        c.commit.hash, c.commit.message_summary, c.commit.subsystems,
    );
    for f in &c.commit.files_changed {
        user.push_str(&format!("- {} (+{} -{})\n", f.path, f.additions, f.deletions));
        if let Some(fs) = file_summaries.get(&f.path) {
            user.push_str(&format!("    purpose: {}\n", fs.purpose));
        }
    }
    if expanded {
        user.push_str("\nThe commit message is terse. Infer intent from the evidence above.\n");
    }

    let resp = provider.call(ModelRequest {
        system: "You summarise git commits for a new-hire onboarding tour. Return strict JSON.".into(),
        user,
        schema: Some(commit_summary_schema()),
        temperature: 0.1,
        model_id: model_id.to_string(),
        max_tokens: Some(768),
    }).await?;

    let parsed = resp.parsed
        .or_else(|| serde_json::from_str(&resp.text).ok())
        .ok_or_else(|| "unparseable commit summary".to_string())?;
    let v: serde_json::Value = parsed;
    let summary = CommitSummary {
        what_changed: v.get("what_changed").and_then(|x| x.as_str()).unwrap_or("").into(),
        why:          v.get("why").and_then(|x| x.as_str()).unwrap_or("").into(),
        takeaway:     v.get("takeaway").and_then(|x| x.as_str()).unwrap_or("").into(),
        expanded,
    };
    write_cache(&cache, &summary);
    Ok(summary)
}

// ---------- Stage 3 — project summary ----------

/// Hierarchical reduce: per-subsystem → repo. One artifact per
/// (repo, prompt_version, subsystem-set) tuple.
///
/// `subsystems` is the authoritative partition list from P1's `MiningConfig`
/// — it carries the *path prefixes* that files are matched against, not just
/// the subsystem *names*. Passing prefixes (rather than names) is what makes
/// the bucketing step actually work: a file at `src/backend/foo.rs` matches
/// the subsystem whose prefixes include `"src/backend/"`, not one merely
/// named `"backend"`.
pub async fn build_project_summary(
    repo_path: &Path,
    subsystems: &[SubsystemDef],
    file_summaries: &HashMap<String, FileSummary>,
    cache_root: &Path,
    prompt_version: &str,
    provider: &dyn ModelProvider,
    model_id: &str,
) -> Result<String, String> {
    // Cache key includes a canonical fingerprint of the subsystem set, since
    // changing which prefixes are configured changes the bucketing output.
    let subsystems_fingerprint = {
        let mut pairs: Vec<(String, Vec<String>)> = subsystems
            .iter()
            .map(|s| {
                let mut p = s.path_prefixes.clone();
                p.sort();
                (s.name.clone(), p)
            })
            .collect();
        pairs.sort();
        let s = serde_json::to_string(&pairs).unwrap_or_default();
        hash_bytes(s.as_bytes())
    };
    let key = format!(
        "{}-{}-{}",
        hash_bytes(repo_path.to_string_lossy().as_bytes()),
        hash_bytes(prompt_version.as_bytes()),
        subsystems_fingerprint,
    );
    let cache = cache_path(cache_root, "project_summaries", &key);
    if let Some(s) = read_cache::<String>(&cache) {
        return Ok(s);
    }

    // Bucket each file's summary under the first configured subsystem whose
    // prefix list matches the file's path. Files matching no subsystem fall
    // into "misc" so they aren't silently dropped.
    let mut by_subsystem: HashMap<String, Vec<&FileSummary>> = HashMap::new();
    for (path, summary) in file_summaries {
        let bucket = subsystems
            .iter()
            .find(|sub| sub.path_prefixes.iter().any(|p| path.starts_with(p.as_str())))
            .map(|sub| sub.name.clone())
            .unwrap_or_else(|| "misc".to_string());
        by_subsystem.entry(bucket).or_default().push(summary);
    }

    // Reduce pass 1: per-subsystem summary.
    let mut subsystem_summaries: Vec<(String, String)> = Vec::new();
    for (top, summaries) in &by_subsystem {
        let mut user = format!("Subsystem: {top}\nFiles:\n");
        for s in summaries.iter().take(40) {
            user.push_str(&format!("- purpose: {}\n  provides: {}\n", s.purpose, s.provides));
        }
        user.push_str("\nIn 2-3 sentences, describe what this subsystem does and its role in the project.");

        let resp = provider.call(ModelRequest {
            system: "You summarise code subsystems. Return plain prose, not JSON.".into(),
            user,
            schema: None,
            temperature: 0.0,
            model_id: model_id.to_string(),
            max_tokens: Some(400),
        }).await?;
        subsystem_summaries.push((top.clone(), resp.text.trim().to_string()));
    }

    // Reduce pass 2: repo summary.
    let mut user = format!("Project subsystems ({}):\n", subsystems.len());
    for (name, summary) in &subsystem_summaries {
        user.push_str(&format!("\n## {name}\n{summary}\n"));
    }
    user.push_str("\nWrite a single short paragraph describing the project as a whole, \
                   its architectural shape, and what a new hire should know first.");

    let resp = provider.call(ModelRequest {
        system: "You write crisp architecture overviews for onboarding. Plain prose.".into(),
        user,
        schema: None,
        temperature: 0.2,
        model_id: model_id.to_string(),
        max_tokens: Some(600),
    }).await?;

    let summary = resp.text.trim().to_string();
    write_cache(&cache, &summary);
    Ok(summary)
}