//! Person 3 — Significance Ranking Agent.
//!
//! Pipeline:
//!   0. Load & scope          — take P1 commits ∪ P2 enrichment
//!   1. File summaries        — cached, cheap tier, candidate-touched files only
//!   2. Commit summaries      — vague-trigger expands with diff/blame
//!   3. Project summary       — hierarchical reduce, built once
//!   4. Ranking ★             — map per-candidate → reduce to top N
//!   5. Post-process          — validate → noise → diversity → refill
//!   6. Emit                  — RankingOutput

use regex::Regex;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use crate::git_mining::SubsystemDef;
use crate::provider::{ModelProvider, ModelRequest};
use crate::significance_ranking_stages;
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

/// True if a candidate should be dropped from the primary pool — either its
/// message reads like routine churn, or every file it touched is a
/// lockfile/config. Note that "dropped" here means "moved to the noise pool":
/// `post_process` can still re-admit noise items if the non-noise pool can't
/// meet `target_min`.
pub fn is_noise_commit(c: &CandidateInput) -> bool {
    if noise_msg_re().is_match(c.commit.message_summary.trim()) {
        return true;
    }
    if c.commit.files_changed.is_empty() {
        return true;
    }
    c.commit
        .files_changed
        .iter()
        .all(|f| noise_path_re().is_match(&f.path))
}

/// Score-descending comparator, tie-broken stably. `partial_cmp` returns
/// `None` only for NaN, which shouldn't occur given the ranking schema types
/// `score` as a JSON number — but treating NaN as equal (rather than
/// panicking) keeps a single bad model response from taking down the pipeline.
fn by_score_desc(a: &ModelScoredCandidate, b: &ModelScoredCandidate) -> Ordering {
    b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal)
}

/// Deterministic selection + ranking. No LLM calls.
///
/// Two phases:
///
/// **Phase 1 — select the set.** Tiered fallback until `target_min` entries
/// are chosen or the pools are exhausted:
///   1. Diversity floor — `min_per_subsystem` per primary subsystem, by score.
///   2. Global fill from the non-noise pool, by score, capped at `target_max`.
///   3. Shortfall refill — highest-scoring noise items, only if (1)+(2) fell
///      below `target_min`. This is the deterministic substitute for the
///      plan's "re-query the model with the excluded set": since Stage 4
///      already scored every candidate, re-querying would just return the
///      same scores. Re-admitting from the local pools is strictly better —
///      no extra latency, no extra cost, fully reproducible from cache.
///
/// **Phase 2 — rank the set.** Sort the selected set by final score, then
/// assign 1-based ranks. Selection method no longer influences rank once the
/// set is fixed (a diversity-floor pick no longer outranks a higher-scoring
/// global-fill pick purely because it was selected first).
///
/// `target_max` truncation is authoritative: if the diversity floor plus
/// global fill would exceed it, the extra entries are dropped and the floor
/// is treated as best-effort.
///
/// Returns `(ranked, below_target_min)` where `below_target_min` is `true`
/// iff the final result is still short of `cfg.target_min` after refill.
pub fn post_process(
    ranked_raw: Vec<ModelScoredCandidate>,
    candidates: &[CandidateInput],
    cfg: &RankingConfig,
) -> (Vec<RankedCandidate>, bool) {
    let by_hash: HashMap<&str, &CandidateInput> = candidates
        .iter()
        .map(|c| (c.commit.hash.as_str(), c))
        .collect();

    // 1. Hash validation — drop hallucinations first so subsequent steps can
    //    index `by_hash` without a missing-key guard.
    let validated: Vec<ModelScoredCandidate> = ranked_raw
        .into_iter()
        .filter(|r| by_hash.contains_key(r.commit_hash.as_str()))
        .collect();

    // 2. Partition into non-noise and noise pools, each score-sorted. The
    //    noise pool is retained rather than discarded so a shortfall against
    //    `target_min` can be remedied without another LLM call.
    let (mut non_noise, mut noise): (Vec<_>, Vec<_>) = validated
        .into_iter()
        .partition(|r| !is_noise_commit(by_hash[r.commit_hash.as_str()]));
    non_noise.sort_by(by_score_desc);
    noise.sort_by(by_score_desc);

    // 3. Selection
    //    Bucket non-noise candidates by primary subsystem for the diversity
    //    floor. Vectors inherit the parent's score order; sort defensively in
    //    case that invariant ever changes.
    let mut buckets: HashMap<String, Vec<ModelScoredCandidate>> = HashMap::new();
    for r in &non_noise {
        let primary = by_hash[r.commit_hash.as_str()]
            .commit
            .subsystems
            .first()
            .cloned()
            .unwrap_or_else(|| "unknown".into());
        buckets.entry(primary).or_default().push(r.clone());
    }
    for v in buckets.values_mut() {
        v.sort_by(by_score_desc);
    }

    let mut selected: Vec<ModelScoredCandidate> = Vec::new();
    let mut selected_hashes: HashSet<String> = HashSet::new();

    // (a) diversity floor
    for list in buckets.values() {
        for r in list.iter().take(cfg.min_per_subsystem) {
            if selected_hashes.insert(r.commit_hash.clone()) {
                selected.push(r.clone());
            }
        }
    }
    // (b) global fill from non-noise
    for r in &non_noise {
        if selected.len() >= cfg.target_max {
            break;
        }
        if selected_hashes.insert(r.commit_hash.clone()) {
            selected.push(r.clone());
        }
    }
    // (c) shortfall refill from noise
    if selected.len() < cfg.target_min {
        for r in &noise {
            if selected.len() >= cfg.target_min {
                break;
            }
            if selected_hashes.insert(r.commit_hash.clone()) {
                selected.push(r.clone());
            }
        }
    }
    selected.truncate(cfg.target_max);

    // 4. Rank the selected set by final score (selection order no longer
    //    matters once the set is fixed).
    selected.sort_by(by_score_desc);

    let below_target_min = selected.len() < cfg.target_min;

    // 5. Materialize
    let ranked = selected
        .into_iter()
        .enumerate()
        .map(|(i, r)| {
            let c = by_hash[r.commit_hash.as_str()];
            let mut sig =
                SignalSummary::from_candidate(c, cfg.discussion_rich_comment_threshold);
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
        })
        .collect();

    (ranked, below_target_min)
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
///
/// `commit_summaries` is Stage 2's output, keyed by commit hash. When present
/// for a candidate, its prose (`what_changed` / `why` / `takeaway`) is appended
/// beneath the raw signals — the model gets both the mechanical evidence and a
/// natural-language synthesis to reason over. Empty map is fine: the prompt
/// degrades to just the raw signal block, which is what Stage 4 saw before
/// Stage 2 existed.
pub fn build_ranking_prompt(
    candidates: &[CandidateInput],
    commit_summaries: &HashMap<String, crate::significance_ranking_stages::CommitSummary>,
    project_summary: &str,
    target_max: usize,
) -> String {
    // Per-field cap so a single verbose summary can't blow the context budget
    // when the candidate set is large. Tune against the demo repo.
    const SUMMARY_FIELD_CAP: usize = 240;

    fn clip(s: &str, cap: usize) -> String {
        let mut out: String = s.chars().take(cap).collect();
        if s.chars().count() > cap {
            out.push('…');
        }
        out
    }

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
            c.commit
                .files_changed
                .iter()
                .map(|f| f.path.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            h.is_revert,
            h.was_reverted,
            h.touches_repeated_fix_file,
            h.incident_refs,
            c.pr.pr_comment_count,
            c.pr.linked_issues,
        ));

        if let Some(s) = commit_summaries.get(&c.commit.hash) {
            out.push_str("  summary:\n");
            out.push_str(&format!(
                "    changed:  {}\n",
                clip(&s.what_changed, SUMMARY_FIELD_CAP)
            ));
            out.push_str(&format!(
                "    why:      {}\n",
                clip(&s.why, SUMMARY_FIELD_CAP)
            ));
            out.push_str(&format!(
                "    takeaway: {}\n",
                clip(&s.takeaway, SUMMARY_FIELD_CAP)
            ));
        }
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
    let arr = v
        .get("candidates")
        .and_then(|c| c.as_array())
        .ok_or_else(|| "response missing 'candidates' array".to_string())?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let h = item
            .get("commit_hash")
            .and_then(|x| x.as_str())
            .ok_or("missing commit_hash")?;
        let s = item.get("score").and_then(|x| x.as_f64()).unwrap_or(0.0) as f32;
        let a = item
            .get("architecture_shaping")
            .and_then(|x| x.as_bool())
            .unwrap_or(false);
        let r = item
            .get("rationale")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let e = item
            .get("evidence_summary")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        out.push(ModelScoredCandidate {
            commit_hash: h.to_string(),
            score: s,
            architecture_shaping: a,
            rationale: r,
            evidence_summary: e,
        });
    }
    Ok(out)
}

// ---------- Orchestrator ----------

pub async fn run_ranking(
    candidates: Vec<CandidateInput>,
    cfg: RankingConfig,
    repo_path: &std::path::Path,
    cache_root: &std::path::Path,
    subsystems: &[SubsystemDef],
    provider: &dyn ModelProvider,
) -> Result<RankingOutput, String> {
    if candidates.is_empty() {
        return Err("no candidates to rank".into());
    }

    // ---- Stage 1–3 — enrichment (skipped unless the cfg flags are on) ----
    let mut project_summary = String::new();
    let mut file_summaries = std::collections::HashMap::new();
    let mut commit_summaries = std::collections::HashMap::new();

    if cfg.use_file_summaries || cfg.use_commit_summaries {
        // Stage 1 — per-file summaries (disk-cached; candidate-touched files only).
        if cfg.use_file_summaries {
            file_summaries = significance_ranking_stages::summarize_candidate_files(
                repo_path,
                &candidates,
                cache_root,
                provider,
                &cfg.model_id,
            )
            .await;
        }

        // Stage 2 — per-commit summaries (disk-cached; uses Stage 1 output when
        // available, degrades to message + file list otherwise).
        if cfg.use_commit_summaries {
            for c in &candidates {
                match significance_ranking_stages::summarize_commit(
                    c,
                    &file_summaries,
                    cache_root,
                    provider,
                    &cfg.model_id,
                )
                .await
                {
                    Ok(s) => {
                        commit_summaries.insert(c.commit.hash.clone(), s);
                    }
                    Err(e) => eprintln!("[p3] commit summary skipped {}: {e}", c.commit.hash),
                }
            }
        }

        // Stage 3 — hierarchical project summary (disk-cached; one per run).
        //
        // Gated on Stage 1 having actually produced file context: without file
        // summaries the subsystem reduce has nothing to describe and would
        // emit a hollow repo summary that then misleads the ranking prompt.
        if !file_summaries.is_empty() {
            if let Ok(s) = significance_ranking_stages::build_project_summary(
                repo_path,
                subsystems,
                &file_summaries,
                cache_root,
                &cfg.prompt_version,
                provider,
                &cfg.model_id,
            )
            .await
            {
                project_summary = s;
            }
        }
    }

    // ---- Stage 4 — the ranking call (the actual deliverable) ----
    let prompt = build_ranking_prompt(
        &candidates,
        &commit_summaries,
        &project_summary,
        cfg.target_max,
    );
    let resp = provider
        .call(ModelRequest {
            system: "You rank git commits for onboarding tours. Return strict JSON.".into(),
            user: prompt,
            schema: Some(ranking_response_schema()),
            temperature: cfg.temperature,
            model_id: cfg.model_id.clone(),
            max_tokens: Some(4096),
        })
        .await?;

    let parsed = resp
        .parsed
        .or_else(|| serde_json::from_str::<serde_json::Value>(&resp.text).ok())
        .ok_or_else(|| "model returned unparseable output".to_string())?;
    let scored = parse_ranking_response(&parsed)?;

    // ---- Stage 5 — deterministic post-process (no LLM) ----
    // validate → noise → diversity → refill → rank
    let (ranked, below_target_min) = post_process(scored, &candidates, &cfg);

    // ---- Stage 6 — emit ----
    Ok(RankingOutput {
        tour_candidates: ranked,
        meta: RankingMeta {
            prompt_version: cfg.prompt_version,
            model_id: resp.model_id,
            generated_at_utc: chrono::Utc::now().to_rfc3339(),
            candidate_count: candidates.len(),
            below_target_min,
        },
    })
}

// ---------- Tauri IPC command ----------

/// Person 5's frontend calls this after P1's extraction (and P2's enrichment,
/// when available) to get the ranked tour candidates for P4.
///
/// `subsystems` should be the same `Vec<SubsystemDef>` used for extraction —
/// Stage 3 needs the path prefixes to bucket files correctly.
///
/// NOTE: `provider` is injected. Until P6's provider lands, the caller passes
/// a stub implementation. See `lib.rs` for the wiring point.
#[tauri::command]
pub async fn rank_significant_commits(
    candidates: Vec<CandidateInput>,
    repo_path: String,
    cache_root: String,
    subsystems: Vec<SubsystemDef>,
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
        &subsystems,
        provider.as_ref(),
    )
    .await
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
                author_name: "a".into(),
                author_email: "a@b".into(),
                timestamp_utc: "2024-01-01T00:00:00Z".into(),
                message_summary: subject.into(),
                subsystems: subsystems.iter().map(|s| s.to_string()).collect(),
                files_changed: files
                    .iter()
                    .map(|p| FileChange {
                        path: p.to_string(),
                        additions: 1,
                        deletions: 0,
                        status: "M".into(),
                    })
                    .collect(),
                heuristics: HeuristicFlags::default(),
            },
            pr: PrEnrichment::default(),
        }
    }

    fn scored(hash: &str, score: f32) -> ModelScoredCandidate {
        ModelScoredCandidate {
            commit_hash: hash.into(),
            score,
            architecture_shaping: false,
            rationale: String::new(),
            evidence_summary: String::new(),
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
        let c = mk(
            "h1",
            "Fix race condition in scheduler",
            &["backend"],
            &["src/lib.rs"],
        );
        assert!(!is_noise_commit(&c));
    }

    #[test]
    fn hash_validation_drops_hallucinations() {
        let real = vec![mk("real1", "Fix bug", &["backend"], &["src/a.rs"])];
        let pool = vec![scored("real1", 0.9), scored("ghost", 0.99)];
        let (out, _) = post_process(pool, &real, &RankingConfig::default());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].commit_hash, "real1");
    }

    #[test]
    fn diversity_floor_prefers_spread() {
        // 6 backend commits (score 0.9) + 2 frontend (score 0.1)
        // with min_per_subsystem=2, both frontend entries should survive.
        let mut cands = vec![];
        let mut pool = vec![];
        for i in 0..6 {
            let h = format!("b{i}");
            cands.push(mk(&h, "feat backend", &["backend"], &["src/b.rs"]));
            pool.push(scored(&h, 0.9));
        }
        for i in 0..2 {
            let h = format!("f{i}");
            cands.push(mk(&h, "feat frontend", &["frontend"], &["src/f.js"]));
            pool.push(scored(&h, 0.1));
        }
        let (out, _) = post_process(pool, &cands, &RankingConfig::default());
        let frontend_count = out.iter().filter(|c| c.subsystem == "frontend").count();
        assert!(
            frontend_count >= 2,
            "expected diversity floor to keep both frontend entries"
        );
    }

    #[test]
    fn refill_from_noise_when_below_target_min() {
        // All three candidates are noise-flagged ("chore:" messages). The
        // non-noise pool is empty, so the only way to reach target_min=2 is
        // to re-admit noise by score.
        let mut cfg = RankingConfig::default();
        cfg.target_min = 2;
        cfg.target_max = 15;

        let cands = vec![
            mk("n1", "chore: tidy", &["backend"], &["src/a.rs"]),
            mk("n2", "chore: cleanup", &["backend"], &["src/b.rs"]),
            mk("n3", "chore: format", &["backend"], &["src/c.rs"]),
        ];
        let pool = vec![scored("n1", 0.9), scored("n2", 0.5), scored("n3", 0.1)];

        let (out, below) = post_process(pool, &cands, &cfg);
        assert_eq!(out.len(), 2, "refill should top up to target_min");
        // highest-scoring noise is ranked first after the final sort
        assert_eq!(out[0].commit_hash, "n1");
        assert_eq!(out[1].commit_hash, "n2");
        assert!(
            !below,
            "below_target_min should be false after a successful refill"
        );
    }

    #[test]
    fn below_target_min_when_pool_exhausted() {
        // Only two non-noise candidates exist in the pool, but target_min is
        // five. Refill has nothing left to admit; the flag should report the
        // shortfall.
        let mut cfg = RankingConfig::default();
        cfg.target_min = 5;
        cfg.target_max = 15;

        let cands = vec![
            mk("a", "Fix race in scheduler", &["backend"], &["src/a.rs"]),
            mk("b", "Fix deadlock in pool", &["backend"], &["src/b.rs"]),
        ];
        let pool = vec![scored("a", 0.9), scored("b", 0.5)];

        let (out, below) = post_process(pool, &cands, &cfg);
        assert_eq!(out.len(), 2);
        assert!(
            below,
            "2 < target_min=5 should report below_target_min"
        );
    }
}