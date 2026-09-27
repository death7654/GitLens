/**
 * main.js — GitLens frontend
 *
 * Tauri IPC commands (defined in src-tauri/src/lib.rs and submodules):
 *   read_repo_file(root, relativePath)               → string
 *   extract_git_history(config)                      → MiningOutput
 *   discover_repo_subsystems(repoPath)               → SubsystemDef[]
 *   rank_significant_commits(candidates, repoPath,
 *     cacheRoot, subsystems, cfg?)                   → RankingOutput
 *   fetch_stop_documents(rankingOutput,
 *     miningOutput, cfg?)                            → StopStub[]
 *   narrate_stop(stub, otherEvidenceSummaries,
 *     repoPath, cacheRoot, cfg?)                     → TourStop
 *
 * Folder picking uses tauri-plugin-dialog (open() with directory:true).
 * `withGlobalTauri: true` in tauri.conf.json exposes these as
 * window.__TAURI__.core and window.__TAURI__.dialog.
 */

const { invoke } = window.__TAURI__.core;
const { open }   = window.__TAURI__.dialog;
const { getCurrentWindow } = window.__TAURI__.window;

// ---------------------------------------------------------------------------
// Globals
// ---------------------------------------------------------------------------

/** @type {string} Currently open repo path. */
let repoPath = '';

/** @type {import('./types').MiningOutput|null} */
window.miningOutput  = null;

/** @type {import('./types').RankingOutput|null} */
window.rankingOutput = null;

// Tour state machine
/** @type {'idle'|'ranking'|'fetching_docs'|'narrating'|'ready'|'partial_failure'|'error'} */
let tourState = 'idle';
let currentStopIndex = 0;
let tourStopCount    = 0;

// ---------------------------------------------------------------------------
// DOM helpers
// ---------------------------------------------------------------------------

const $ = (sel) => document.querySelector(sel);

/** @param {string} msg @param {'info'|'error'|'success'} [kind] */
function toast(msg, kind = 'info') {
  const el = $('#toast');
  el.textContent = msg;
  el.className = `toast show toast-${kind}`;
  clearTimeout(el._t);
  el._t = setTimeout(() => el.classList.remove('show'), 3800);
}

/**
 * Append a line to the activity log.
 * @param {string} html  Raw HTML string (use esc() for untrusted content)
 * @param {'info'|'error'|'success'} [kind]
 */
function log(html, kind = 'info') {
  const el = document.createElement('p');
  const iconClass = kind === 'error' ? 'bi-exclamation-circle log-error'
    : kind === 'success' ? 'bi-check-circle log-success'
    : 'bi-circle-fill log-dot';
  el.innerHTML = `<i class="bi ${iconClass}"></i> ${html}`;
  const logEl = $('#activity-log');
  logEl.prepend(el);
  // keep at most 60 entries — remove excess in one splice instead of a loop
  const excess = logEl.children.length - 60;
  if (excess > 0) {
    Array.from(logEl.children).slice(-excess).forEach(c => c.remove());
  }
}

/** HTML-escape untrusted string. */
function esc(str) {
  return String(str)
    .replace(/&/g, '&amp;').replace(/</g, '&lt;')
    .replace(/>/g, '&gt;').replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

/** Format bytes → human-readable. */
function bytes(n) {
  if (n < 1024) return `${n} B`;
  if (n < 1048576) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1048576).toFixed(1)} MB`;
}

/** Switch active sidebar nav item, update topbar breadcrumb. */
function setActiveNav(viewId) {
  document.querySelectorAll('.nav-item').forEach(b => b.classList.remove('active'));
  const btn = document.querySelector(`[data-view="${viewId}"]`);
  if (btn) btn.classList.add('active');
}

/**
 * Show one view, hide the others.
 * @param {'welcome'|'workspace'|'extraction'|'tour'} name
 */
function showView(name) {
  ['view-welcome', 'view-workspace', 'view-extraction', 'view-tour', 'view-settings'].forEach(id => {
    const el = document.getElementById(id);
    if (el) el.hidden = id !== `view-${name}`;
  });
  setActiveNav(name === 'workspace' ? 'overview' : name);
  window.scrollTo({ top: 0, behavior: 'smooth' });
}

// ---------------------------------------------------------------------------
// Folder picker & repo loading
// ---------------------------------------------------------------------------

/**
 * Open the native OS folder picker (tauri-plugin-dialog) and load the result.
 * Returns the chosen path string, or null if the user cancelled.
 * @returns {Promise<string|null>}
 */
async function chooseFolder() {
  try {
    // open() returns a string (single selection) or null if cancelled.
    const chosen = await open({ directory: true, multiple: false, title: 'Choose a repository folder' });
    if (!chosen || typeof chosen !== 'string') return null;
    repoPath = chosen;
    applyRepoPath(repoPath);
    refreshRepoStatus(repoPath);
    return repoPath;
  } catch (e) {
    toast(`Folder picker unavailable: ${e}`, 'error');
    return null;
  }
}

/**
 * Populate the Overview stat bar (branch / working-tree / latest commit)
 * via the `get_repo_status` command. Independent of extraction — safe to
 * call the moment a folder is chosen. Failures (e.g. folder isn't a git
 * repo) degrade to em-dashes rather than blocking the rest of the UI.
 * @param {string} path
 */
async function refreshRepoStatus(path) {
  $('#stat-branch').textContent = '…';
  $('#stat-clean').textContent = '…';
  $('#stat-commit').textContent = '…';
  try {
    const status = await invoke('get_repo_status', { repoPath: path });
    $('#stat-branch').textContent = status.branch;
    $('#stat-clean').textContent = status.is_clean ? 'Clean' : 'Modified';
    $('#stat-commit').textContent = status.latest_commit_summary
      ? `${status.latest_commit_hash} · ${status.latest_commit_summary}`
      : status.latest_commit_hash;
  } catch (err) {
    $('#stat-branch').textContent = '—';
    $('#stat-clean').textContent = '—';
    $('#stat-commit').textContent = '—';
    log(`Could not read repo status: ${esc(String(err))}`, 'error');
  }
}

/**
 * Update every UI element that displays the repo name or path, then switch to
 * the workspace view. Does no I/O — call after chooseFolder() resolves.
 * @param {string} path  Absolute path to the repository root.
 */
function applyRepoPath(path) {
  if (!path) return;
  repoPath = path;
  const name = path.split(/[/\\]/).filter(Boolean).pop() || path;

  // Topbar breadcrumb
  $('#topbar-breadcrumb').innerHTML =
    `<span class="bc-item bc-dim">Local</span>` +
    `<i class="bi bi-chevron-right bc-sep" style="font-size:10px"></i>` +
    `<span class="bc-item bc-active">${esc(name)}</span>`;

  // Sidebar
  $('#sidebar-repo-name').textContent = name;
  $('#sidebar-repo-path').textContent = path;

  // Overview page
  $('#repo-name-heading').textContent = name;
  $('#repo-path-display').textContent = path;

  // Extraction wizard
  $('#mining-repo-path').value = path;

  // Enable sidebar items that need a repo
  $('#nav-history').disabled = false;

  showView('workspace');
  log(`Opened <strong>${esc(name)}</strong>`);
}

// ---------------------------------------------------------------------------
// File explorer
//
// Entries are populated from MiningOutput.commits[].files_changed after
// extraction runs. Before extraction the list is empty.
// ---------------------------------------------------------------------------

/** @type {Array<{path:string, kind:'file'|'directory', size:number, name:string}>} */
let currentEntries = [];

/** @param {string} filter */
function renderEntries(filter = '') {
  const list = $('#file-list');
  list.innerHTML = '';
  const lc = filter.toLowerCase();
  const filtered = filter
    ? currentEntries.filter(e => e.path.toLowerCase().includes(lc))
    : currentEntries;

  filtered.forEach(entry => {
    const btn = document.createElement('button');
    const isDir = entry.kind === 'directory';
    btn.className = `file-row ${entry.kind}`;
    btn.title = entry.path;
    const icon = isDir ? 'bi-folder-fill' : fileIcon(entry.name || entry.path);
    btn.innerHTML =
      `<i class="bi ${icon}"></i>` +
      `<span>${esc(entry.path)}</span>` +
      (isDir ? '' : `<small>${bytes(entry.size || 0)}</small>`);
    if (!isDir) {
      btn.addEventListener('click', () => previewEntry(entry, btn));
    } else {
      btn.disabled = true;
    }
    list.append(btn);
  });

  $('#file-count-label').textContent = `${filtered.length} items`;
  $('#stat-count').textContent = currentEntries.length;
  const badge = $('#nav-files-count');
  badge.textContent = currentEntries.length;
  badge.hidden = false;
}

/** Pick a Bootstrap icon for common file extensions. */
function fileIcon(name) {
  const ext = (name.split('.').pop() || '').toLowerCase();
  const map = {
    rs: 'bi-filetype-rs', js: 'bi-filetype-js', ts: 'bi-filetype-ts',
    html: 'bi-filetype-html', css: 'bi-filetype-css',
    json: 'bi-filetype-json', md: 'bi-filetype-md',
    toml: 'bi-file-code', yaml: 'bi-file-code', yml: 'bi-file-code',
    lock: 'bi-file-earmark-lock', sql: 'bi-file-earmark-code',
    png: 'bi-file-earmark-image', jpg: 'bi-file-earmark-image',
    svg: 'bi-file-earmark-image', pdf: 'bi-file-earmark-pdf',
    txt: 'bi-file-earmark-text', log: 'bi-file-earmark-text',
  };
  return map[ext] || 'bi-file-earmark';
}

/**
 * Load and display a file in the preview panel using the read_repo_file command.
 * The command canonicalises the path server-side and rejects anything that
 * escapes the repo root or is not valid UTF-8.
 * @param {{path:string, name:string, size:number}} entry
 * @param {HTMLElement} btn  The file-row button to mark as selected.
 */
async function previewEntry(entry, btn) {
  document.querySelectorAll('.file-row').forEach(r => r.classList.remove('selected'));
  btn.classList.add('selected');

  const ext = (entry.name || entry.path).split('.').pop().toUpperCase();
  $('#preview-kind').textContent = `${ext} FILE`;
  $('#preview-name').textContent = entry.path;
  $('#preview-size').textContent = bytes(entry.size || 0);
  $('#preview-copy-btn').hidden = false;

  const pre = $('#file-preview');
  pre.textContent = 'Loading…';

  try {
    const content = await invoke('read_repo_file', { root: repoPath, relativePath: entry.path });
    pre.textContent = content;
    log(`Previewing <code>${esc(entry.path)}</code>`);
  } catch (err) {
    pre.innerHTML = `<span class="log-error">${esc(String(err))}</span>`;
  }
}

// ---------------------------------------------------------------------------
// Extraction
// ---------------------------------------------------------------------------

/**
 * Navigate to the extraction wizard for `path` and, if the subsystems box
 * is still empty, auto-run discovery immediately — so the common case
 * ("I opened a repo, now mine it") needs no manual JSON editing before
 * "Run extraction" does anything. This is the fix for extraction appearing
 * to "just sit there": previously the subsystems textarea started empty,
 * clicking Run extraction silently no-opped behind a toast that vanishes
 * in ~4s, and the only way forward was to notice and click Auto-discover
 * by hand.
 * @param {string} path
 */
function enterExtractionWizard(path) {
  $('#mining-repo-path').value = path;
  showView('extraction');
  if (!$('#subsystems-input').value.trim()) {
    discoverSubsystems();
  }
}

/** @returns {Promise<void>} */
async function runExtraction() {
  const pathVal = $('#mining-repo-path').value.trim();
  const status  = $('#mining-status');
  if (!pathVal) {
    status.textContent = 'Enter a repository path first.';
    toast('Enter a repository path first.', 'error');
    return;
  }

  let subsystems;
  const raw = $('#subsystems-input').value.trim();
  if (!raw) {
    status.textContent = 'Define at least one subsystem, or click "Auto-discover" above.';
    toast('Define at least one subsystem (or auto-discover first).', 'error');
    return;
  }
  try { subsystems = JSON.parse(raw); }
  catch (e) {
    status.textContent = `Subsystems JSON is invalid: ${e.message}`;
    toast(`Subsystems JSON is invalid: ${e.message}`, 'error');
    return;
  }
  if (!Array.isArray(subsystems) || subsystems.length === 0) {
    status.textContent = 'Subsystems must be a non-empty array — click "Auto-discover" to fill it in.';
    toast('Subsystems must be a non-empty array.', 'error');
    return;
  }

  const maxAge    = parseInt($('#max-age-input').value, 10) || null;
  const maxCom    = parseInt($('#max-commits-input').value, 10) || null;

  const config = {
    repo_path: pathVal,
    subsystems,
    window: { max_age_days: maxAge, max_commits_per_subsystem: maxCom },
  };

  const btn = $('#run-mining-btn');
  btn.disabled = true;
  btn.innerHTML = '<div class="spinner" style="width:16px;height:16px;border-width:2px"></div> Extracting…';
  status.textContent = 'Scanning commit history…';
  $('#extraction-results').hidden = true;
  log('Starting extraction…');

  const t0 = performance.now();
  try {
    const result = await invoke('extract_git_history', { config });
    const elapsed = ((performance.now() - t0) / 1000).toFixed(2);

    window.miningOutput = result;

    // Populate file list from commits' files_changed
    const pathSet = new Set();
    (result.commits || []).forEach(c => {
      (c.files_changed || []).forEach(f => pathSet.add(f.path));
    });
    currentEntries = [...pathSet].sort().map(p => ({
      path: p, kind: 'file',
      name: p.split('/').pop(),
      size: 0,
    }));
    renderEntries();

    // Show results summary
    showExtractionResults(result, elapsed);

    if (result.candidate_count === 0) {
      // A real, non-error outcome: the heuristic filter (reverts,
      // repeated-fix files, incident refs) only keeps commits matching one
      // of those patterns. A repo without any of those in its scanned
      // window legitimately yields zero — which otherwise looks identical
      // to "extraction is broken". Say so explicitly.
      status.textContent =
        `Done in ${elapsed}s — scanned ${result.total_commits_scanned} commits, ` +
        `0 matched a significance heuristic (revert / repeated-fix / incident reference).`;
      log(
        `Extraction finished but found <strong>0 candidates</strong> across ${result.total_commits_scanned} commits. ` +
        `This repo's history may not contain reverts, repeated-fix files, or incident-tagged commits ` +
        `in the scanned window — try widening "Max age" / "Max commits" above, or check your subsystem ` +
        `path prefixes actually match files in the repo.`,
        'info'
      );
      toast('Extraction finished — 0 significant commits found. See Activity log for why.', 'info');
    } else {
      status.textContent = `Done in ${elapsed}s — ${result.candidate_count} candidates found.`;
      log(`Extracted ${result.total_commits_scanned} commits from ${result.subsystems_scanned.length} subsystem(s), found <strong>${result.candidate_count}</strong> candidates`, 'success');
      toast(`${result.candidate_count} candidates found in ${elapsed}s`, 'success');
    }

    // Enable tour nav item
    $('#nav-tour').disabled = false;
    $('#nav-tour-count').textContent = result.candidate_count;
    $('#nav-tour-count').hidden = false;

  } catch (err) {
    status.textContent = `Extraction failed.`;
    log(`Extraction failed: ${esc(String(err))}`, 'error');
    toast(String(err), 'error');
  } finally {
    btn.disabled = false;
    btn.innerHTML = '<i class="bi bi-play-fill"></i> Run extraction';
  }
}

/** @param {import('./types').MiningOutput} result @param {string} elapsed */
function showExtractionResults(result, elapsed) {
  const bar = $('#results-summary-bar');
  bar.innerHTML = [
    { value: result.total_commits_scanned, label: 'Commits scanned' },
    { value: result.subsystems_scanned.length, label: 'Subsystems' },
    { value: result.candidate_count, label: 'Candidates' },
    { value: `${elapsed}s`, label: 'Duration' },
  ].map(s => `
    <div class="result-stat">
      <span class="result-stat-value">${esc(String(s.value))}</span>
      <span class="result-stat-label">${esc(s.label)}</span>
    </div>`).join('');

  $('#mining-output-pre').textContent = JSON.stringify(result, null, 2);
  $('#extraction-results').hidden = false;

  // Ranking has nothing to work with on zero candidates — disable the
  // proceed button rather than let the user hit a confusing
  // "no candidates to rank" error one step later.
  const proceedBtn = $('#proceed-to-tour-btn');
  proceedBtn.disabled = result.candidate_count === 0;
  proceedBtn.title = result.candidate_count === 0
    ? 'No significant commits were found to rank — widen the mining window or check subsystems.'
    : '';
}

/** Auto-discover subsystems from the repo tree. */
async function discoverSubsystems() {
  const pathVal = $('#mining-repo-path').value.trim();
  if (!pathVal) { toast('Enter a repository path first.', 'error'); return; }

  const btn    = $('#discover-btn');
  const status = $('#discover-status');
  btn.disabled = true;
  btn.innerHTML = '<div class="spinner" style="width:12px;height:12px;border-width:2px"></div> Scanning…';
  status.textContent = 'Scanning repo tree…';

  try {
    const subs = await invoke('discover_repo_subsystems', { repoPath: pathVal });
    $('#subsystems-input').value = JSON.stringify(subs, null, 2);
    status.textContent = subs.length
      ? `Found ${subs.length} subsystem(s) — review and edit before extracting.`
      : 'No top-level directories found (everything at root may have been filtered).';
    if (subs.length) toast(`${subs.length} subsystems discovered`, 'success');
  } catch (err) {
    status.textContent = `Discovery failed: ${String(err)}`;
    toast(String(err), 'error');
  } finally {
    btn.disabled = false;
    btn.innerHTML = '<i class="bi bi-magic"></i> Auto-discover';
  }
}

// ---------------------------------------------------------------------------
// Tour config & cache
// ---------------------------------------------------------------------------

/** Cache directory written next to the repository root at runtime. */
function getCacheRoot() {
  return (window.miningOutput?.repo_path || repoPath) + '/.ghost-cache';
}

/**
 * Build a TourConfig from the credentials panel inputs.
 * model_id defaults to 'gemini-flash-latest' — matches TourConfig::default()
 * in tour_types.rs (a Google-maintained alias, not a pinned version that can
 * be retired). fetch_linked_documents is enabled only when at least one
 * token is provided (the backend enforces the same check and will error if
 * the flag is true but no token is present).
 */
function buildTourConfig() {
  const ghToken   = $('#gh-token-input')?.value?.trim() || null;
  const repoSlug  = $('#repo-slug-input')?.value?.trim() || null;
  const jiraUrl   = $('#jira-url-input')?.value?.trim() || null;
  const jiraToken = $('#jira-token-input')?.value?.trim() || null;
  return {
    narration_prompt_version: 'v1',
    model_id: 'gemini-flash-latest',
    temperature: 0.3,
    max_narration_tokens: 1024,
    fetch_linked_documents: !!(ghToken || jiraToken),
    github_token: ghToken,
    repo_slug: repoSlug,
    jira_base_url: jiraUrl,
    jira_token: jiraToken,
  };
}

/**
 * Full three-step pipeline triggered by "Generate tour".
 *
 * Step 1 — rank_significant_commits: scores every candidate commit and
 *   selects 10-15 tour stops using the model provider.
 * Step 2 — fetch_stop_documents: optionally fetches linked GitHub / Jira
 *   issue bodies and assembles StopStub objects for each ranked candidate.
 * Step 3 — narrate_stop (parallel): fires one narrate_stop call per stub
 *   concurrently, rendering each stop card as its promise resolves.
 */
async function generateTour() {
  if (!window.miningOutput) {
    toast('Run extraction first.', 'error'); return;
  }

  const mining    = window.miningOutput;
  const cfg       = buildTourConfig();
  const cacheRoot = getCacheRoot();

  // Step 1: rank
  setTourState('ranking');
  log('Ranking candidates…');

  // Wrap each CommitRecord in the CandidateInput shape the backend expects.
  // The `pr` field is optional enrichment (PR comment counts, linked issues,
  // etc.) — we pass an empty object since this data isn't available locally.
  const candidates = mining.commits.map(c => ({ commit: c, pr: {} }));
  // Subsystem definitions only need names here; path_prefixes were already
  // used during extraction and are not consulted again by the ranker.
  const subsystems = mining.subsystems_scanned.map(name => ({
    name,
    path_prefixes: [],
  }));

  let ranking;
  try {
    ranking = await invoke('rank_significant_commits', {
      candidates,
      repoPath: mining.repo_path,
      cacheRoot,
      subsystems,
      cfg: null,  // null → RankingConfig::default() (target 10-15 stops)
    });
    window.rankingOutput = ranking;
    log(`Ranking complete — ${ranking.tour_candidates.length} tour stops selected`, 'success');
    window.tourOnRankingReady?.();
  } catch (err) {
    setTourState('error', { message: `Ranking failed: ${err}` });
    log(`Ranking failed: ${esc(String(err))}`, 'error');
    return;
  }

  // Step 2: fetch stop documents
  setTourState('fetching_docs');
  log('Fetching linked documents…');

  let stubs;
  try {
    stubs = await invoke('fetch_stop_documents', {
      rankingOutput: ranking,
      miningOutput: mining,
      cfg,
    });
    log(`Gathered ${stubs.length} stop stubs`);
  } catch (err) {
    setTourState('error', { message: `Doc fetch failed: ${err}` });
    log(`Doc fetch failed: ${esc(String(err))}`, 'error');
    return;
  }

  // Step 3: narrate all stops in parallel; render each card as it resolves.
  setTourState('narrating', { stubs });
  log(`Narrating ${stubs.length} stops in parallel…`);

  const allSummaries = stubs.map(s => s.evidence_summary);

  const promises = stubs.map((stub, i) => {
    const others = allSummaries.filter((_, j) => j !== i);
    return invoke('narrate_stop', {
      stub,
      otherEvidenceSummaries: others,
      repoPath: mining.repo_path,
      cacheRoot,
      cfg,
    }).then(stop => {
      renderStopCard(stop, i);
      log(`Stop ${stop.sequence}: <em>${esc(stop.title)}</em>`, 'success');
      return { ok: true };
    }).catch(err => {
      renderStopError(stub, i, String(err));
      log(`Stop ${stub.sequence} narration failed: ${esc(String(err))}`, 'error');
      return { ok: false };
    });
  });

  const results  = await Promise.allSettled(promises);
  const anyFailed = results.some(r => r.status === 'fulfilled' && r.value?.ok === false);
  setTourState(anyFailed ? 'partial_failure' : 'ready');

  const ok = results.filter(r => r.status === 'fulfilled' && r.value?.ok).length;
  toast(`Tour ready — ${ok} of ${stubs.length} stops narrated`, anyFailed ? 'info' : 'success');
}

// ---------------------------------------------------------------------------
// Tour state machine
// ---------------------------------------------------------------------------

/**
 * Switch the tour section into a new state, showing/hiding sub-panels.
 *
 * States:
 *   idle            — waiting for user to click "Generate tour"
 *   ranking         — rank_significant_commits in flight
 *   fetching_docs   — fetch_stop_documents in flight
 *   narrating       — narrate_stop calls in flight; skeleton cards visible
 *   ready           — all stops narrated successfully
 *   partial_failure — some stops failed; partial banner shown
 *   error           — a blocking step failed; retry available
 *
 * @param {'idle'|'ranking'|'fetching_docs'|'narrating'|'ready'|'partial_failure'|'error'} state
 * @param {{ message?: string, stubs?: object[] }} [payload]
 */
function setTourState(state, payload = {}) {
  tourState = state;

  const panels = ['tour-idle', 'tour-ranking', 'tour-fetching', 'tour-error', 'tour-stop-list'];
  panels.forEach(id => { const el = $('#' + id); if (el) el.hidden = true; });
  $('#tour-partial-banner').hidden = true;

  switch (state) {
    case 'idle': {
      $('#tour-idle').hidden = false;
      const count = window.rankingOutput?.tour_candidates?.length ?? 0;
      $('#tour-candidate-count').textContent = count > 0 ? String(count) : '—';
      break;
    }
    case 'ranking':
      $('#tour-ranking').hidden = false;
      break;

    case 'fetching_docs':
      $('#tour-fetching').hidden = false;
      break;

    case 'narrating': {
      $('#tour-stop-list').hidden = false;
      if (payload.stubs) {
        tourStopCount = payload.stubs.length;
        const container = $('#tour-cards-container');
        container.innerHTML = '';
        payload.stubs.forEach((_, i) => container.appendChild(buildSkeletonCard(i)));
        currentStopIndex = 0;
        updateNav();
      }
      break;
    }

    case 'ready':
      $('#tour-stop-list').hidden = false;
      showStop(currentStopIndex);
      break;

    case 'partial_failure':
      $('#tour-stop-list').hidden = false;
      $('#tour-partial-banner').hidden = false;
      showStop(currentStopIndex);
      break;

    case 'error':
      $('#tour-error').hidden = false;
      if (payload.message) $('#tour-error-msg').textContent = payload.message;
      break;
  }
}

// ---------------------------------------------------------------------------
// Tour card builders
// ---------------------------------------------------------------------------

/** @param {number} i */
function buildSkeletonCard(i) {
  const el = document.createElement('article');
  el.className = 'tour-stop-skeleton';
  el.dataset.index = String(i);
  el.innerHTML = `
    <div class="sk-line sk-line--xs"></div>
    <div class="sk-line sk-line--title"></div>
    <div class="sk-line sk-line--full"></div>
    <div class="sk-line sk-line--full"></div>
    <div class="sk-line sk-line--mid"></div>
  `;
  return el;
}

/** @param {object} stop  TourStop @param {number} index */
function renderStopCard(stop, index) {
  const existing = $(`[data-index="${index}"]`);
  if (!existing) return;

  const date = stop.timestamp_utc
    ? new Date(stop.timestamp_utc).toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' })
    : '';

  const chips = (stop.triggered_by || [])
    .map(k => `<span class="trigger-chip trigger-${k}">${triggerLabel(k)}</span>`)
    .join('');

  const linkedBadge = stop.linked_document
    ? '<span class="linked-doc-badge"><i class="bi bi-link-45deg"></i> Issue context</span>' : '';

  const narration = (stop.narration || '')
    .split(/\n\n+/)
    .filter(p => p.trim())
    .map(p => `<p>${esc(p.trim())}</p>`)
    .join('');

  const filesHtml = (stop.files_changed || [])
    .map(f => `<li><code>${esc(f)}</code></li>`)
    .join('');

  const article = document.createElement('article');
  article.className = 'tour-stop-card';
  article.dataset.index = String(index);
  article.hidden = true;

  article.innerHTML = `
    <header class="stop-header">
      <span class="stop-sequence">Stop ${stop.sequence}</span>
      <span class="stop-subsystem-badge">${esc(stop.subsystem || '')}</span>
      <span class="stop-timestamp">${esc(date)}</span>
      ${chips}
      ${linkedBadge}
    </header>
    <h2 class="stop-title">${esc(stop.title || '')}</h2>
    <div class="stop-narration">${narration}</div>
    <details class="stop-rationale">
      <summary>
        <i class="bi bi-chevron-right rationale-chevron"></i>
        Why this stop was selected
      </summary>
      <p>${esc(stop.selection_rationale || '')}</p>
    </details>
    <div class="stop-files">
      <p class="stop-files-label">Files changed</p>
      <ul>${filesHtml}</ul>
    </div>
  `;

  existing.replaceWith(article);
  if (index === currentStopIndex) showStop(currentStopIndex);
}

/** @param {object} stub  StopStub @param {number} index @param {string} errMsg */
function renderStopError(stub, index, errMsg) {
  const existing = $(`[data-index="${index}"]`);
  if (!existing) return;

  const article = document.createElement('article');
  article.className = 'tour-stop-card tour-stop-error';
  article.dataset.index = String(index);
  article.hidden = true;

  article.innerHTML = `
    <header class="stop-header">
      <span class="stop-sequence">Stop ${stub.sequence}</span>
      <span class="stop-subsystem-badge">${esc(stub.subsystem || '')}</span>
    </header>
    <h2 class="stop-title">Narration unavailable — API error</h2>
    <p class="stop-error-detail">${esc(errMsg)}</p>
    <details class="stop-rationale">
      <summary>
        <i class="bi bi-chevron-right rationale-chevron"></i>
        Why this stop was selected
      </summary>
      <p>${esc(stub.selection_rationale || '')}</p>
    </details>
  `;

  existing.replaceWith(article);
  if (index === currentStopIndex) showStop(currentStopIndex);
}

// ---------------------------------------------------------------------------
// Tour navigation
// ---------------------------------------------------------------------------

function showStop(index) {
  const cards = document.querySelectorAll('#tour-cards-container [data-index]');
  if (!cards.length) return;
  index = Math.max(0, Math.min(index, cards.length - 1));
  currentStopIndex = index;
  cards.forEach(c => { c.hidden = true; });
  const target = $(`#tour-cards-container [data-index="${index}"]`);
  if (target) target.hidden = false;
  updateNav();
}

// Expose for commit_graph.js click handler
window.showTourStop = showStop;

function updateNav() {
  const total = tourStopCount || document.querySelectorAll('#tour-cards-container [data-index]').length;
  $('#tour-counter').textContent = `Stop ${currentStopIndex + 1} of ${total || '—'}`;
  $('#tour-prev-btn').disabled = currentStopIndex <= 0;
  $('#tour-next-btn').disabled = !total || currentStopIndex >= total - 1;
}

function triggerLabel(kind) {
  return { revert: 'Revert', was_reverted: 'Was reverted', repeated_fix: 'Repeated fix',
           incident_linked: 'Incident', architecture_shaping: 'Architecture' }[kind] || kind;
}

// ---------------------------------------------------------------------------
// tourOnRankingReady hook (extended by commit_graph.js & architecture_diagram.js)
// ---------------------------------------------------------------------------
window.tourOnRankingReady = function tourOnRankingReady() {
  const count = window.rankingOutput?.tour_candidates?.length ?? 0;
  if (count > 0) {
    const badge = $('#nav-tour-count');
    badge.textContent = count;
    badge.hidden = false;
    $('#nav-tour').disabled = false;
    // Update idle panel count if tour section is already open
    if (!$('#view-tour').hidden && tourState === 'idle') {
      setTourState('idle');
    }
  }
};

// ---------------------------------------------------------------------------
// Window controls (traffic lights)
// ---------------------------------------------------------------------------

/**
 * Wire the custom traffic-light buttons to real window actions and keep them
 * in sync with actual window state. Works identically on macOS and Linux —
 * both platforms run with native decorations off, so these three buttons are
 * the only way to close/minimize/maximize the app.
 */
function initWindowControls() {
  const appWindow    = getCurrentWindow();
  const closeBtn     = $('#wm-close');
  const minimizeBtn  = $('#wm-minimize');
  const maximizeBtn  = $('#wm-maximize');
  const topbar       = $('.topbar');

  closeBtn.addEventListener('click', () => appWindow.close());
  minimizeBtn.addEventListener('click', () => appWindow.minimize());
  maximizeBtn.addEventListener('click', () => appWindow.toggleMaximize());

  /** Reflect real maximized state in the zoom button's hover glyph/tooltip. */
  async function syncMaximizedState() {
    try {
      const isMaximized = await appWindow.isMaximized();
      maximizeBtn.classList.toggle('is-maximized', isMaximized);
      maximizeBtn.title = isMaximized ? 'Restore' : 'Maximize';
    } catch { /* window may be mid-teardown; ignore */ }
  }
  syncMaximizedState();
  appWindow.onResized(() => syncMaximizedState());

  // Double-clicking empty space in the title bar toggles maximize — matches
  // native title-bar behaviour on both macOS and every Linux desktop shell.
  topbar.addEventListener('dblclick', (e) => {
    if (e.target.closest('.wm-buttons, .topbar-actions, .brand')) return;
    appWindow.toggleMaximize();
  });

  // Dim the traffic lights when the window loses focus, like every native app.
  appWindow.onFocusChanged(({ payload: focused }) => {
    document.body.classList.toggle('wm-inactive', !focused);
  });
}

// ---------------------------------------------------------------------------
// Dark mode
// ---------------------------------------------------------------------------

function applyTheme(dark) {
  document.body.classList.toggle('dark', dark);
  const icon = $('#theme-icon');
  icon.className = dark ? 'bi bi-sun-fill' : 'bi bi-moon-stars-fill';
  try { localStorage.setItem('gl-dark', dark ? '1' : '0'); } catch {}
}

// ---------------------------------------------------------------------------
// Init & event wiring
// ---------------------------------------------------------------------------

window.addEventListener('DOMContentLoaded', () => {

  // ── Window controls ───────────────────────────────────────────────────────
  initWindowControls();

  // ── Theme ──────────────────────────────────────────────────────────────────
  const savedDark = (() => { try { return localStorage.getItem('gl-dark') === '1'; } catch { return false; } })();
  applyTheme(savedDark);
  $('#theme-toggle').addEventListener('click', () => applyTheme(!document.body.classList.contains('dark')));

  // ── Welcome / folder ───────────────────────────────────────────────────────
  $('#choose-folder-btn').addEventListener('click', chooseFolder);
  $('#change-folder-btn').addEventListener('click', chooseFolder);
  $('#sidebar-repo-btn').addEventListener('click', () => repoPath ? showView('workspace') : chooseFolder());
  // Topbar breadcrumb shows the currently open repo — clicking it re-opens
  // the folder picker so the user can switch repos from the title bar
  // without having to navigate back to the Overview page first. (Needs the
  // matching -webkit-app-region: no-drag override in styles.css, since the
  // rest of the topbar is a window-drag region.)
  $('#topbar-breadcrumb').addEventListener('click', chooseFolder);
  $('#topbar-breadcrumb').addEventListener('keydown', e => {
    if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); chooseFolder(); }
  });

  // ── Sidebar nav ────────────────────────────────────────────────────────────
  $('#nav-overview').addEventListener('click', () => {
    if (repoPath) showView('workspace');
    else chooseFolder();
  });

  $('#nav-files').addEventListener('click', () => {
    if (!repoPath) { chooseFolder(); return; }
    showView('workspace');
    setTimeout(() => $('#file-list')?.scrollIntoView({ behavior: 'smooth', block: 'nearest' }), 80);
  });

  $('#nav-tour').addEventListener('click', () => {
    if (!window.miningOutput) { toast('Run extraction first.', 'error'); return; }
    showView('tour');
    if (tourState === 'idle') setTourState('idle');
  });

  // History and Settings have no dedicated view yet — give explicit
  // feedback instead of leaving the click silently do nothing.
  $('#nav-history').addEventListener('click', () => {
    toast('History view is coming soon.', 'info');
  });
  $('#nav-settings').addEventListener('click', () => showView('settings'));

  // ── Overview actions ───────────────────────────────────────────────────────
  $('#git-pull-btn').addEventListener('click', async () => {
    if (!repoPath) { toast('No repository open.', 'error'); return; }
    // git_pull is not a registered command; show a user-friendly message
    toast('Use your terminal: git pull --ff-only', 'info');
    log('Tip: run <code>git pull --ff-only</code> in your terminal, then re-open the folder.');
  });

  $('#run-extraction-btn').addEventListener('click', () => {
    if (!repoPath) { chooseFolder().then(p => p && enterExtractionWizard(p)); return; }
    enterExtractionWizard(repoPath);
  });

  // ── File search ────────────────────────────────────────────────────────────
  $('#file-search').addEventListener('input', e => renderEntries(e.target.value));

  // ── Preview copy ──────────────────────────────────────────────────────────
  $('#preview-copy-btn').addEventListener('click', () => {
    const text = $('#file-preview').textContent;
    navigator.clipboard.writeText(text).then(() => toast('Copied to clipboard', 'success'));
  });

  // ── Clear log ─────────────────────────────────────────────────────────────
  $('#clear-log-btn').addEventListener('click', () => {
    $('#activity-log').innerHTML = '<p><i class="bi bi-circle-fill log-dot"></i> Log cleared.</p>';
  });

  // ── Extraction wizard ──────────────────────────────────────────────────────
  $('#extraction-back-btn').addEventListener('click', () => showView('workspace'));

  $('#mining-browse-btn').addEventListener('click', async () => {
    const p = await chooseFolder();
    if (p) $('#mining-repo-path').value = p;
  });

  $('#discover-btn').addEventListener('click', discoverSubsystems);

  $('#run-mining-btn').addEventListener('click', runExtraction);

  $('#proceed-to-tour-btn').addEventListener('click', () => {
    showView('tour');
    setTourState('idle');
  });

  $('#toggle-raw-output-btn').addEventListener('click', () => {
    const det = $('#raw-output-details');
    det.open = !det.open;
  });

  // ── Tour ───────────────────────────────────────────────────────────────────
  $('#tour-back-btn').addEventListener('click', () => showView('extraction'));

  $('#generate-tour-btn').addEventListener('click', generateTour);
  $('#tour-retry-btn').addEventListener('click', generateTour);

  $('#tour-prev-btn').addEventListener('click', () => showStop(currentStopIndex - 1));
  $('#tour-next-btn').addEventListener('click', () => showStop(currentStopIndex + 1));

  // Keyboard navigation when tour is visible
  document.addEventListener('keydown', e => {
    if ($('#view-tour').hidden) return;
    if (e.key === 'ArrowLeft')  showStop(currentStopIndex - 1);
    if (e.key === 'ArrowRight') showStop(currentStopIndex + 1);
  });

  // Tour tourNavigate event from commit_graph.js
  document.addEventListener('tourNavigate', e => {
    showStop(e.detail.rank - 1);
  });

  // ── Settings ───────────────────────────────────────────────────────────────
  // Show / hide the Base URL field depending on the chosen provider.
  $('#settings-provider-select').addEventListener('change', () => {
    const isOpenAi = $('#settings-provider-select').value === 'openai';
    $('#settings-base-url-field').hidden = !isOpenAi;
  });

  // Toggle key visibility.
  $('#settings-reveal-btn').addEventListener('click', () => {
    const input = $('#settings-api-key');
    const icon  = $('#settings-reveal-icon');
    const isHidden = input.type === 'password';
    input.type = isHidden ? 'text' : 'password';
    icon.className = isHidden ? 'bi bi-eye-slash' : 'bi bi-eye';
  });

  // Save & apply — invoke the backend set_api_key command.
  $('#settings-save-btn').addEventListener('click', async () => {
    const providerName = $('#settings-provider-select').value;
    const apiKey       = $('#settings-api-key').value.trim();
    const baseUrl      = $('#settings-base-url').value.trim() || null;
    const statusEl     = $('#settings-status');

    if (!apiKey) {
      statusEl.className = 'settings-status err';
      statusEl.innerHTML = '<i class="bi bi-x-circle"></i> API key is required.';
      statusEl.hidden = false;
      return;
    }

    statusEl.className = 'settings-status';
    statusEl.innerHTML = '<i class="bi bi-hourglass-split"></i> Saving…';
    statusEl.hidden = false;

    try {
      await invoke('set_api_key', { providerName, apiKey, baseUrl });
      statusEl.className = 'settings-status ok';
      statusEl.innerHTML = '<i class="bi bi-check-circle"></i> Provider updated successfully.';
      toast('API key saved — provider is active.', 'success');
    } catch (err) {
      statusEl.className = 'settings-status err';
      statusEl.innerHTML = `<i class="bi bi-x-circle"></i> ${esc(String(err))}`;
      toast('Failed to set API key.', 'error');
    }
  });
});