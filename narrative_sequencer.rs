//! Person 4 — Narrative Sequencer & Generation
//! =============================================
//!
//! Responsibilities implemented here:
//!   1. Order selected commits/PRs into a tour (chronological default, or
//!      clustered by subsystem).
//!   2. Generate a per-stop narrative: what changed / what prompted it / what a
//!      new hire should take away.
//!   3. Enforce a hard no-verbatim-reproduction rule on any PR discussion text
//!      that gets paraphrased into a narrative.
//!   4. Call the model ONLY through the shared abstraction interface (owned by
//!      Person 6) — never directly.
//!   5. Guarantee every stop links back to its source commit/PR.
//!   6. Emit the finished tour as JSON for Person 5 to consume.
//!
//! This file has zero knowledge of how commits were selected (Person 1/2) or
//! how the tour is rendered (Person 5) — it only knows the input/output
//! contracts below, so it can be developed and tested in isolation.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// 1. Model abstraction interface (contract with Person 6)
// ---------------------------------------------------------------------------
// Person 6 owns the real implementation (rate limiting, retries, provider
// choice, prompt logging, etc). Person 4 and Person 3 both code against this
// interface only, so the underlying model call is swappable without touching
// sequencing/generation logic.

/// Abstraction boundary. Do not call any model SDK directly outside this.
pub trait ModelInterface {
    /// Send `prompt` to the underlying model and return the text response.
    fn call_model(&self, prompt: &str, max_tokens: usize) -> String;
}

/// A trivial stand-in so this module runs/tests before Person 6's real
/// interface lands. Replace with the real implementation at wiring time —
/// nothing else in this file changes.
pub struct EchoStubModel;

impl ModelInterface for EchoStubModel {
    fn call_model(&self, prompt: &str, _max_tokens: usize) -> String {
        format!(
            "[STUB OUTPUT — replace EchoStubModel with Person 6's real \
             ModelInterface implementation]\n\
             (prompt was {} chars)",
            prompt.len()
        )
    }
}

// ---------------------------------------------------------------------------
// 2. Input data contracts
// ---------------------------------------------------------------------------

/// One PR's discussion thread, as handed off by the upstream stage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PRDiscussion {
    pub pr_number: u64,
    pub pr_url: String,
    /// Raw discussion text, source material only — never emitted verbatim.
    pub excerpts: Vec<String>,
}

/// A single commit selected upstream for inclusion in the tour.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectedCommit {
    pub commit_hash: String,
    pub commit_url: String,
    pub message: String,
    pub author: String,
    /// ISO 8601 timestamp string.
    pub timestamp: String,
    /// e.g. "auth", "billing", "infra" — set by upstream tagging.
    pub subsystem: String,
    pub files_changed: Vec<String>,
    /// Short structured summary of the diff, NOT the raw diff.
    pub diff_summary: String,
    pub pr: Option<PRDiscussion>,
}

// ---------------------------------------------------------------------------
// 3. Output data contract (what Person 5 consumes)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TourStop {
    pub order: usize,
    pub commit_hash: String,
    pub commit_url: String,
    pub pr_number: Option<u64>,
    pub pr_url: Option<String>,
    pub subsystem: String,
    pub timestamp: String,
    pub what_changed: String,
    pub what_prompted_it: String,
    pub new_hire_takeaway: String,
}

// ---------------------------------------------------------------------------
// 4. Ordering
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderMode {
    Chronological,
    SubsystemCluster,
}

/// Order the selected commits into tour sequence.
///
/// - `Chronological` (default): earliest first.
/// - `SubsystemCluster`: group by subsystem (in `subsystem_order` if given,
///   else first-seen order), chronological within each cluster.
pub fn order_commits(
    mut commits: Vec<SelectedCommit>,
    mode: OrderMode,
    subsystem_order: Option<&[String]>,
) -> Vec<SelectedCommit> {
    match mode {
        OrderMode::Chronological => {
            commits.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
            commits
        }
        OrderMode::SubsystemCluster => {
            // Build subsystem rank: use caller-supplied order, or first-seen.
            let rank: HashMap<String, usize> = match subsystem_order {
                Some(order) => order
                    .iter()
                    .enumerate()
                    .map(|(i, s)| (s.clone(), i))
                    .collect(),
                None => {
                    let mut seen: Vec<String> = Vec::new();
                    for c in &commits {
                        if !seen.contains(&c.subsystem) {
                            seen.push(c.subsystem.clone());
                        }
                    }
                    seen.into_iter().enumerate().map(|(i, s)| (s, i)).collect()
                }
            };
            let fallback = rank.len();
            commits.sort_by(|a, b| {
                let ra = rank.get(&a.subsystem).copied().unwrap_or(fallback);
                let rb = rank.get(&b.subsystem).copied().unwrap_or(fallback);
                ra.cmp(&rb).then_with(|| a.timestamp.cmp(&b.timestamp))
            });
            commits
        }
    }
}

// ---------------------------------------------------------------------------
// 5. Verbatim-reproduction guard (copyright requirement)
// ---------------------------------------------------------------------------
// PR discussion excerpts are source material for paraphrasing only. This
// guard catches cases where a generated narrative accidentally reproduces a
// long run of the original text, so a violation is caught deterministically
// rather than trusted to the model's instruction-following alone.

fn normalize(text: &str) -> Vec<String> {
    let re = Regex::new(r"[a-z0-9']+").unwrap();
    re.find_iter(&text.to_lowercase())
        .map(|m| m.as_str().to_owned())
        .collect()
}

/// Return the length (in words) of the longest run of words shared
/// verbatim between `generated` and `source`.
pub fn longest_shared_ngram(generated: &str, source: &str) -> usize {
    let g = normalize(generated);
    let s = normalize(source);
    if g.is_empty() || s.is_empty() {
        return 0;
    }

    // Build a word → positions index over the source sequence.
    let mut s_index: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, w) in s.iter().enumerate() {
        s_index.entry(w.as_str()).or_default().push(i);
    }

    let mut best = 0usize;
    for i in 0..g.len() {
        if let Some(positions) = s_index.get(g[i].as_str()) {
            for &j in positions {
                let mut k = 0usize;
                while i + k < g.len() && j + k < s.len() && g[i + k] == s[j + k] {
                    k += 1;
                }
                best = best.max(k);
            }
        }
    }
    best
}

/// Words — beyond this, treat as verbatim reproduction.
const MAX_ALLOWED_SHARED_RUN: usize = 6;

pub fn violates_verbatim_rule(generated_text: &str, source_excerpts: &[String]) -> bool {
    source_excerpts
        .iter()
        .any(|src| longest_shared_ngram(generated_text, src) > MAX_ALLOWED_SHARED_RUN)
}

// ---------------------------------------------------------------------------
// 6. Narrative generation
// ---------------------------------------------------------------------------

const NARRATIVE_PROMPT_TEMPLATE: &str = "\
You are writing one stop of a codebase \"narrative tour\" for new engineering hires.

Given the commit and PR context below, produce THREE short sections:
1. WHAT CHANGED — a plain-language summary of the change itself.
2. WHAT PROMPTED IT — why this change happened, based on the PR discussion.
3. TAKEAWAY — one or two sentences a new hire should remember from this.

CRITICAL RULES:
- Paraphrase everything. Never quote PR discussion text verbatim, not even short phrases.
- Do not copy more than a few consecutive words from the source material.
- Keep each section to 2-4 sentences.
- Output exactly in this format, with no extra commentary:
WHAT_CHANGED: ...
WHAT_PROMPTED_IT: ...
TAKEAWAY: ...

--- COMMIT ---
Message: {message}
Files changed: {files}
Diff summary: {diff_summary}

--- PR DISCUSSION (paraphrase only, do not quote) ---
{discussion}
";

fn build_prompt(commit: &SelectedCommit) -> String {
    let discussion = match &commit.pr {
        Some(pr) => pr.excerpts.join("\n"),
        None => "(no linked PR discussion)".to_owned(),
    };
    let files = if commit.files_changed.is_empty() {
        "(not specified)".to_owned()
    } else {
        commit.files_changed.join(", ")
    };
    let diff_summary = if commit.diff_summary.is_empty() {
        "(not specified)"
    } else {
        &commit.diff_summary
    };

    NARRATIVE_PROMPT_TEMPLATE
        .replace("{message}", &commit.message)
        .replace("{files}", &files)
        .replace("{diff_summary}", diff_summary)
        .replace("{discussion}", &discussion)
}

/// The three narrative sections parsed out of a model response.
#[derive(Debug, Clone)]
pub struct NarrativeSections {
    pub what_changed: String,
    pub what_prompted_it: String,
    pub takeaway: String,
}

impl NarrativeSections {
    fn combined(&self) -> String {
        format!("{} {} {}", self.what_changed, self.what_prompted_it, self.takeaway)
    }

    fn withheld() -> Self {
        let msg = "[content withheld: could not paraphrase below the verbatim-overlap \
                   threshold — flag for manual review]"
            .to_owned();
        NarrativeSections {
            what_changed: msg.clone(),
            what_prompted_it: msg.clone(),
            takeaway: msg,
        }
    }
}

fn parse_sections(raw: &str) -> NarrativeSections {
    // Try to pull out the three labelled sections.
    let re = Regex::new(
        r"(?s)WHAT_CHANGED:\s*(?P<changed>.*?)\s*WHAT_PROMPTED_IT:\s*(?P<prompted>.*?)\s*TAKEAWAY:\s*(?P<takeaway>.*)",
    )
    .unwrap();

    if let Some(caps) = re.captures(raw) {
        NarrativeSections {
            what_changed: caps["changed"].trim().to_owned(),
            what_prompted_it: caps["prompted"].trim().to_owned(),
            takeaway: caps["takeaway"].trim().to_owned(),
        }
    } else {
        // Model didn't follow format — fall back to putting everything in
        // what_changed so nothing silently disappears; caller can flag for review.
        NarrativeSections {
            what_changed: raw.trim().to_owned(),
            what_prompted_it: String::new(),
            takeaway: String::new(),
        }
    }
}

pub struct NarrativeGenerator<'a> {
    model: &'a dyn ModelInterface,
    max_retries: usize,
}

impl<'a> NarrativeGenerator<'a> {
    pub fn new(model: &'a dyn ModelInterface, max_retries: usize) -> Self {
        NarrativeGenerator { model, max_retries }
    }

    pub fn generate_stop_narrative(&self, commit: &SelectedCommit) -> NarrativeSections {
        let source_excerpts: Vec<String> = commit
            .pr
            .as_ref()
            .map(|pr| pr.excerpts.clone())
            .unwrap_or_default();

        let mut prompt = build_prompt(commit);
        let mut last_sections: Option<NarrativeSections> = None;

        for _attempt in 0..=self.max_retries {
            let raw = self.model.call_model(&prompt, 500);
            let sections = parse_sections(&raw);

            if !violates_verbatim_rule(&sections.combined(), &source_excerpts) {
                return sections;
            }

            last_sections = Some(sections);
            // Escalate the instruction on retry.
            prompt = format!(
                "Your previous answer copied wording too closely from the source. \
                 Rewrite fully in your own words, changing sentence structure, \
                 with no run of more than a few words matching the source text.\n\n{}",
                prompt
            );
        }

        // Exhausted retries: strip anything that still overlaps rather than
        // ship a verbatim fragment.
        Self::sanitize(last_sections.unwrap(), &source_excerpts)
    }

    fn sanitize(sections: NarrativeSections, source_excerpts: &[String]) -> NarrativeSections {
        let withheld = "[content withheld: could not paraphrase below the verbatim-overlap \
                        threshold — flag for manual review]"
            .to_owned();

        NarrativeSections {
            what_changed: if violates_verbatim_rule(&sections.what_changed, source_excerpts) {
                withheld.clone()
            } else {
                sections.what_changed
            },
            what_prompted_it: if violates_verbatim_rule(&sections.what_prompted_it, source_excerpts)
            {
                withheld.clone()
            } else {
                sections.what_prompted_it
            },
            takeaway: if violates_verbatim_rule(&sections.takeaway, source_excerpts) {
                withheld
            } else {
                sections.takeaway
            },
        }
    }
}

// ---------------------------------------------------------------------------
// 7. Tour assembly
// ---------------------------------------------------------------------------

pub fn build_tour(
    commits: Vec<SelectedCommit>,
    model: &dyn ModelInterface,
    mode: OrderMode,
    subsystem_order: Option<&[String]>,
) -> Vec<TourStop> {
    let ordered = order_commits(commits, mode, subsystem_order);
    let generator = NarrativeGenerator::new(model, 2);

    ordered
        .into_iter()
        .enumerate()
        .map(|(i, commit)| {
            let sections = generator.generate_stop_narrative(&commit);
            TourStop {
                order: i + 1,
                commit_hash: commit.commit_hash.clone(),
                commit_url: commit.commit_url.clone(),
                pr_number: commit.pr.as_ref().map(|pr| pr.pr_number),
                pr_url: commit.pr.as_ref().map(|pr| pr.pr_url.clone()),
                subsystem: commit.subsystem.clone(),
                timestamp: commit.timestamp.clone(),
                what_changed: sections.what_changed,
                what_prompted_it: sections.what_prompted_it,
                new_hire_takeaway: sections.takeaway,
            }
        })
        .collect()
}

#[derive(Serialize)]
struct TourPayload<'a> {
    generated_at: String,
    order_mode: OrderMode,
    stop_count: usize,
    stops: &'a [TourStop],
}

pub fn tour_to_json(stops: &[TourStop], mode: OrderMode) -> String {
    let payload = TourPayload {
        generated_at: Utc::now().to_rfc3339(),
        order_mode: mode,
        stop_count: stops.len(),
        stops,
    };
    serde_json::to_string_pretty(&payload).expect("serialisation of TourPayload cannot fail")
}

// ---------------------------------------------------------------------------
// 8. Example / smoke test (uses the stub model — swap for the real one)
// ---------------------------------------------------------------------------

fn main() {
    let sample_commits = vec![
        SelectedCommit {
            commit_hash: "a1b2c3d".to_owned(),
            commit_url: "https://github.com/death7654/GitLens/commit/a1b2c3d".to_owned(),
            message: "Switch session tokens to rotating refresh tokens".to_owned(),
            author: "alice".to_owned(),
            timestamp: "2025-03-01T10:00:00".to_owned(),
            subsystem: "auth".to_owned(),
            files_changed: vec![
                "auth/session.py".to_owned(),
                "auth/tokens.py".to_owned(),
            ],
            diff_summary: "Replaced long-lived JWTs with short-lived access tokens plus a refresh flow.".to_owned(),
            pr: Some(PRDiscussion {
                pr_number: 142,
                pr_url: "https://github.com/death7654/GitLens/pull/142".to_owned(),
                excerpts: vec![
                    "we kept seeing tokens leak through logs because they lived for 30 days".to_owned(),
                    "rotating refresh tokens cut our exposure window down to about 15 minutes".to_owned(),
                ],
            }),
        },
        SelectedCommit {
            commit_hash: "e4f5g6h".to_owned(),
            commit_url: "https://github.com/death7654/GitLens/commit/e4f5g6h".to_owned(),
            message: "Add idempotency keys to billing webhook handler".to_owned(),
            author: "bob".to_owned(),
            timestamp: "2025-02-15T09:00:00".to_owned(),
            subsystem: "billing".to_owned(),
            files_changed: vec!["billing/webhooks.py".to_owned()],
            diff_summary: "Webhook handler now dedupes on a client-supplied idempotency key.".to_owned(),
            pr: Some(PRDiscussion {
                pr_number: 98,
                pr_url: "https://github.com/death7654/GitLens/pull/98".to_owned(),
                excerpts: vec![
                    "stripe retries the same webhook multiple times and we were double-charging".to_owned(),
                ],
            }),
        },
    ];

    let stops = build_tour(sample_commits, &EchoStubModel, OrderMode::Chronological, None);
    println!("{}", tour_to_json(&stops, OrderMode::Chronological));
}
git 