/**
 * main.js — GitLens frontend
 *
 * Wires the HTML shell to the Tauri IPC backend.
 * Real commands available:
 *   extract_git_history(config: MiningConfig)        → MiningOutput
 *   discover_repo_subsystems(repoPath: string)        → SubsystemDef[]
 *   rank_significant_commits(candidates, repoPath,
 *     cacheRoot, subsystems, cfg?)                    → RankingOutput
 *   fetch_stop_documents(rankingOutput, miningOutput, cfg) → StopStub[]
 *   narrate_stop(stub, otherEvidenceSummaries,
 *     repoPath, cacheRoot, cfg)                       → TourStop
 *
 * NOTE: inspect_folder / read_repo_file / git_pull are NOT available.
 * Folder browsing uses the Tauri dialog API; repo info is derived from
 * extract_git_history results.
 */

// Tauri IPC — available via window.__TAURI__ injected by the Tauri runtime.
const { invoke } = window.__TAURI__.core;
const { open }   = window.__TAURI__.dialog;

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
  const log = $('#activity-log');
  log.prepend(el);
  // keep at most 60 entries
  while (log.children.length > 60) log.lastChild.remove();
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
  ['view-welcome', 'view-workspace', 'view-extraction', 'view-tour'].forEach(id => {
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
 * Open the native folder picker and update repoPath + UI.
 * Returns the chosen path, or null if cancelled.
 * @returns {Promise<string|null>}
 */
async function chooseFolder() {
  try {
    const chosen = await open({ directory: true, multiple: false, title: 'Choose a repository folder' });
    if (typeof chosen !== 'string') return null;
    repoPath = chosen;
    applyRepoPath(repoPath);
    return repoPath;
  } catch (e) {
    toast(`Folder picker unavailable: ${e}`, 'error');
    return null;
  }
}

/**
 * Apply a repo path to all persistent UI slots without doing any I/O.
 * @param {string} path
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
// File explorer (backed by the MiningOutput file list as a fallback;
// we use discover_repo_subsystems just for subsystem discovery, not file listing)
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

/** @param {{path:string, name:string, size:number}} entry @param {HTMLElement} btn */
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
    pre.innerHTML = `<span class="log-error">Cannot read file: ${esc(String(err))}</span>`;
  }
}

// ---------------------------------------------------------------------------
// Extraction
// ---------------------------------------------------------------------------

/** @returns {Promise<void>} */
async function runExtraction() {
  const pathVal = $('#mining-repo-path').value.trim();
  if (!pathVal) { toast('Enter a repository path first.', 'error'); return; }

  let subsystems;
  const raw = $('#subsystems-input').value.trim();
  if (!raw) { toast('Define at least one subsystem (or auto-discover first).', 'error'); return; }
  try { subsystems = JSON.parse(raw); }
  catch (e) { toast(`Subsystems JSON is invalid: ${e.message}`, 'error'); return; }

  const maxAge    = parseInt($('#max-age-input').value, 10) || null;
  const maxCom    = parseInt($('#max-commits-input').value, 10) || null;

  const config = {
    repo_path: pathVal,
    subsystems,
    window: { max_age_days: maxAge, max_commits_per_subsystem: maxCom },
  };

  const btn    = $('#run-mining-btn');
  const status = $('#mining-status');
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
    status.textContent = `Done in ${elapsed}s — ${result.candidate_count} candidates found.`;

    log(`Extracted ${result.total_commits_scanned} commits from ${result.subsystems_scanned.length} subsystem(s), found <strong>${result.candidate_count}</strong> candidates`, 'success');
    toast(`${result.candidate_count} candidates found in ${elapsed}s`, 'success');

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
// Ranking
// ---------------------------------------------------------------------------

function getCacheRoot() {
  return (window.miningOutput?.repo_path || repoPath) + '/.ghost-cache';
}

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

/**
 * Run the full ranking + narration pipeline.
 * Called from the tour section's "Generate tour" button.
 */
async function generateTour() {
  if (!window.miningOutput) {
    toast('Run extraction first.', 'error'); return;
  }

  const mining   = window.miningOutput;
  const cfg      = buildTourConfig();
  const cacheRoot = getCacheRoot();

  // ── Step 1: rank ──────────────────────────────────────────────────────────
  setTourState('ranking');
  log('Ranking candidates…');

  const candidates = mining.commits.map(c => ({ commit: c, pr: {} }));
  const subsystems = mining.subsystems_scanned.map(name => ({
    name,
    path_prefixes: [],   // already filtered upstream; empty is safe
  }));

  let ranking;
  try {
    ranking = await invoke('rank_significant_commits', {
      candidates,
      repoPath: mining.repo_path,
      cacheRoot,
      subsystems,
      cfg: null,  // use RankingConfig::default()
    });
    window.rankingOutput = ranking;
    log(`Ranking complete — ${ranking.tour_candidates.length} tour stops selected`, 'success');
    window.tourOnRankingReady?.();
  } catch (err) {
    setTourState('error', { message: `Ranking failed: ${err}` });
    log(`Ranking failed: ${esc(String(err))}`, 'error');
    return;
  }

  // ── Step 2: fetch docs ────────────────────────────────────────────────────
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

  // ── Step 3: narrate (parallel) ────────────────────────────────────────────
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

  // ── Theme ──────────────────────────────────────────────────────────────────
  const savedDark = (() => { try { return localStorage.getItem('gl-dark') === '1'; } catch { return false; } })();
  applyTheme(savedDark);
  $('#theme-toggle').addEventListener('click', () => applyTheme(!document.body.classList.contains('dark')));

  // ── Welcome / folder ───────────────────────────────────────────────────────
  $('#choose-folder-btn').addEventListener('click', chooseFolder);
  $('#change-folder-btn').addEventListener('click', chooseFolder);
  $('#sidebar-repo-btn').addEventListener('click', () => repoPath ? showView('workspace') : chooseFolder());

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

  // ── Overview actions ───────────────────────────────────────────────────────
  $('#git-pull-btn').addEventListener('click', async () => {
    if (!repoPath) { toast('No repository open.', 'error'); return; }
    // git_pull is not a registered command; show a user-friendly message
    toast('Use your terminal: git pull --ff-only', 'info');
    log('Tip: run <code>git pull --ff-only</code> in your terminal, then re-open the folder.');
  });

  $('#run-extraction-btn').addEventListener('click', () => {
    if (!repoPath) { chooseFolder().then(p => p && showView('extraction')); return; }
    $('#mining-repo-path').value = repoPath;
    showView('extraction');
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
});
