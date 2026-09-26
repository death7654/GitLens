/**
 * commit_graph.js — Workstream E
 *
 * Renders a horizontal SVG timeline of all MiningOutput.commits.
 * Tour stops (those whose hash appears in rankingOutput.tour_candidates) are
 * drawn as coloured circles; background commits are small grey dots.
 * Clicking a highlighted circle dispatches a CustomEvent('tourNavigate')
 * or calls window.showTourStop if available.
 */

/** @param {string} str @returns {string} */
function esc(str) {
  return String(str)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

/** Priority-ordered trigger colour map */
const TRIGGER_COLORS = {
  revert:               '#7c5cd8',
  was_reverted:         '#e8922c',
  repeated_fix:         '#d97706',
  incident_linked:      '#dc2626',
  architecture_shaping: '#3b82d4',
};

const TRIGGER_PRIORITY = [
  'revert',
  'was_reverted',
  'repeated_fix',
  'incident_linked',
  'architecture_shaping',
];

/**
 * Derive the display colour for a RankedCandidate based on its signals array.
 * @param {string[]} signals
 * @returns {string}
 */
function colorForSignals(signals) {
  if (!signals || signals.length === 0) return '#3b82d4';
  for (const kind of TRIGGER_PRIORITY) {
    if (signals.includes(kind)) return TRIGGER_COLORS[kind];
  }
  return '#3b82d4';
}

/** Shared floating tooltip element, created once. */
let _tooltip = null;
function getTooltip() {
  if (!_tooltip) {
    _tooltip = document.createElement('div');
    _tooltip.className = 'vz-tooltip';
    _tooltip.style.display = 'none';
    document.body.appendChild(_tooltip);
  }
  return _tooltip;
}

/**
 * Render the commit graph SVG into #commit-graph-container.
 * Safe to call multiple times — clears and re-renders.
 */
window.renderCommitGraph = function renderCommitGraph() {
  const container = document.getElementById('commit-graph-container');
  if (!container) return;

  const mining  = window.miningOutput;
  const ranking = window.rankingOutput;
  if (!mining || !mining.commits || mining.commits.length === 0) return;

  // Build a lookup: hash → RankedCandidate (with rank = 1-based index)
  /** @type {Map<string, {candidate: object, rank: number}>} */
  const selectedMap = new Map();
  if (ranking && ranking.tour_candidates) {
    ranking.tour_candidates.forEach((c, i) => {
      selectedMap.set(c.commit_hash, { candidate: c, rank: i + 1 });
    });
  }

  // Sort commits by timestamp ascending (ISO strings sort lexicographically)
  const commits = [...mining.commits].sort((a, b) => {
    const ta = a.timestamp_utc || '';
    const tb = b.timestamp_utc || '';
    return ta < tb ? -1 : ta > tb ? 1 : 0;
  });

  const W = 800; // internal SVG coordinate width
  const H = 80;
  const CY = 40;       // vertical centre
  const PAD = 24;      // horizontal padding

  const minTs = new Date(commits[0].timestamp_utc || 0).getTime();
  const maxTs = new Date(commits[commits.length - 1].timestamp_utc || 0).getTime();
  const span  = maxTs - minTs || 1;

  /** Map a timestamp string to an X coordinate in SVG space */
  function tsToX(tsStr) {
    const t = new Date(tsStr || 0).getTime();
    return PAD + ((t - minTs) / span) * (W - PAD * 2);
  }

  // Build SVG elements as strings for performance
  const bgCircles   = [];
  const fgCircles   = [];
  const hitTargets  = []; // invisible wider hit areas for tiny dots

  commits.forEach((commit) => {
    const x = tsToX(commit.timestamp_utc);
    const sel = selectedMap.get(commit.commit_hash);

    if (sel) {
      const color  = colorForSignals(sel.candidate.signals || []);
      const r      = 7 + Math.min((commit.subsystems || []).length, 4);
      const hashShort = (commit.commit_hash || '').slice(0, 7);
      const summary   = esc(commit.message_summary || '');
      const rank      = sel.rank;

      fgCircles.push(
        `<circle class="vz-dot-selected" cx="${x}" cy="${CY}" r="${r}" fill="${esc(color)}" ` +
        `data-hash="${esc(hashShort)}" data-rank="${rank}" data-summary="${summary}" ` +
        `stroke="#fff" stroke-width="2" style="cursor:pointer" />`
      );
    } else {
      bgCircles.push(
        `<circle cx="${x}" cy="${CY}" r="3" fill="#9ca3af" opacity="0.4" />`
      );
    }
  });

  // Timeline base line
  const baseLine = `<line x1="${PAD}" y1="${CY}" x2="${W - PAD}" y2="${CY}" ` +
    `stroke="#e5e7eb" stroke-width="1.5" />`;

  const svgContent = baseLine + bgCircles.join('') + fgCircles.join('') + hitTargets.join('');

  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  svg.setAttribute('viewBox', `0 0 ${W} ${H}`);
  svg.setAttribute('preserveAspectRatio', 'xMidYMid meet');
  svg.style.width  = '100%';
  svg.style.height = `${H}px`;
  svg.style.display = 'block';
  svg.setAttribute('aria-label', 'Commit history timeline');
  svg.innerHTML = svgContent;

  // Attach tooltip + click behaviour via event delegation
  const tooltip = getTooltip();

  svg.addEventListener('mouseover', (e) => {
    const dot = e.target.closest('.vz-dot-selected');
    if (!dot) return;
    tooltip.textContent = `${dot.dataset.hash}  ${dot.dataset.summary}`;
    tooltip.style.display = 'block';
  });

  svg.addEventListener('mousemove', (e) => {
    tooltip.style.left = (e.clientX + 14) + 'px';
    tooltip.style.top  = (e.clientY - 28) + 'px';
  });

  svg.addEventListener('mouseout', (e) => {
    if (!e.target.closest('.vz-dot-selected')) {
      tooltip.style.display = 'none';
    }
  });

  svg.addEventListener('click', (e) => {
    const dot = e.target.closest('.vz-dot-selected');
    if (!dot) return;
    const rank = parseInt(dot.dataset.rank, 10);
    if (typeof window.showTourStop === 'function') {
      window.showTourStop(rank - 1);
    } else {
      document.dispatchEvent(new CustomEvent('tourNavigate', { detail: { rank } }));
    }
  });

  container.innerHTML = '';
  container.appendChild(svg);

  // Show the wrapping section
  const section = document.getElementById('vz-graph-section');
  if (section) section.hidden = false;
};

// ---------------------------------------------------------------------------
// Hook into tourOnRankingReady (set by main.js inside DOMContentLoaded).
// We extend it here inside our own DOMContentLoaded listener, which fires
// after main.js's listener because main.js's <script> tag appears first.
// ---------------------------------------------------------------------------

window.addEventListener('DOMContentLoaded', () => {
  const _orig = window.tourOnRankingReady;
  window.tourOnRankingReady = function tourOnRankingReadyViz() {
    if (_orig) _orig.apply(this, arguments);
    if (window.rankingOutput && window.miningOutput) {
      window.renderCommitGraph();
    }
  };

  // Also re-render if the function has already been called before we wrapped it
  // (e.g. ranking output already present when page loads with cached data).
  if (window.rankingOutput && window.miningOutput) {
    window.renderCommitGraph();
  }
});
