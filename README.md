# Onboarding Ghost 👻
### A GitLens feature — built for the IBM Bob 2.0 Hackathon

> New to a codebase? Skip the "spelunk through six years of git blame" phase.
> Onboarding Ghost mines your repository's history for the commits that
> actually shaped the project — the reverts, the incidents, the repeated
> fixes, the architecture-defining refactors — and turns them into a
> narrated, click-through **tour**.

---

## What it does

Point Onboarding Ghost at any local git repository and it will:

1. **Mine** the commit history, concurrently, one worker per subsystem
   (frontend / backend / db-schema / whatever you define).
2. **Pre-filter** with cheap, deterministic heuristics — no LLM calls yet —
   to keep only commits that look like they mattered:
   - `git revert`-generated commits, and commits that were *later* reverted
   - commits touching a "repeated-fix" file (patched ≥2 times by other fix
     commits in the window)
   - commits referencing an incident/bug tracker (`INC-4821`, `#987`, …)
3. **Rank** the survivors with an LLM across four signal families —
   architecture-shaping, revert/revert-chain, incident-linked, and
   discussion-rich — while downweighting dependency bumps, formatting
   passes, and lockfile-only commits. A deterministic post-process pass then
   enforces a diversity floor per subsystem and tops up the set if the model
   under-selects, so the tour always tries to land 10–15 stops.
4. **Narrate** each selected stop with its own LLM call — title + 2–4
   paragraph story — using the commit diff, the selection rationale, and
   (optionally) the linked GitHub issue / Jira ticket body for grounding.
   Every other selected stop's one-line evidence summary is included as
   cross-stop context, so the tour reads as a coherent narrative rather than
   15 isolated blurbs.
5. **Visualize** the result: an SVG commit timeline highlighting every
   selected stop by trigger type, and a Mermaid subsystem co-occurrence graph
   showing which parts of the codebase tend to change together.
6. **Export** the finished tour as a single self-contained offline HTML file
   — no server, no dependency on the app being open — for sharing with a
   new hire directly.

Everything after step 1 is cached to disk (keyed by file content hash,
commit hash, or a repo+config fingerprint as appropriate), so re-running a
tour after a small tweak doesn't re-pay for work that hasn't changed.

## Why "GitLens" / "Onboarding Ghost"?

**GitLens** is the desktop shell — a local-first Tauri app for browsing a
repository, running the mining pipeline, and reading the results. Files stay
on your machine; nothing is uploaded anywhere except the LLM calls
themselves (and, optionally, GitHub/Jira issue fetches you explicitly
enable).

**Onboarding Ghost** is the tour feature living inside it — the piece built
for this hackathon.

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│  Frontend (HTML/CSS/JS, Tauri webview)                       │
│  main.js · commit_graph.js · architecture_diagram.js          │
│  tour_export.js                                               │
└───────────────────────────┬─────────────────────────────────┘
                             │ Tauri IPC (invoke)
┌───────────────────────────▼─────────────────────────────────┐
│  Rust backend (src-tauri)                                     │
│                                                                 │
│  git_mining/            concurrent per-subsystem history walk  │
│    ├─ discovery           auto-detect subsystems from HEAD     │
│    ├─ heuristics          revert / repeated-fix / incident regex│
│    └─ worker               git2-based commit + diff extraction │
│                                                                 │
│  significance_ranking/   4-stage ranking pipeline               │
│    ├─ stage 1–3            file → commit → project summaries    │
│    └─ stage 4–6            LLM ranking → noise/diversity/refill │
│                                                                 │
│  tour_narration          stub join, doc fetch, per-stop         │
│                          narration with retry + disk cache      │
│                                                                 │
│  doc_fetch               GitHub / Jira issue body fetch          │
│                                                                 │
│  provider (trait)  ──┬── bob_provider    IBM Bob Shell CLI      │
│                       ├── gemini_provider Gemini REST API        │
│                       └── mock_provider   deterministic test data│
└─────────────────────────────────────────────────────────────┘
```

Every LLM-touching stage is written against a single `ModelProvider` trait
(`provider.rs`), so swapping the backend is a one-line config change, not a
code change. Three implementations ship in this repo:

| Provider | File | Backend | Auth |
|---|---|---|---|
| **Bob Shell** | `bob_provider.rs` | Shells out to the `bob` CLI (`bob --auth-method api-key --hide-intermediary-output -p "<prompt>"`) | `BOB_API_KEY` |
| **Gemini** | `gemini_provider.rs` | Direct REST calls to the Gemini `generateContent` API, with structured JSON output via `responseSchema` | `GEMINI_API_KEY` |
| **Mock** | `mock_provider.rs` | Canned responses, keyed off hint lines embedded in the prompt (`MOCK_TRIGGER:`, `MOCK_FAIL_HASH`, `MOCK_LATENCY_MS:`) | none |

## Getting started

### Prerequisites

- Rust (stable) + `cargo`
- Node.js (for the Tauri CLI / frontend tooling)
- `git`
- An API key for whichever provider you want to run against (or none, for
  the mock provider)

### Configure

Create a `.env` file in `src-tauri/` (next to `Cargo.toml`) — it's loaded
automatically at startup, resolved relative to the crate itself so it works
the same whether you're running `cargo tauri dev` or a built binary:

```env
# Pick ONE of the following setups.

# Option A — Gemini (current default)
GEMINI_API_KEY=your-gemini-api-key

# Option B — Bob Shell (requires the `bob` CLI installed and on PATH)
# MODEL_PROVIDER=bob
# BOB_API_KEY=your-bob-api-key

# Option C — no LLM calls at all, deterministic canned output (great for demos)
# GITLENS_MOCK_PROVIDER=1
```

Environment variables you export yourself (shell, CI, etc.) always take
precedence over `.env` values.

### Run

```bash
cargo tauri dev
```

### Use it

1. **Choose a folder** — pick any local git repository.
2. **Mine history** — accept the auto-discovered subsystems (or hand-edit
   the path-prefix JSON), set an optional age/commit-count cap, and run
   extraction.
3. **Rank & generate tour** — kicks off ranking → document fetch →
   parallel narration. Stop cards stream in as each one finishes; a partial
   failure on one stop doesn't block the rest.
4. **Walk the tour** — arrow keys, the nav buttons, or clicking a highlighted
   dot on the commit timeline all jump between stops.
5. **Export** — download the tour as a standalone HTML file once you're
   happy with it.

## Notes for judges

- The mining heuristics, ranking post-process (diversity floor / noise
  filtering / shortfall refill), and narration retry/caching all have unit
  and integration test coverage under `#[cfg(test)]` in their respective
  modules — see `git_mining.rs`, `significance_ranking.rs`, and
  `tour_narration.rs`.
- Run with `GITLENS_MOCK_PROVIDER=1` for a fully offline, deterministic demo
  that exercises the whole pipeline (including the partial-failure UI path
  via `MOCK_FAIL_HASH`) without needing any API credentials.

## Team

- [Your name / team here]

## License

- [License here]
