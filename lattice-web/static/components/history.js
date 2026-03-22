// ============================================================================
// History — DAG graph of store intentions with decoded ops
//
// Data fetching + SVG/HTML rendering. Layout algorithm lives in dag.js.
// ============================================================================

import { html, Fragment, useState, useEffect, useRef, hex, fmtSExprPretty, extractIntentionHashes } from './util.js';
import * as Helpers from '../helpers.js';
import * as S from '../state.js';
import { layoutDag } from '../dag.js';
import { sdk } from '../sdk.js';

// Build SExpr-like objects for fmtSExprPretty.
function sexprSym(s)    { return { value: 'symbol', symbol: s }; }
function sexprList(items) { return { value: 'list', list: { items } }; }

// --- Data fetching -----------------------------------------------------------

// Build pubkey hex → display name map from peers + local node.
async function buildAuthorNames(storeId) {
  const names = new Map();
  try {
    const peers = (await sdk.api.store.ListPeers({ id: storeId })).peers || [];
    for (const p of peers) {
      if (p.name) names.set(hex(p.public_key), p.name);
    }
  } catch (e) { /* best effort */ }
  const ns = S.nodeStatus.value;
  if (ns?.public_key && ns.display_name) {
    const k = hex(ns.public_key);
    if (!names.has(k)) names.set(k, ns.display_name);
  }
  return names;
}

// Pure data fetch — returns { layout, authorNames } or null.
async function fetchHistoryLayout(storeId) {
  const [logResp, authorNames] = await Promise.all([
    sdk.api.store.WitnessLog({ store_id: storeId }),
    buildAuthorNames(storeId),
  ]);
  const entries = logResp.entries || [];
  if (entries.length === 0) return null;

  // Collect unique intention hashes from witness log
  const intentionHashes = extractIntentionHashes(entries);
  if (intentionHashes.length === 0) return null;

  // Fetch all intentions in parallel
  const results = await Promise.all(intentionHashes.map(async ({ bytes: hashBytes }) => {
    try {
      return await sdk.api.store.GetIntention({
        store_id: storeId,
        hash_prefix: hashBytes.slice(0, 4),
      });
    } catch (e) { return null; }
  }));

  // Build DAG entries keyed by hex hash
  const dagEntries = new Map();
  for (const resp of results) {
    if (!resp || !resp.intention) continue;
    const i = resp.intention;
    const h = hex(i.hash);

    const causalDeps = [];
    if (i.condition && i.condition.v1 && i.condition.v1.hashes) {
      for (const dep of i.condition.v1.hashes) causalDeps.push(hex(dep));
    }

    dagEntries.set(h, {
      hash: h,
      author: hex(i.author),
      hlc: i.timestamp
        ? (Number(i.timestamp.wall_time) * 65536) + (i.timestamp.counter || 0)
        : 0,
      causalDeps,
      isMerge: causalDeps.length > 1,
      intention: i,
      ops: resp.ops || [],
    });
  }

  if (dagEntries.size === 0) return null;
  return { layout: layoutDag(dagEntries), authorNames };
}

// Entry point for AsyncPanel — returns vnode.
export async function loadHistory(storeId) {
  const result = await fetchHistoryLayout(storeId);
  if (!result) return html`<div class="empty-state">No history</div>`;
  return html`<${HistoryGraph} layout=${result.layout} authorNames=${result.authorNames} />`;
}

// --- SVG + HTML rendering ----------------------------------------------------

const COL_W = 24;
const MIN_ROW_H = 28;
const NR = 4;
const NR_ROOT = 5;
const CURVE = 8;
const PAD_L = NR_ROOT + 4;

function historyLabel(entry, authorNames) {
  const name = authorNames.get(entry.author) || (entry.author.slice(0, 8) + '\u2026');
  const color = Helpers.colorFromHex(entry.author);
  const ops = entry.ops.length > 0
    ? html` ${fmtSExprPretty(sexprList([sexprSym('ops'), ...entry.ops]))}`
    : null;
  return html`<span class="history-author" style="--author-color:${color}"
    >${name}</span>${ops ? html` <span class="history-ops">${ops}</span>` : null}`;
}

function HistoryGraph({ layout, authorNames }) {
  const { rows, edges, maxCol, totalRows } = layout;
  const graphW = (maxCol + 1) * COL_W + PAD_L + NR + 2;
  const labelsRef = useRef(null);
  const [rowYs, setRowYs] = useState(null);

  // Measure label heights via ResizeObserver — re-measures on resize/wrap changes.
  useEffect(() => {
    const container = labelsRef.current;
    if (!container) return;

    const measure = () => {
      const children = container.children;
      if (children.length !== totalRows) return;
      const ys = new Float64Array(totalRows);
      let y = 0;
      for (let i = 0; i < totalRows; i++) {
        const h = children[i].offsetHeight;
        ys[i] = y + h / 2;
        y += h;
      }
      setRowYs(ys);
    };

    const observer = new ResizeObserver(measure);
    observer.observe(container);
    for (const child of container.children) observer.observe(child);

    measure();
    return () => observer.disconnect();
  }, [totalRows]);

  function nx(col) { return col * COL_W + PAD_L; }

  // Compute SVG once we have measured positions
  let svgH = totalRows * MIN_ROW_H;
  let svgContent = null;
  if (rowYs) {
    svgH = labelsRef.current ? labelsRef.current.offsetHeight : svgH;

    const edgePaths = edges.map(e => {
      const px = nx(e.parentCol), py = rowYs[e.parentRow];
      const cx = nx(e.childCol),  cy = rowYs[e.childRow];
      if (e.childCol === e.parentCol) return `M${px},${py}V${cy}`;
      if (e.isMerge) {
        const dir = cx < px ? -1 : 1;
        return `M${px},${py}V${cy - CURVE}Q${px},${cy} ${px + CURVE * dir},${cy}H${cx}`;
      }
      const dir = cx < px ? -1 : 1;
      return `M${px},${py}H${cx - CURVE * dir}Q${cx},${py} ${cx},${py + CURVE}V${cy}`;
    });

    svgContent = html`
      ${edgePaths.map(d => html`<path d=${d} fill="none" stroke="var(--border)" stroke-width="1.5" />`)}
      ${rows.map(r => {
        const x = nx(r.col), y = rowYs[r.row];
        const color = Helpers.colorFromHex(r.entry.author);
        if (r.isRoot) return html`<${Fragment}>
          <circle cx=${x} cy=${y} r=${NR_ROOT} fill=${color} />
          <circle cx=${x} cy=${y} r=${NR_ROOT + 2.5} fill="none" stroke=${color} stroke-width="1" />
        </${Fragment}>`;
        if (r.entry.isMerge) return html`<circle cx=${x} cy=${y} r=${NR} fill="var(--bg)" stroke=${color} stroke-width="2" />`;
        return html`<circle cx=${x} cy=${y} r=${NR} fill=${color} />`;
      })}
    `;
  }

  const ready = !!rowYs;

  return html`
    <div class="history-view">
      <div class="history-count muted">${totalRows} entries</div>
      <div class="history-container" style="--ready:${ready ? 1 : 0};--graph-w:${graphW}px">
        <svg class="history-graph" width=${graphW} height=${svgH}>
          ${svgContent}
        </svg>
        <div class="history-labels" ref=${labelsRef}>
          ${rows.map(r => html`
            <div class="history-label" key=${r.hash}
                 onClick=${() => S.setPanelOverride({
                   type: 'intention', intention: r.entry.intention,
                   ops: r.entry.ops, hexStr: r.entry.hash,
                 })}>
              ${historyLabel(r.entry, authorNames)}
            </div>
          `)}
        </div>
      </div>
    </div>
  `;
}
