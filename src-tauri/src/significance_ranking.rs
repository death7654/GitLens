//! Person 3 — Significance Ranking Agent.
//!
//!   0. Load & scope          — take P1 commits ∪ P2 enrichment
//!   1. File summaries        — [stub] cached, cheap tier, candidate-touched files only
//!   2. Commit summaries      — [stub] vague-trigger expands with diff/blame
//!   3. Project summary       — [stub] hierarchical reduce, built once
//!   4. Ranking ★             — map per-candidate → reduce to top N
//!   5. Post-process          — noise, diversity, hash-validate, refill (no LLM)
//!   6. Emit                  — RankingOutput

use std::collections::{HashMap};
use std::sync::OnceLock;
use regex::Regex;

use crate::significance_ranking_stages;
use crate::provider::{ModelProvider, ModelRequest};
use crate::significance_ranking_types::*;

// ---------- Stage 5 helpers (deterministic, no LLM) ----------

fn noise_path_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?ix)(^|/)(package-lock\.json|yarn\.lock|pnpm-lock\.yaml|Cargo\.lock|poetry\.lock|Gemfile\.lock|go\.sum|composer\.lock|\.prettierrc.*|\.eslintrc.*|\.editorconfig|rustfmt\.toml)$",
        )
        .unwrap()
    })
}

fn noise_msg_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?ix)^(chore|style|format|formatting|lint|deps|dependenc(y|ies)|bump\s+version|version\s+bump|merge\s+branch|update\s+.*\.lock|release\s+v?\d)(:|\(|\s|$)",
        )
        .unwrap()
    })
}

/// True if a candidate should be dropped before ranking — either its message
/// reads like routine churn, or every file it touched is a lockfile/config.
pub fn is_noise_commit(c: &CandidateInput) -> bool {
    if noise_msg_re().is_match(c.commit.message_summary.trim()) {
        return true;
    }
    if c.commit.files_changed.is_empty() {
        return true;
    }
    c.commit.files_changed.iter().all(|f| noise_path_re().is_match(&f.path))
}

/// The deterministic chain from the plan: filter → quota → validate → (refill).
pub fn post_process(
    ranked_raw: Vec<ModelScoredCandidate>,
    candidates: &[CandidateInput],
    cfg: &RankingConfig,
) -> (Vec<RankedCandidate>, bool) {
    let by_hash: HashMap<&str, &CandidateInput> =
        candidates.iter().map(|c| (c.commit.hash.as_str(), c)).collect();

    // 1. noise filter
    let mut kept: Vec<ModelScoredCandidate> = ranked_raw
        .into_iter()
        .filter(|r| by_hash.get(r.commit_hash.as_str()).map(|c| !is_noise_commit(c)).unwrap_or(false))
        .collect();

    // 2. hash validation (drop hallucinations)
    kept.retain(|r| by_hash.contains_key(r.commit_hash.as_str()));

    // 3. diversity quota — round-robin by primary subsystem
    let mut buckets: HashMap<String, Vec<ModelScoredCandidate>> = HashMap::new();
    for r in kept {
        let primary = by_hash[r.commit_hash.as_str()]
            .commit.subsystems.first().cloned().unwrap_or_else(|| "unknown".into());
        buckets.entry(primary).or_default().push(r);
    }
    for v in buckets.values_mut() { v.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap()); }

    let mut ordered: Vec<ModelScoredCandidate> = Vec::new();
    let mut quota_met: HashMap<String, usize> =
        buckets.keys().cloned().map(|k| (k, 0)).collect();

    // pass 1: fill diversity floor
    for (sub, list) in &buckets {
        for r in list.iter().take(cfg.min_per_subsystem) {
            ordered.push(r.clone());
            *quota_met.get_mut(sub).unwrap() += 1;
        }
    }
    // pass 2: fill remaining slots by global score, capped at target_max
    let mut rest: Vec<&ModelScoredCandidate> = buckets.values().flatten().collect();
    rest.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
    for r in rest {
        if ordered.len() >= cfg.target_max { break; }
        if ordered.iter().any(|o| o.commit_hash == r.commit_hash) { continue; }
        ordered.push(r.clone());
    }
    ordered.truncate(cfg.target_max);

    let shortfall = ordered.len() < cfg.target_min;

    // 4. materialize RankedCandidate
    let ranked = ordered.into_iter().enumerate().map(|(i, r)| {
        let c = by_hash[r.commit_hash.as_str()];
        let mut sig = SignalSummary::from_candidate(c);
        sig.architecture_shaping = r.architecture_shaping;
        RankedCandidate {
            rank: i + 1,
            commit_hash: r.commit_hash.clone(),
            subsystem: c.commit.subsystems.first().cloned().unwrap_or_default(),
            subsystems: c.commit.subsystems.clone(),
            timestamp_utc: c.commit.timestamp_utc.clone(),
            score: r.score,
            signals: sig,
            rationale: r.rationale,
            evidence_summary: r.evidence_summary,
            source_pr: c.pr.pr_number,
        }
    }).collect();

    (ranked, shortfall)
}

// ---------- Stage 4 — ranking prompt + response shape ----------

#[derive(Debug, Clone)]
pub struct ModelScoredCandidate {
    pub commit_hash: String,
    pub score: f32,
    pub architecture_shaping: bool,
    pub rationale: String,
    pub evidence_summary: String,
}

/// Builds the ranking prompt. The rubric names the four signal families
/// explicitly so the model optimises for onboarding-tour value, not diff size.
pub fn build_ranking_prompt(
    candidates: &[CandidateInput],
    project_summary: &str,
    target_max: usize,
) -> String {
    let mut out = String::new();
    if !project_summary.is_empty() {
        out.push_str("PROJECT CONTEXT\n");
        out.push_str(project_summary);
        out.push_str("\n\n");
    }
    out.push_str(
        "You are ranking candidate git commits for a new-hire onboarding tour.\n\
         Score each on these axes — NOT on diff size:\n\
         - Architecture-shaping (introduced/replaced a pattern)\n\
         - Revert / revert chain (tried and undone — high narrative value)\n\
         - Incident-linked fix (references a bug, outage, or incident)\n\
         - Discussion richness (gnarly PR review, many comments)\n\
         Downweight: dependency bumps, formatting passes, lockfile-only commits.\n\n\
         Return JSON: {\"candidates\":[{\"commit_hash\",\"score\"(0..1),\
         \"architecture_shaping\"(bool),\"rationale\",\"evidence_summary\"}]}\n\
         Return at most ",
    );
    out.push_str(&target_max.to_string());
    out.push_str(" entries, highest first.\n\nCANDIDATES:\n");

    for c in candidates {
        let h = &c.commit.heuristics;
        out.push_str(&format!(
            "- hash: {}\n  subject: {}\n  subsystems: {:?}\n  files: {}\n  \
             signals: revert={} was_reverted={} repeated_fix={} incidents={:?} \
             pr_comments={:?} pr_links={:?}\n",
            c.commit.hash,
            c.commit.message_summary,
            c.commit.subsystems,
            c.commit.files_changed.iter().map(|f| f.path.as_str()).collect::<Vec<_>>().join(", "),
            h.is_revert, h.was_reverted, h.touches_repeated_fix_file,
            h.incident_refs, c.pr.pr_comment_count, c.pr.linked_issues,
        ));
    }
    out
}

fn ranking_response_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["candidates"],
        "properties": {
            "candidates": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["commit_hash","score","architecture_shaping","rationale","evidence_summary"],
                    "properties": {
                        "commit_hash":         {"type":"string"},
                        "score":               {"type":"number"},
                        "architecture_shaping":{"type":"boolean"},
                        "rationale":           {"type":"string"},
                        "evidence_summary":    {"type":"string"}
                    }
                }
            }
        }
    })
}

fn parse_ranking_response(v: &serde_json::Value) -> Result<Vec<ModelScoredCandidate>, String> {
    let arr = v.get("candidates").and_then(|c| c.as_array())
        .ok_or_else(|| "response missing 'candidates' array".to_string())?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let h = item.get("commit_hash").and_then(|x| x.as_str()).ok_or("missing commit_hash")?;
        let s = item.get("score").and_then(|x| x.as_f64()).unwrap_or(0.0) as f32;
        let a = item.get("architecture_shaping").and_then(|x| x.as_bool()).unwrap_or(false);
        let r = item.get("rationale").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let e = item.get("evidence_summary").and_then(|x| x.as_str()).unwrap_or("").to_string();
        out.push(ModelScoredCandidate {
            commit_hash: h.to_string(), score: s, architecture_shaping: a,
            rationale: r, evidence_summary: e,
        });
    }
    Ok(out)
}

// ---------- Orchestrator ----------

pub async fn run_ranking(
    candidates: Vec<CandidateInput>,
    cfg: RankingConfig,
    repo_path: &std::path::Path,   // NEW: pinned clone path (from P6)
    cache_root: &std::path::Path,  // NEW: stage-cache directory (shared with P5/P6)
    provider: &dyn ModelProvider,
) -> Result<RankingOutput, String> {
    if candidates.is_empty() {
        return Err("no candidates to rank".into());
    }

    // ---- Stage 1–3 — enrichment (skipped unless the cfg flags are on) ----
    let mut project_summary = String::new();
    let mut file_summaries = std::collections::HashMap::new();
    // Reserved for a future revision that threads per-commit prose into the
    // stage-4 prompt; today we still populate the cache so it's warm.
    let mut _commit_summaries = std::collections::HashMap::new();

    if cfg.use_file_summaries || cfg.use_commit_summaries {
        // Stage 1 — per-file summaries (disk-cached; candidate-touched files only).
        if cfg.use_file_summaries {
            file_summaries = significance_ranking_stages::summarize_candidate_files(
                repo_path, &candidates, cache_root, provider, &cfg.model_id,
            ).await;
        }

        // Stage 2 — per-commit summaries (disk-cached; uses Stage 1 output).
        if cfg.use_commit_summaries {
            for c in &candidates {
                if let Ok(s) = significance_ranking_stages::summarize_commit(
                    c, &file_summaries, cache_root, provider, &cfg.model_id,
                ).await {
                    _commit_summaries.insert(c.commit.hash.clone(), s);
                }
            }
        }

        // Stage 3 — hierarchical project summary (disk-cached; one per run).
        let subsystem_names: Vec<String> = {
            let mut v: Vec<String> = candidates.iter()
                .flat_map(|c| c.commit.subsystems.iter().cloned())
                .collect::<std::collections::HashSet<_>>()
                .into_iter().collect();
            v.sort();
            v
        };
        if let Ok(s) = significance_ranking_stages::build_project_summary(
            repo_path, &subsystem_names, &file_summaries, cache_root,
            &cfg.prompt_version, provider, &cfg.model_id,
        ).await {
            project_summary = s;
        }
    }

    // ---- Stage 4 — the ranking call (the actual deliverable) ----
    let prompt = build_ranking_prompt(&candidates, &project_summary, cfg.target_max);
    let resp = provider.call(ModelRequest {
        system: "You rank git commits for onboarding tours. Return strict JSON.".into(),
        user: prompt,
        schema: Some(ranking_response_schema()),
        temperature: cfg.temperature,
        model_id: cfg.model_id.clone(),
        max_tokens: Some(4096),
    }).await?;

    let parsed = resp.parsed
        .or_else(|| serde_json::from_str::<serde_json::Value>(&resp.text).ok())
        .ok_or_else(|| "model returned unparseable output".to_string())?;
    let scored = parse_ranking_response(&parsed)?;

    // ---- Stage 5 — deterministic post-process (no LLM) ----
    // noise filter → diversity quota → hash validation → shortfall flag
    let (ranked, shortfall) = post_process(scored, &candidates, &cfg);

    // ---- Stage 6 — emit ----
    Ok(RankingOutput {
        tour_candidates: ranked,
        meta: RankingMeta {
            prompt_version: cfg.prompt_version,
            model_id: resp.model_id,
            generated_at_utc: chrono::Utc::now().to_rfc3339(),
            candidate_count: candidates.len(),
            shortfall_filled: shortfall,
        },
    })
}

// ---------- Tauri IPC command ----------

/// Person 5's frontend calls this after P1's extraction (and P2's enrichment,
/// when available) to get the ranked tour candidates for P4.
///
/// NOTE: `provider` is injected. Until P6's provider lands, the caller passes
/// a stub implementation. See `lib.rs` for the wiring point.
#[tauri::command]
pub async fn rank_significant_commits(
    candidates: Vec<CandidateInput>,
    repo_path: String,
    cache_root: String,
    cfg: Option<RankingConfig>,
    provider_state: tauri::State<'_, crate::ProviderState>,
) -> Result<RankingOutput, String> {
    let cfg = cfg.unwrap_or_default();
    let provider = provider_state.provider.clone();
    run_ranking(
        candidates,
        cfg,
        std::path::Path::new(&repo_path),
        std::path::Path::new(&cache_root),
        provider.as_ref(),
    ).await
}

// ---------- Tests ----------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_mining::{CommitRecord, FileChange, HeuristicFlags};

    fn mk(hash: &str, subject: &str, subsystems: &[&str], files: &[&str]) -> CandidateInput {
        CandidateInput {
            commit: CommitRecord {
                hash: hash.into(),
                parent_hashes: vec![],
                author_name: "a".into(), author_email: "a@b".into(),
                timestamp_utc: "2024-01-01T00:00:00Z".into(),
                message_summary: subject.into(),
                subsystems: subsystems.iter().map(|s| s.to_string()).collect(),
                files_changed: files.iter().map(|p| FileChange {
                    path: p.to_string(), additions: 1, deletions: 0, status: "M".into(),
                }).collect(),
                heuristics: HeuristicFlags::default(),
            },
            pr: PrEnrichment::default(),
        }
    }

    #[test]
    fn noise_filter_drops_lockfile_only_commits() {
        let c = mk("h1", "update deps", &["backend"], &["Cargo.lock"]);
        assert!(is_noise_commit(&c));
    }

    #[test]
    fn noise_filter_drops_chore_prefix() {
        let c = mk("h1", "chore: tidy up", &["backend"], &["src/lib.rs"]);
        assert!(is_noise_commit(&c));
    }

    #[test]
    fn noise_filter_keeps_real_fix() {
        let c = mk("h1", "Fix race condition in scheduler", &["backend"], &["src/lib.rs"]);
        assert!(!is_noise_commit(&c));
    }

    #[test]
    fn hash_validation_drops_hallucinations() {
        let real = vec![mk("real1", "Fix bug", &["backend"], &["src/a.rs"])];
        let scored = vec![
            ModelScoredCandidate { commit_hash: "real1".into(), score: 0.9,
                architecture_shaping: false, rationale: "".into(), evidence_summary: "".into() },
            ModelScoredCandidate { commit_hash: "ghost".into(), score: 0.99,
                architecture_shaping: false, rationale: "".into(), evidence_summary: "".into() },
        ];
        let (out, _) = post_process(scored, &real, &RankingConfig::default());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].commit_hash, "real1");
    }

    #[test]
    fn diversity_floor_prefers_spread() {
        // 6 backend commits (score 0.9) + 2 frontend (score 0.1)
        // with min_per_subsystem=2, both frontend entries should survive.
        let mut cands = vec![];
        let mut scored = vec![];
        for i in 0..6 {
            let h = format!("b{i}");
            cands.push(mk(&h, "feat backend", &["backend"], &["src/b.rs"]));
            scored.push(ModelScoredCandidate { commit_hash: h, score: 0.9,
                architecture_shaping: false, rationale: "".into(), evidence_summary: "".into() });
        }
        for i in 0..2 {
            let h = format!("f{i}");
            cands.push(mk(&h, "feat frontend", &["frontend"], &["src/f.js"]));
            scored.push(ModelScoredCandidate { commit_hash: h, score: 0.1,
                architecture_shaping: false, rationale: "".into(), evidence_summary: "".into() });
        }
        let (out, _) = post_process(scored, &cands, &RankingConfig::default());
        let frontend_count = out.iter().filter(|c| c.subsystem == "frontend").count();
        assert!(frontend_count >= 2, "expected diversity floor to keep both frontend entries");
    }
}