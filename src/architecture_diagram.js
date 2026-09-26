/**
 * architecture_diagram.js — Workstream E
 *
 * Derives a subsystem co-occurrence map from MiningOutput.commits and renders
 * it as a Mermaid graph LR diagram — showing which subsystems tend to change
 * together across the repo's history.
 */

/** @param {string} str @returns {string} */
function escArch(str) {
  return String(str)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

/**
 * Sanitise a subsystem name into a safe Mermaid node identifier.
 * Mermaid node IDs must not contain spaces or special chars.
 * @param {string} name
 * @returns {string}
 */
function toNodeId(name) {
  return String(name).replace(/[^a-zA-Z0-9_]/g, '_').replace(/^_+|_+$/g, '') || 'unknown';
}

/**
 * Inject the Mermaid CDN script if not already loaded, then call `cb`.
 * Idempotent: if window.mermaid exists the callback fires synchronously.
 * @param {() => void} cb
 */
function ensureMermaid(cb) {
  if (window.mermaid) {
    cb();
    return;
  }
  const script = document.createElement('script');
  script.src = 'https://cdn.jsdelivr.net/npm/mermaid@11/dist/mermaid.min.js';
  script.onload = () => {
    window.mermaid.initialize({ startOnLoad: false, theme: 'neutral' });
    cb();
  };
  script.onerror = () => {
    console.warn('[arch-diagram] Failed to load Mermaid from CDN.');
  };
  document.head.appendChild(script);
}

/**
 * Build the co-occurrence map from commits.
 * @param {object[]} commits  CommitRecord[]
 * @returns {Map<string, Map<string, number>>}
 */
function buildCoOccurrenceMap(commits) {
  /** @type {Map<string, Map<string, number>>} */
  const map = new Map();

  for (const commit of commits) {
    const subs = commit.subsystems;
    if (!subs || subs.length < 2) continue;

    // Record every unique pair (A, B) where A < B lexicographically
    for (let i = 0; i < subs.length; i++) {
      for (let j = i + 1; j < subs.length; j++) {
        const a = subs[i] < subs[j] ? subs[i] : subs[j];
        const b = subs[i] < subs[j] ? subs[j] : subs[i];

        if (!map.has(a)) map.set(a, new Map());
        const inner = map.get(a);
        inner.set(b, (inner.get(b) || 0) + 1);
      }
    }
  }

  return map;
}

/**
 * Derive a set of all subsystem names from commits.
 * @param {object[]} commits
 * @returns {Set<string>}
 */
function allSubsystems(commits) {
  const set = new Set();
  for (const commit of commits) {
    if (commit.subsystems) commit.subsystems.forEach(s => set.add(s));
  }
  return set;
}

/**
 * Build a Mermaid graph LR definition string.
 * @param {Map<string, Map<string, number>>} coMap
 * @param {Set<string>} subsystems
 * @param {number} threshold  minimum co-occurrence count to show an edge
 * @returns {string}
 */
function buildMermaidDef(coMap, subsystems, threshold = 2) {
  const lines = ['graph LR'];

  // Declare all nodes so isolated subsystems still appear
  for (const name of subsystems) {
    const id    = toNodeId(name);
    const label = escArch(name).replace(/"/g, '#quot;');
    lines.push(`  ${id}["${label}"]`);
  }

  // Edges above threshold
  for (const [a, innerMap] of coMap) {
    for (const [b, count] of innerMap) {
      if (count < threshold) continue;
      const idA = toNodeId(a);
      const idB = toNodeId(b);
      lines.push(`  ${idA} -- "${count} commits" --> ${idB}`);
    }
  }

  return lines.join('\n');
}

/**
 * Render the architecture diagram into #arch-diagram-container.
 * Safe to call multiple times — clears and re-renders.
 */
window.renderArchDiagram = function renderArchDiagram() {
  const container = document.getElementById('arch-diagram-container');
  if (!container) return;

  const mining = window.miningOutput;
  if (!mining || !mining.commits || mining.commits.length === 0) return;

  const commits    = mining.commits;
  const coMap      = buildCoOccurrenceMap(commits);
  const subsystems = allSubsystems(commits);

  if (subsystems.size === 0) return;

  const diagramDef = buildMermaidDef(coMap, subsystems, 2);

  ensureMermaid(async () => {
    try {
      // mermaid.render returns a promise in v10+/v11
      const { svg } = await window.mermaid.render('arch-diagram-svg', diagramDef);

      // Label above the diagram
      const label = document.createElement('p');
      label.className = 'vz-diagram-label';
      label.textContent = 'Subsystem co-occurrence — commits touching multiple areas';

      container.innerHTML = '';
      container.appendChild(label);

      const wrapper = document.createElement('div');
      wrapper.className = 'vz-diagram-inner';
      // svg is a string of SVG markup
      wrapper.innerHTML = svg;
      container.appendChild(wrapper);

      // Show the wrapping section
      const section = document.getElementById('vz-arch-section');
      if (section) section.hidden = false;
    } catch (err) {
      console.warn('[arch-diagram] Mermaid render failed:', err);
      container.innerHTML =
        `<p class="vz-diagram-label">Subsystem map unavailable: ${escArch(String(err))}</p>`;
    }
  });
};

// ---------------------------------------------------------------------------
// Hook into tourOnRankingReady — same pattern as commit_graph.js.
// ---------------------------------------------------------------------------

window.addEventListener('DOMContentLoaded', () => {
  const _orig = window.tourOnRankingReady;
  window.tourOnRankingReady = function tourOnRankingReadyArch() {
    if (_orig) _orig.apply(this, arguments);
    if (window.rankingOutput && window.miningOutput) {
      window.renderArchDiagram();
    }
  };

  if (window.rankingOutput && window.miningOutput) {
    window.renderArchDiagram();
  }
});
