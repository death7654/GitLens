const { invoke } = window.__TAURI__.core;
const { open } = window.__TAURI__.dialog;
const $ = (selector) => document.querySelector(selector);
const toast = $('#toast');
let selectedRoot = '';

function notify(message) { toast.textContent = message; toast.classList.add('show'); window.setTimeout(() => toast.classList.remove('show'), 3600); }
function log(message, error = false) { const item = document.createElement('p'); item.innerHTML = `<i>${error ? '!' : '•'}</i> ${message}`; if (error) item.style.color = 'var(--orange)'; $('#activity-log').prepend(item); }
function bytes(size) { if (size < 1024) return `${size} B`; if (size < 1048576) return `${(size / 1024).toFixed(1)} KB`; return `${(size / 1048576).toFixed(1)} MB`; }

async function chooseFolder() {
  try {
    const path = await open({ directory: true, multiple: false, title: 'Choose a local repository folder' });
    if (typeof path === 'string') await loadFolder(path);
  } catch (error) { notify(`Folder picker unavailable: ${error}`); }
}

async function loadFolder(path) {
  try {
    const repo = await invoke('inspect_folder', { path });
    selectedRoot = repo.root;
    $('#welcome').hidden = true; $('#workspace').hidden = false;
    $('#repo-name').textContent = repo.name || 'Untitled folder'; $('#repo-path').textContent = repo.root;
    $('#topbar-repo').textContent = repo.name || 'Untitled folder';
    $('#sidebar-repo-name').textContent = repo.name || 'Untitled folder'; $('#sidebar-repo-path').textContent = repo.root;
    $('#repo-branch').textContent = repo.branch; $('#repo-clean').textContent = repo.clean ? 'clean' : 'changes present';
    $('#repo-clean').style.color = repo.clean ? 'var(--green)' : 'var(--orange)'; $('#repo-commit').textContent = repo.commit;
    $('#repo-count').textContent = repo.entries.length; $('#file-count').textContent = `${repo.entries.length} items`;
    $('#sidebar-count').textContent = repo.entries.length;
    renderEntries(repo.entries); log(`Opened ${repo.name} locally.`); window.scrollTo({ top: 0, behavior: 'smooth' });
  } catch (error) { notify(String(error)); log(String(error), true); }
}

function renderEntries(entries) {
  const list = $('#file-list'); list.innerHTML = '';
  entries.forEach((entry) => {
    const button = document.createElement('button'); button.className = `file-row ${entry.kind}`; button.title = entry.path;
    button.innerHTML = `<span class="file-symbol">${entry.kind === 'directory' ? '▸' : '·'}</span><span>${entry.path}</span>${entry.kind === 'file' ? `<small>${bytes(entry.size)}</small>` : ''}`;
    if (entry.kind === 'file') button.addEventListener('click', () => readFile(entry, button));
    else button.disabled = true;
    list.append(button);
  });
}

async function readFile(entry, button) {
  document.querySelectorAll('.file-row').forEach((row) => row.classList.remove('selected')); button.classList.add('selected');
  $('#preview-name').textContent = entry.path; $('#preview-size').textContent = bytes(entry.size); $('#preview-kind').textContent = entry.name.split('.').pop().toUpperCase() + ' FILE';
  try { $('#file-preview').textContent = await invoke('read_repo_file', { root: selectedRoot, relativePath: entry.path }); }
  catch (error) { $('#file-preview').textContent = String(error); }
}

async function pull() {
  if (!selectedRoot) return;
  $('#git-pull').disabled = true; $('#git-pull').innerHTML = '<span class="pull-symbol">…</span> Pulling'; log('Running git pull --ff-only…');
  try { const result = await invoke('git_pull', { path: selectedRoot }); log(result.output, !result.success); notify(result.success ? 'Git pull completed.' : 'Git pull needs attention.'); await loadFolder(selectedRoot); }
  catch (error) { log(String(error), true); notify(String(error)); }
  finally { $('#git-pull').disabled = false; $('#git-pull').innerHTML = '<span class="pull-symbol">↓</span> Git pull'; }
}

$('#choose-folder').addEventListener('click', chooseFolder); $('#change-folder').addEventListener('click', chooseFolder); $('#git-pull').addEventListener('click', pull);
$('#sidebar-repo').addEventListener('click', chooseFolder);
$('#sidebar-files').addEventListener('click', () => $('#workspace').hidden ? chooseFolder() : $('#file-list').scrollIntoView({ behavior: 'smooth', block: 'center' }));
$('#refresh-view').addEventListener('click', () => selectedRoot && loadFolder(selectedRoot));
$('#theme-toggle').addEventListener('click', () => document.body.classList.toggle('dim-mode'));

let greetInputEl;
let greetMsgEl;

async function greet() {
  // Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
  greetMsgEl.textContent = await invoke("greet", { name: greetInputEl.value });
}

// --- Git History Extraction test panel (Person 1) ---
async function runExtraction() {
  const statusEl = document.querySelector("#mining-status");
  const outputEl = document.querySelector("#mining-output");
  const repoPath = document.querySelector("#repo-path-input").value.trim();
  const subsystemsRaw = document.querySelector("#subsystems-input").value;
  const maxAgeDays = Number(document.querySelector("#max-age-input").value) || null;
  const maxCommits = Number(document.querySelector("#max-commits-input").value) || null;

  outputEl.textContent = "";

  if (!repoPath) {
    statusEl.textContent = "Enter a repo path first.";
    return;
  }

  let subsystems;
  try {
    subsystems = JSON.parse(subsystemsRaw);
  } catch (err) {
    statusEl.textContent = `Subsystems JSON is invalid: ${err.message}`;
    return;
  }

  const config = {
    repo_path: repoPath,
    subsystems,
    window: {
      max_age_days: maxAgeDays,
      max_commits_per_subsystem: maxCommits,
    },
  };

  statusEl.textContent = "Running extraction...";
  const started = performance.now();

  try {
    const result = await invoke("extract_git_history", { config });
    const elapsed = ((performance.now() - started) / 1000).toFixed(2);
    statusEl.textContent =
      `Scanned ${result.total_commits_scanned} raw commits across ` +
      `${result.subsystems_scanned.length} subsystem(s), found ` +
      `${result.candidate_count} candidate(s) in ${elapsed}s.`;
    outputEl.textContent = JSON.stringify(result, null, 2);
  } catch (err) {
    statusEl.textContent = "Extraction failed — see output below.";
    outputEl.textContent = typeof err === "string" ? err : JSON.stringify(err, null, 2);
  }
}

// --- Auto tree discovery ---
// Reads the repo's HEAD tree and turns its top-level directories into
// candidate subsystems, so the user doesn't have to guess path_prefixes
// against a repo they haven't opened in an editor. Only fills in the
// subsystems textarea (never touches repo path / age / commit-cap fields),
// and the result is still just a starting point the user can hand-edit
// before running extraction — discovery doesn't run extraction itself.
async function discoverSubsystems() {
  const statusEl = document.querySelector("#discover-status");
  const repoPath = document.querySelector("#repo-path-input").value.trim();

  if (!repoPath) {
    statusEl.textContent = "Enter a repo path first.";
    return;
  }

  statusEl.textContent = "Scanning repo tree...";

  try {
    const subsystems = await invoke("discover_repo_subsystems", { repoPath });
    document.querySelector("#subsystems-input").value = JSON.stringify(subsystems, null, 2);
    statusEl.textContent = subsystems.length
      ? `Found ${subsystems.length} candidate subsystem(s) — review the prefixes below before running extraction.`
      : "No subsystems found (empty repo, or everything at root was filtered out).";
  } catch (err) {
    statusEl.textContent =
      "Discovery failed: " + (typeof err === "string" ? err : JSON.stringify(err));
  }
}

window.addEventListener("DOMContentLoaded", () => {
  greetInputEl = document.querySelector("#greet-input");
  greetMsgEl = document.querySelector("#greet-msg");
  const greetForm = document.querySelector("#greet-form");
  if (greetForm) greetForm.addEventListener("submit", (e) => { e.preventDefault(); greet(); });

  const miningForm = document.querySelector("#mining-form");
  if (miningForm) miningForm.addEventListener("submit", (e) => { e.preventDefault(); runExtraction(); });

  const discoverBtn = document.querySelector("#discover-subsystems-btn");
  if (discoverBtn) discoverBtn.addEventListener("click", () => { discoverSubsystems(); });

  initTourUI();
});

// =============================================================================
// Tour UI — Onboarding Ghost
// =============================================================================

/** @type {'idle'|'fetching_docs'|'narrating'|'ready'|'partial_failure'|'error'} */
let tourState = 'idle';
let currentStopIndex = 0;
/** Total number of stops (stubs length), set when narrating begins. */
let tourStopCount = 0;

// Shared results from extraction / ranking steps (set by callers when available)
window.miningOutput = window.miningOutput || null;
window.rankingOutput = window.rankingOutput || null;

// ---------------------------------------------------------------------------
// State machine
// ---------------------------------------------------------------------------

/**
 * Transition the tour section into a new state, showing/hiding the appropriate
 * sub-panels.  `payload` carries state-specific data (e.g. error message).
 * @param {'idle'|'fetching_docs'|'narrating'|'ready'|'partial_failure'|'error'} state
 * @param {{ message?: string, stubs?: import('./types').StopStub[], anyFailed?: boolean }} [payload]
 */
function setTourState(state, payload = {}) {
  tourState = state;

  // Sub-panels
  const idlePanel    = $('#tour-idle');
  const fetchPanel   = $('#tour-fetching');
  const errorPanel   = $('#tour-error');
  const stopList     = $('#tour-stop-list');
  const partialBanner = $('#tour-partial-banner');

  // Hide all sub-panels first
  idlePanel.hidden    = true;
  fetchPanel.hidden   = true;
  errorPanel.hidden   = true;
  stopList.hidden     = true;
  partialBanner.hidden = true;

  switch (state) {
    case 'idle': {
      idlePanel.hidden = false;
      const candidateCount = window.rankingOutput?.tour_candidates?.length ?? 0;
      $('#tour-candidate-count').textContent = candidateCount > 0 ? String(candidateCount) : '—';
      break;
    }
    case 'fetching_docs':
      fetchPanel.hidden = false;
      break;

    case 'narrating': {
      stopList.hidden = false;
      // Build skeleton cards for all stubs
      if (payload.stubs) {
        tourStopCount = payload.stubs.length;
        const container = $('#tour-cards-container');
        container.innerHTML = '';
        payload.stubs.forEach((_stub, i) => {
          container.appendChild(buildSkeletonCard(i));
        });
        currentStopIndex = 0;
        updateNav();
      }
      break;
    }

    case 'ready':
      stopList.hidden = false;
      showStop(currentStopIndex);
      break;

    case 'partial_failure':
      stopList.hidden = false;
      partialBanner.hidden = false;
      showStop(currentStopIndex);
      break;

    case 'error':
      errorPanel.hidden = false;
      if (payload.message) $('#tour-error-msg').textContent = payload.message;
      break;
  }
}

// ---------------------------------------------------------------------------
// Trigger labels
// ---------------------------------------------------------------------------

/**
 * @param {string} kind
 * @returns {string}
 */
function triggerLabel(kind) {
  const labels = {
    revert:               'Revert',
    was_reverted:         'Was reverted',
    repeated_fix:         'Repeated fix',
    incident_linked:      'Incident',
    architecture_shaping: 'Architecture',
  };
  return labels[kind] || kind;
}

// ---------------------------------------------------------------------------
// Card builders
// ---------------------------------------------------------------------------

/**
 * Build an animated skeleton placeholder card for index `i`.
 * @param {number} i
 * @returns {HTMLElement}
 */
function buildSkeletonCard(i) {
  const article = document.createElement('article');
  article.className = 'tour-stop-skeleton';
  article.dataset.index = String(i);
  article.innerHTML = `
    <div class="skeleton-line skeleton-line--short"></div>
    <div class="skeleton-line skeleton-line--title"></div>
    <div class="skeleton-line"></div>
    <div class="skeleton-line skeleton-line--mid"></div>
  `;
  return article;
}

/**
 * Replace the skeleton at `index` with a fully rendered stop card.
 * @param {object} stop  TourStop
 * @param {number} index
 */
function renderStopCard(stop, index) {
  const existing = $(`[data-index="${index}"]`);
  if (!existing) return;

  const formattedDate = stop.timestamp_utc
    ? new Date(stop.timestamp_utc).toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' })
    : '';

  const triggerChips = (stop.triggered_by || [])
    .map(k => `<span class="trigger-chip trigger-${k}">${triggerLabel(k)}</span>`)
    .join('');

  const linkedBadge = stop.linked_document
    ? '<span class="linked-doc-badge">Issue context used</span>'
    : '';

  const narrationHtml = (stop.narration || '')
    .split(/\n\n+/)
    .filter(p => p.trim())
    .map(p => `<p>${escapeHtml(p.trim())}</p>`)
    .join('');

  const filesHtml = (stop.files_changed || [])
    .map(f => `<li><code>${escapeHtml(f)}</code></li>`)
    .join('');

  const article = document.createElement('article');
  article.className = 'tour-stop-card';
  article.dataset.index = String(index);
  article.hidden = true; // showStop controls visibility

  article.innerHTML = `
    <header class="stop-header">
      <span class="stop-sequence">Stop ${stop.sequence}</span>
      <span class="stop-subsystem-badge">${escapeHtml(stop.subsystem || '')}</span>
      <span class="stop-timestamp">${escapeHtml(formattedDate)}</span>
      ${triggerChips}
      ${linkedBadge}
    </header>
    <h2 class="stop-title">${escapeHtml(stop.title || '')}</h2>
    <div class="stop-narration">${narrationHtml}</div>
    <details class="stop-rationale">
      <summary>Why this stop was selected</summary>
      <p>${escapeHtml(stop.selection_rationale || '')}</p>
    </details>
    <div class="stop-files">
      <p class="stop-files-label">Files changed</p>
      <ul>${filesHtml}</ul>
    </div>
  `;

  existing.replaceWith(article);
  // If this is the current stop, make it visible immediately
  if (index === currentStopIndex) showStop(currentStopIndex);
}

/**
 * Replace the skeleton at `index` with a stub error card.
 * @param {object} stub  StopStub
 * @param {number} index
 * @param {string} errMsg
 */
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
      <span class="stop-subsystem-badge">${escapeHtml(stub.subsystem || '')}</span>
    </header>
    <h2 class="stop-title">Narration unavailable — API error</h2>
    <p class="stop-error-detail">${escapeHtml(errMsg)}</p>
    <details class="stop-rationale">
      <summary>Why this stop was selected</summary>
      <p>${escapeHtml(stub.selection_rationale || '')}</p>
    </details>
  `;

  existing.replaceWith(article);
  if (index === currentStopIndex) showStop(currentStopIndex);
}

// ---------------------------------------------------------------------------
// Navigation
// ---------------------------------------------------------------------------

/** Show the stop card at `index`, hide all others, update the counter. */
function showStop(index) {
  const cards = document.querySelectorAll('#tour-cards-container [data-index]');
  if (cards.length === 0) return;

  // Clamp index to valid range
  index = Math.max(0, Math.min(index, cards.length - 1));
  currentStopIndex = index;

  cards.forEach((card) => { card.hidden = true; });
  const target = $(`#tour-cards-container [data-index="${index}"]`);
  if (target) target.hidden = false;

  updateNav();
}

/** Refresh the prev/next buttons and counter text. */
function updateNav() {
  const total = tourStopCount || document.querySelectorAll('#tour-cards-container [data-index]').length;
  $('#tour-counter').textContent = `Stop ${currentStopIndex + 1} of ${total || '—'}`;
  $('#tour-prev-btn').disabled = currentStopIndex <= 0;
  $('#tour-next-btn').disabled = total === 0 || currentStopIndex >= total - 1;
}

// ---------------------------------------------------------------------------
// Config helpers
// ---------------------------------------------------------------------------

function buildTourConfig() {
  const ghToken   = $('#gh-token-input')?.value?.trim() || null;
  const repoSlug  = $('#repo-slug-input')?.value?.trim() || null;
  const jiraUrl   = $('#jira-url-input')?.value?.trim() || null;
  const jiraToken = $('#jira-token-input')?.value?.trim() || null;
  return {
    narration_prompt_version: 'v1',
    model_id: 'gemini-1.5-pro',
    temperature: 0.3,
    max_narration_tokens: 1024,
    fetch_linked_documents: !!(ghToken || jiraToken),
    github_token: ghToken,
    repo_slug: repoSlug,
    jira_base_url: jiraUrl,
    jira_token: jiraToken,
  };
}

function getCacheRoot() {
  return (window.miningOutput?.repo_path || '') + '/.ghost-cache';
}

// ---------------------------------------------------------------------------
// Core generation flow
// ---------------------------------------------------------------------------

async function generateTour() {
  const cfg = buildTourConfig();

  setTourState('fetching_docs');

  let stubs;
  try {
    stubs = await invoke('fetch_stop_documents', {
      rankingOutput: window.rankingOutput,
      miningOutput: window.miningOutput,
      cfg,
    });
  } catch (err) {
    setTourState('error', { message: String(err) });
    return;
  }

  // Transition to narrating — skeleton cards are rendered inside setTourState
  setTourState('narrating', { stubs });

  // Collect all evidence summaries for cross-stop context
  const allSummaries = stubs.map(s => s.evidence_summary);

  // Fire all narrate_stop calls in parallel; render each stop as it resolves
  const stopPromises = stubs.map((stub, i) => {
    const otherEvidenceSummaries = allSummaries.filter((_, j) => j !== i);
    return invoke('narrate_stop', {
      stub,
      otherEvidenceSummaries,
      repoPath: window.miningOutput?.repo_path || '',
      cacheRoot: getCacheRoot(),
      cfg,
    }).then(stop => {
      renderStopCard(stop, i);
      return { ok: true, stop };
    }).catch(err => {
      renderStopError(stub, i, String(err));
      return { ok: false, stub, err: String(err) };
    });
  });

  const results = await Promise.allSettled(stopPromises);
  const anyFailed = results.some(r => r.status === 'fulfilled' && r.value?.ok === false);
  setTourState(anyFailed ? 'partial_failure' : 'ready');
}

// ---------------------------------------------------------------------------
// Utility
// ---------------------------------------------------------------------------

/** Escape a string for safe insertion into innerHTML. */
function escapeHtml(str) {
  return String(str)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

// ---------------------------------------------------------------------------
// Wire up UI
// ---------------------------------------------------------------------------

/**
 * Called once from DOMContentLoaded.  Attaches all tour-related event
 * listeners and sets the initial state of the "View tour" button.
 */
function initTourUI() {
  // Back to overview
  $('#tour-back-btn').addEventListener('click', () => {
    $('#tour-section').hidden = true;
    $('#workspace').hidden = false;
  });

  // View tour button in workspace header
  $('#view-tour-btn').addEventListener('click', () => {
    $('#workspace').hidden = true;
    $('#tour-section').hidden = false;
    // Only reset to idle if we haven't started yet
    if (tourState === 'idle') setTourState('idle');
  });

  // Generate / retry buttons
  $('#generate-tour-btn').addEventListener('click', () => generateTour());
  $('#tour-retry-btn').addEventListener('click', () => generateTour());

  // Navigation buttons
  $('#tour-prev-btn').addEventListener('click', () => showStop(currentStopIndex - 1));
  $('#tour-next-btn').addEventListener('click', () => showStop(currentStopIndex + 1));

  // Keyboard navigation (only when tour section is visible)
  document.addEventListener('keydown', (e) => {
    if ($('#tour-section').hidden) return;
    if (e.key === 'ArrowLeft')  showStop(currentStopIndex - 1);
    if (e.key === 'ArrowRight') showStop(currentStopIndex + 1);
  });

  // Show "View tour" button whenever rankingOutput becomes available.
  // External code (extraction/ranking panels) should call tourOnRankingReady()
  // after setting window.rankingOutput.
  window.tourOnRankingReady = function tourOnRankingReady() {
    const count = window.rankingOutput?.tour_candidates?.length ?? 0;
    if (count > 0) {
      $('#view-tour-btn').hidden = false;
      // If tour section is already open and idle, refresh the candidate count
      if (!$('#tour-section').hidden && tourState === 'idle') {
        setTourState('idle');
      }
    }
  };
}