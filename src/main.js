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
