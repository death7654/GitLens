/**
 * tour_export.js — Workstream E
 *
 * "Export tour" button logic: serialises all rendered tour-stop cards into a
 * self-contained offline HTML file and triggers a browser download via a
 * Blob + object URL — no server round-trip required.
 */

/** @param {string} str @returns {string} */
function escExport(str) {
  return String(str)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

/**
 * Extract a plain-text representation of a tour stop card element.
 * We read the rendered DOM rather than raw JS objects so the export
 * faithfully reflects what the user actually sees (including error stubs).
 *
 * @param {Element} card  .tour-stop-card element
 * @param {number}  idx   0-based index
 * @returns {string}  HTML fragment for the stop
 */
function cardToHtml(card, idx) {
  if (card.classList.contains('tour-stop-error')) {
    const seq      = card.querySelector('.stop-sequence')?.textContent || `Stop ${idx + 1}`;
    const subsys   = card.querySelector('.stop-subsystem-badge')?.textContent || '';
    const errMsg   = card.querySelector('.stop-error-detail')?.textContent || 'Narration unavailable';
    const rationale = card.querySelector('.stop-rationale p')?.textContent || '';
    return `
    <section class="stop error-stop">
      <header>
        <span class="seq">${escExport(seq)}</span>
        ${subsys ? `<span class="badge">${escExport(subsys)}</span>` : ''}
      </header>
      <h2>Narration unavailable — API error</h2>
      <p class="err">${escExport(errMsg)}</p>
      ${rationale ? `<details><summary>Why this stop was selected</summary><p>${escExport(rationale)}</p></details>` : ''}
    </section>`;
  }

  const seq        = card.querySelector('.stop-sequence')?.textContent || `Stop ${idx + 1}`;
  const subsys     = card.querySelector('.stop-subsystem-badge')?.textContent || '';
  const timestamp  = card.querySelector('.stop-timestamp')?.textContent || '';
  const chips      = [...card.querySelectorAll('.trigger-chip')].map(c => `<span class="chip">${escExport(c.textContent)}</span>`).join(' ');
  const linkedBadge = card.querySelector('.linked-doc-badge') ? '<span class="chip chip-linked">Issue context used</span>' : '';
  const title      = card.querySelector('.stop-title')?.textContent || '';
  const narrationParas = [...card.querySelectorAll('.stop-narration p')]
    .map(p => `<p>${escExport(p.textContent)}</p>`)
    .join('\n      ');
  const rationale  = card.querySelector('.stop-rationale p')?.textContent || '';
  const files      = [...card.querySelectorAll('.stop-files li code')]
    .map(c => `<li><code>${escExport(c.textContent)}</code></li>`)
    .join('\n          ');

  return `
    <section class="stop">
      <header>
        <span class="seq">${escExport(seq)}</span>
        ${subsys ? `<span class="badge">${escExport(subsys)}</span>` : ''}
        ${timestamp ? `<span class="ts">${escExport(timestamp)}</span>` : ''}
        ${chips}
        ${linkedBadge}
      </header>
      <h2>${escExport(title)}</h2>
      <div class="narration">
        ${narrationParas}
      </div>
      ${rationale ? `
      <details>
        <summary>Why this stop was selected</summary>
        <p>${escExport(rationale)}</p>
      </details>` : ''}
      ${files ? `
      <div class="files">
        <p class="files-label">Files changed</p>
        <ul>${files}</ul>
      </div>` : ''}
    </section>`;
}

/**
 * Build the complete standalone HTML document string for the exported tour.
 * @param {Element[]} cards
 * @returns {string}
 */
function buildExportHtml(cards) {
  const repoPath  = window.miningOutput?.repo_path || 'repository';
  // Use last path segment as the repo name for the title
  const repoName  = repoPath.split(/[/\\]/).filter(Boolean).pop() || repoPath;
  const generated = new Date().toLocaleString(undefined, {
    year: 'numeric', month: 'short', day: 'numeric',
    hour: '2-digit', minute: '2-digit',
  });

  const stopsHtml = cards.map(cardToHtml).join('\n');

  return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1.0" />
  <title>Onboarding Tour — ${escExport(repoName)}</title>
  <style>
    *, *::before, *::after { box-sizing: border-box; }
    body {
      margin: 0;
      font-family: -apple-system, "Segoe UI", system-ui, sans-serif;
      font-size: 15px;
      line-height: 1.6;
      color: #1f2328;
      background: #ffffff;
    }
    .page {
      max-width: 760px;
      margin: 0 auto;
      padding: 48px 24px 80px;
    }
    .page-header {
      margin-bottom: 40px;
      border-bottom: 1px solid #e5e7eb;
      padding-bottom: 24px;
    }
    .eyebrow {
      font-size: 11px;
      font-family: "SFMono-Regular", Consolas, "Liberation Mono", monospace;
      text-transform: uppercase;
      letter-spacing: .1em;
      color: #57606a;
      margin: 0 0 8px;
    }
    h1 {
      font-size: 26px;
      font-weight: 700;
      letter-spacing: -.03em;
      margin: 0 0 8px;
      color: #1f2328;
    }
    .meta {
      font-size: 12px;
      color: #57606a;
      font-family: "SFMono-Regular", Consolas, "Liberation Mono", monospace;
    }
    .stop {
      background: #f7f8fa;
      border: 1px solid #e5e7eb;
      border-radius: 9px;
      padding: 26px 28px;
      margin-bottom: 28px;
    }
    .stop.error-stop {
      border-left: 4px solid #f26b3a;
    }
    .stop header {
      display: flex;
      flex-wrap: wrap;
      align-items: center;
      gap: 8px;
      margin-bottom: 12px;
    }
    .seq {
      font-size: 10px;
      font-family: "SFMono-Regular", Consolas, "Liberation Mono", monospace;
      text-transform: uppercase;
      letter-spacing: .1em;
      color: #57606a;
    }
    .badge {
      background: #eef2fb;
      color: #1d4ed8;
      border-radius: 4px;
      padding: 2px 8px;
      font-size: 11px;
      font-family: "SFMono-Regular", Consolas, "Liberation Mono", monospace;
      font-weight: 650;
    }
    .ts {
      color: #57606a;
      font-size: 11px;
      font-family: "SFMono-Regular", Consolas, "Liberation Mono", monospace;
    }
    .chip {
      background: #f0eaff;
      color: #6b2fad;
      border-radius: 4px;
      padding: 2px 8px;
      font-size: 11px;
      font-family: "SFMono-Regular", Consolas, "Liberation Mono", monospace;
      font-weight: 650;
    }
    .chip-linked {
      background: #e5f7ee;
      color: #176b3d;
    }
    h2 {
      font-size: 20px;
      font-weight: 700;
      letter-spacing: -.03em;
      margin: 0 0 16px;
      color: #1f2328;
    }
    .narration {
      color: #3d4d61;
      line-height: 1.75;
      margin-bottom: 18px;
    }
    .narration p { margin: 0 0 14px; }
    .narration p:last-child { margin-bottom: 0; }
    .err {
      font-family: "SFMono-Regular", Consolas, "Liberation Mono", monospace;
      font-size: 12px;
      color: #f26b3a;
      margin: 10px 0 18px;
    }
    details {
      border-top: 1px solid #e5e7eb;
      padding-top: 12px;
      margin-top: 4px;
      margin-bottom: 16px;
    }
    summary {
      font-size: 11px;
      font-family: "SFMono-Regular", Consolas, "Liberation Mono", monospace;
      text-transform: uppercase;
      letter-spacing: .07em;
      color: #57606a;
      cursor: pointer;
      user-select: none;
    }
    details p {
      margin: 8px 0 0;
      font-size: 13px;
      color: #57606a;
      line-height: 1.65;
    }
    .files {
      border-top: 1px solid #e5e7eb;
      padding-top: 12px;
    }
    .files-label {
      font-size: 10px;
      font-family: "SFMono-Regular", Consolas, "Liberation Mono", monospace;
      text-transform: uppercase;
      letter-spacing: .07em;
      color: #57606a;
      margin: 0 0 8px;
    }
    .files ul {
      margin: 0;
      padding: 0;
      list-style: none;
      display: flex;
      flex-wrap: wrap;
      gap: 6px;
    }
    .files li code {
      font-size: 11px;
      font-family: "SFMono-Regular", Consolas, "Liberation Mono", monospace;
      color: #596678;
      background: #ffffff;
      border: 1px solid #e5e7eb;
      border-radius: 4px;
      padding: 2px 7px;
    }
    footer.made-with {
      margin-top: 48px;
      padding-top: 16px;
      border-top: 1px solid #e5e7eb;
      text-align: center;
      font-size: 12px;
      color: #57606a;
    }
  </style>
</head>
<body>
  <div class="page">
    <div class="page-header">
      <p class="eyebrow">Onboarding tour</p>
      <h1>Repository story — ${escExport(repoName)}</h1>
      <p class="meta">Generated ${escExport(generated)} · ${cards.length} stop${cards.length !== 1 ? 's' : ''}</p>
    </div>
    ${stopsHtml}
    <footer class="made-with">Made with IBM Bob · The Onboarding Ghost</footer>
  </div>
</body>
</html>`;
}

/**
 * Export the current tour to a self-contained HTML file and trigger a download.
 * Skips if no stop cards are rendered yet.
 */
window.exportTour = function exportTour() {
  const cards = [...document.querySelectorAll('#tour-cards-container .tour-stop-card')];
  if (cards.length === 0) {
    console.warn('[tour-export] No rendered stop cards to export.');
    return;
  }

  const html = buildExportHtml(cards);
  const blob = new Blob([html], { type: 'text/html' });
  const url  = URL.createObjectURL(blob);

  const a = document.createElement('a');
  a.href     = url;
  a.download = 'onboarding-tour.html';
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  URL.revokeObjectURL(url);
};

// ---------------------------------------------------------------------------
// Wire up the export button and the stop-card observer
// ---------------------------------------------------------------------------

window.addEventListener('DOMContentLoaded', () => {
  const exportBtn = document.getElementById('tour-export-btn');
  if (exportBtn) {
    exportBtn.addEventListener('click', () => window.exportTour());
  }

  // Show/hide the export row when stop cards appear or disappear.
  // We observe #tour-cards-container for child mutations.
  const cardsContainer = document.getElementById('tour-cards-container');
  const exportRow      = document.getElementById('vz-export-row');

  if (cardsContainer && exportRow) {
    const observer = new MutationObserver(() => {
      const hasCards = cardsContainer.querySelectorAll('.tour-stop-card').length > 0;
      exportRow.hidden = !hasCards;
    });
    observer.observe(cardsContainer, { childList: true, subtree: false });
  }
});
