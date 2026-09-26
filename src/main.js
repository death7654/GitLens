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
  document.querySelector("#greet-form").addEventListener("submit", (e) => {
    e.preventDefault();
    greet();
  });

  document.querySelector("#mining-form").addEventListener("submit", (e) => {
    e.preventDefault();
    runExtraction();
  });

  document.querySelector("#discover-subsystems-btn").addEventListener("click", () => {
    discoverSubsystems();
  });
});