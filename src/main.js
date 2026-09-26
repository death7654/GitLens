const { invoke } = window.__TAURI__.core;

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