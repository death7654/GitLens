# GitLens
### A GitLens feature — built by **WireCoffins** for the IBM Bob 2.0 Hackathon

**License:** AGPL-3.0

> New to a codebase? Skip the "spelunk through six years of git blame" phase.
> Onboarding Ghost mines your repository's history for the commits that
> actually shaped the project — the reverts, the incidents, the repeated
> fixes, the architecture-defining refactors — and turns them into a
> narrated, click-through **tour**.

---
# Project Screenshots
<img width="1277" height="819" alt="image" src="https://github.com/user-attachments/assets/dd71fa49-2706-4151-aa11-19443ed86a2b" />
<img width="1277" height="819" alt="image" src="https://github.com/user-attachments/assets/e5045958-fef6-4cbc-9696-56ab355a1201" />
<img width="1277" height="819" alt="image" src="https://github.com/user-attachments/assets/9e8a88dc-e2be-424c-87d1-66d0aaed0889" />
<img width="1277" height="819" alt="image" src="https://github.com/user-attachments/assets/d81cb1d3-f1c8-4cf5-97da-346d3e3cbaf3" />
<img width="1277" height="819" alt="image" src="https://github.com/user-attachments/assets/5f80bf2c-f3fe-4624-a0c8-f6f4f90768c3" />
<img width="1277" height="819" alt="image" src="https://github.com/user-attachments/assets/73318f73-3a7f-46e2-b693-2ae66b8d5e97" />
<img width="1277" height="819" alt="image" src="https://github.com/user-attachments/assets/cfebcc2e-838a-4346-aa4c-075e885a8c5b" />

---

# IBM Bob Usage
## Robinson George Arysseril
<img width="388" height="207" alt="image" src="https://github.com/user-attachments/assets/80d74089-2f72-4325-b632-d6ee537b4c47" />
<img width="382" height="198" alt="image" src="https://github.com/user-attachments/assets/aeff9643-1589-4220-9cce-4940c07c8b31" />

## Tessa Mariya
<img width="721" height="723" alt="image" src="https://github.com/user-attachments/assets/9e01e4d6-9403-4661-a5d9-0a47404e367d" />
<img width="729" height="752" alt="image" src="https://github.com/user-attachments/assets/d22e11ed-c95e-4542-96ba-70cf6582a962" />

## Rosmi Reji
<img width="875" height="340" alt="image" src="https://github.com/user-attachments/assets/d7e89898-15cc-478d-84c2-c452a509b42d" />
<img width="885" height="332" alt="image" src="https://github.com/user-attachments/assets/67b4f886-f725-4c77-bcc6-5d39e8b47b9c" />

## Evan Biju
<img width="486" height="322" alt="image" src="https://github.com/user-attachments/assets/aa0a6cba-eac6-4fee-8dc4-52ab61b9cdaa" />
<img width="786" height="372" alt="image" src="https://github.com/user-attachments/assets/bb62d271-c825-44d3-ac05-89f7a2d4583b" />
<img width="530" height="266" alt="image" src="https://github.com/user-attachments/assets/312cfb02-5e76-41cc-9972-74da280eb75d" />

## Flavia 
<img width="282" height="377" alt="image" src="https://github.com/user-attachments/assets/ce01ca83-a0bd-4be0-b0a8-ea97ad20b56c" />

## Dhanush
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

## Screenshots

> _Add a few screenshots below — the Overview stat bar, the extraction
> wizard, and a narrated tour stop with the commit timeline / subsystem map
> visible all make good choices._

| | |
|---|---|
| ![Overview](screenshots/overview.png) | ![Extraction wizard](screenshots/extraction.png) |
| ![Tour stop](screenshots/tour-stop.png) | ![Commit timeline](screenshots/commit-timeline.png) |

## Demo video

> _Add your demo video link here (YouTube / Loom / lablab.ai submission
> video)._

[▶ Watch the demo](https://your-demo-video-link-here)

## Team — WireCoffins

| | |
|---|---|
| **Robinson Arysseril** | `robinson_arysseril645` |
| **Tessa Mariya** | `tess__ah` |
| **Evan Iwin Biju** | `evan_iwin_biju42` |
| **Rosmi Reji** | `rosmi_25` |
| **flaviadaryjoseph** | `flaviadaryjoseph452` |
| **DHANUSH SUBHASH** | `lblame_dnu` |

Built for the [IBM Bob 2.0 Hackathon](https://lablab.ai/ai-hackathons/ibm-bob-2-hackathon/wire-coffins) on lablab.ai.

## License

Licensed under the [GNU Affero General Public License v3.0 (AGPL-3.0)](https://www.gnu.org/licenses/agpl-3.0.html).
