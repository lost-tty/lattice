import { html, hex } from './util.js';
import * as Helpers from '../helpers.js';

// Bucket intentions by membership set (which hulls contain them). Relies on each
// author's seqs being contiguous 1..total so each hull's `seen` count also acts
// as its frontier threshold — walking thresholds in ascending order partitions
// the chain into segments sharing a membership pattern.
export function computeUpset(observations, observers, totalsList) {
  const hulls = observers
    .map(o => ({ key: hex(o.public_key) }))
    .sort((a, b) => a.key.localeCompare(b.key));
  const hullIndex = new Map(hulls.map((h, i) => [h.key, i]));

  const seenBy = new Map();
  for (const o of observations) {
    seenBy.set(`${hex(o.observer)}|${hex(o.observed_author)}`, Number(o.seen));
  }

  const buckets = new Map();
  const addBucket = (members, count) => {
    if (count <= 0) return;
    const key = members.join(',');
    if (!buckets.has(key)) buckets.set(key, { members, count: 0 });
    buckets.get(key).count += count;
  };

  let totalIntentions = 0;
  for (const t of totalsList) {
    const author = hex(t.author);
    const total = Number(t.count);
    totalIntentions += total;
    if (total === 0) continue;

    const thresholds = hulls.map(h => ({
      idx: hullIndex.get(h.key),
      seen: Math.min(total, seenBy.get(`${h.key}|${author}`) ?? 0),
    }));
    thresholds.sort((a, b) => a.seen - b.seen);

    let activeIdxs = thresholds.map(t => t.idx);
    let cursor = 0;
    for (const t of thresholds) {
      const segLen = t.seen - cursor;
      if (segLen > 0) {
        addBucket([...activeIdxs].sort((a, b) => a - b), segLen);
      }
      activeIdxs = activeIdxs.filter(i => i !== t.idx);
      cursor = t.seen;
    }
    if (total > cursor) addBucket([], total - cursor);
  }

  const list = Array.from(buckets.values()).sort((a, b) => b.count - a.count);
  return { hulls, buckets: list, totalIntentions };
}

export function UpsetPlot({ hulls, buckets, nameFor }) {
  const totalMax = buckets[0]?.count || 1;
  const dotSize = 10;
  const colWidth = 28;
  const rowHeight = 18;
  const barMaxHeight = 100;

  const label = (h) => nameFor.get(h.key) || h.key.slice(0, 8);

  const gridWidth = buckets.length * colWidth;
  const gridHeight = hulls.length * rowHeight;

  return html`
    <div class="upset">
      <div class="upset-bars" style=${`width: ${gridWidth}px; height: ${barMaxHeight + 20}px;`}>
        ${buckets.map(bucket => {
          const barH = Math.max(2, Math.round((bucket.count / totalMax) * barMaxHeight));
          return html`
            <div class="upset-bar-col" style=${`width: ${colWidth}px;`}>
              <span class="upset-count">${bucket.count}</span>
              <div class="upset-bar" style=${`height: ${barH}px;`}></div>
            </div>
          `;
        })}
      </div>
      <div class="upset-grid">
        <svg class="upset-matrix" width=${gridWidth} height=${gridHeight} viewBox=${`0 0 ${gridWidth} ${gridHeight}`} aria-hidden="true">
          ${buckets.map((bucket, bi) => {
            const memberSet = new Set(bucket.members);
            const cx = bi * colWidth + colWidth / 2;
            const idxs = Array.from(memberSet).sort((a, b) => a - b);
            return html`
              ${idxs.length > 1 ? html`
                <line
                  x1=${cx} x2=${cx}
                  y1=${idxs[0] * rowHeight + rowHeight / 2}
                  y2=${idxs[idxs.length - 1] * rowHeight + rowHeight / 2}
                  stroke="currentColor" stroke-opacity="0.4" stroke-width="1.5" />
              ` : null}
              ${hulls.map((h, hi) => {
                const cy = hi * rowHeight + rowHeight / 2;
                const active = memberSet.has(hi);
                const color = Helpers.colorFromHex(h.key);
                return html`<circle cx=${cx} cy=${cy} r=${dotSize / 2}
                  fill=${active ? color : 'transparent'}
                  stroke=${active ? color : 'currentColor'}
                  stroke-opacity=${active ? 1 : 0.25}
                  stroke-width="1" />`;
              })}
            `;
          })}
        </svg>
        <div class="upset-labels">
          ${hulls.map(h => html`
            <div class="upset-author-row" style=${`height: ${rowHeight}px; color: ${Helpers.colorFromHex(h.key)};`}>
              ${label(h)}
            </div>
          `)}
        </div>
      </div>
    </div>
  `;
}
