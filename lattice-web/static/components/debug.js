import { html, hex, fmtTime, fmtOps, extractIntentionHashes } from './util.js';
import * as Helpers from '../helpers.js';
import * as S from '../state.js';
import * as Router from '../router.js';
import { sdk } from '../sdk.js';

export async function loadDebug(storeId, debugView) {
  switch (debugView) {
    case 'tips': return await loadDebugTips(storeId);
    case 'authorstate': return await loadDebugAuthorState(storeId);
    case 'log': return await loadDebugLog(storeId);
    case 'intentions': return await loadDebugIntentions(storeId);
    case 'floating': return await loadDebugFloating(storeId);
  }
}

function DebugNav({ storeId, debugView }) {
  const uuid = Helpers.uuidFromBytes(storeId);
  const views = ['tips', 'authorstate', 'log', 'intentions', 'floating'];
  return html`
    <div class="action-bar">
      ${views.map(v => html`
        <button
          class="btn${v === debugView ? ' btn-primary' : ''}"
          onClick=${() => Router.navigate('/store/' + uuid + '/debug' + (v === 'tips' ? '' : '/' + v))}
        >${v}</button>
      `)}
      <button class="btn" onClick=${() => S.showModal('inspectIntention')}>inspect</button>
    </div>
  `;
}

async function loadDebugTips(storeId) {
  const authors = (await sdk.api.store.GetAuthorTips({ store_id: storeId })).items || [];
  return html`
    <${DebugNav} storeId=${storeId} debugView="tips" />
    ${authors.length === 0
      ? html`<div class="empty-state">No author state</div>`
      : html`
        <table>
          <tr><th>Name</th><th>Author</th><th>Tip</th><th>Seq</th><th>Status</th></tr>
          ${authors.map(a => html`
            <tr>
              <td>${a.peer_name || '-'}</td>
              <td class="mono">${Helpers.pubkeyShort(a.public_key)}</td>
              <td class="mono">${hex(a.hash) || '-'}</td>
              <td>${a.witness_seq ?? '-'}</td>
              <td>${a.peer_status ?? '-'}</td>
            </tr>
          `)}
        </table>
      `
    }
  `;
}

async function loadDebugAuthorState(storeId) {
  const [resp, ackResp] = await Promise.all([
    sdk.api.store.GetAuthorStateObservations({ store_id: storeId }),
    sdk.api.store.GetAckDelta({ store_id: storeId }),
  ]);
  const observers = resp.observers || [];
  const columns = resp.columns || [];
  const observations = resp.items || [];
  const totalsList = resp.author_totals || [];
  const ackEntries = ackResp.entries || [];

  if (observers.length === 0) {
    return html`<${DebugNav} storeId=${storeId} debugView="authorstate" /><div class="empty-state">No authors</div>`;
  }

  const totals = new Map();
  for (const t of totalsList) totals.set(hex(t.author), t.count);

  const cell = new Map();
  for (const o of observations) {
    cell.set(`${hex(o.observer)}|${hex(o.observed_author)}`, o.seen);
  }

  const shortHex = (u8) => Array.from(u8.slice(0, 4))
    .map(b => b.toString(16).padStart(2, '0'))
    .join('');
  const labelFor = (info) => info.peer_name && info.peer_name.length > 0
    ? info.peer_name
    : '-';
  const colIds = columns.map(c => hex(c.public_key));

  return html`
    <${DebugNav} storeId=${storeId} debugView="authorstate" />
    <table>
      <tr>
        <th>Observer</th><th>Status</th>
        ${columns.map(c => html`
          <th>
            <div>${labelFor(c)}</div>
            <div class="mono muted">${shortHex(c.public_key)}</div>
          </th>
        `)}
      </tr>
      ${observers.map(obs => {
        const observerHex = hex(obs.public_key);
        return html`
          <tr>
            <td>${labelFor(obs)}</td>
            <td>${obs.peer_status ?? '-'}</td>
            ${colIds.map(observedHex => {
              if (observedHex === observerHex) {
                const selfTotal = totals.get(observerHex);
                return selfTotal != null
                  ? html`<td><span class="ok">${selfTotal}</span></td>`
                  : html`<td class="muted">n/a</td>`;
              }
              const key = `${observerHex}|${observedHex}`;
              const seen = cell.get(key) ?? 0;
              const total = totals.get(observedHex) ?? 0;
              const behind = total > seen ? total - seen : 0;
              const seenClass = behind === 0 ? 'ok' : 'sexpr-num';
              return html`<td>
                <span class=${seenClass}>${seen}</span>${behind > 0
                  ? html` <span class="err">-${behind}</span>`
                  : null}
              </td>`;
            })}
          </tr>
        `;
      })}
      <tr>
        <td><em>have (local)</em></td>
        <td>-</td>
        ${colIds.map(observedHex => {
          const total = totals.get(observedHex) ?? 0;
          return html`<td><span class="ok">${total}</span></td>`;
        })}
      </tr>
      <tr>
        <td><em>mesh (min)</em></td>
        <td>-</td>
        ${colIds.map(observedHex => {
          let minSeen = Infinity;
          for (const obs of observers) {
            minSeen = Math.min(minSeen, cell.get(`${hex(obs.public_key)}|${observedHex}`) ?? 0);
          }
          if (minSeen === Infinity) minSeen = 0;
          const total = totals.get(observedHex) ?? 0;
          const behind = total > minSeen ? total - minSeen : 0;
          const klass = behind === 0 ? 'ok' : 'sexpr-num';
          return html`<td>
            <span class=${klass}>${minSeen}</span>${behind > 0
              ? html` <span class="err">-${behind}</span>`
              : null}
          </td>`;
        })}
      </tr>
      <tr>
        <td><em>pending ack</em></td>
        <td>-</td>
        ${colIds.map(observedHex => {
          const entry = ackEntries.find(e => hex(e.author) === observedHex);
          if (!entry) return html`<td class="muted">-</td>`;
          const gap = Number(entry.tip_count) - Number(entry.acknowledged_count);
          return html`<td><span class="err">+${gap}</span></td>`;
        })}
      </tr>
      <tr>
        <td><em>prunable</em></td>
        <td>-</td>
        ${colIds.map(observedHex => {
          let minSeen = Infinity;
          for (const obs of observers) {
            minSeen = Math.min(minSeen, cell.get(`${hex(obs.public_key)}|${observedHex}`) ?? 0);
          }
          if (minSeen === Infinity) minSeen = 0;
          return html`<td><span class="ok">${minSeen}</span></td>`;
        })}
      </tr>
    </table>
  `;
}

async function loadDebugLog(storeId) {
  const entries = (await sdk.api.store.WitnessLog({ store_id: storeId })).items || [];
  return html`
    <${DebugNav} storeId=${storeId} debugView="log" />
    ${entries.length === 0
      ? html`<div class="empty-state">No witness log entries</div>`
      : html`
        <table>
          <tr><th>Seq</th><th>Hash</th><th>Intention</th><th>Wall Time</th><th>Prev</th><th>Sig</th></tr>
          ${entries.map(w => html`
              <tr>
                <td>${w.seq}</td>
                <td class="mono">${hex(w.hash) || '-'}</td>
                <td class="mono">${hex(w.intention_hash) || '-'}</td>
                <td>${w.wall_time ? fmtTime(w.wall_time) : '-'}</td>
                <td class="mono">${hex(w.prev_hash) || '-'}</td>
                <td class="mono">${hex(w.signature) || '-'}</td>
              </tr>
          `)}
        </table>
      `
    }
  `;
}

async function loadDebugIntentions(storeId) {
  const entries = (await sdk.api.store.WitnessLog({ store_id: storeId })).items || [];
  if (entries.length === 0) {
    return html`<${DebugNav} storeId=${storeId} debugView="intentions" /><div class="empty-state">No intentions</div>`;
  }

  const intentionHashes = extractIntentionHashes(entries);

  if (intentionHashes.length === 0) {
    return html`<${DebugNav} storeId=${storeId} debugView="intentions" /><div class="empty-state">No intention hashes found in witness log</div>`;
  }

  const rows = [];
  for (const { hex: h, bytes: hashBytes } of intentionHashes) {
    try {
      const resp = await sdk.api.store.GetIntention({ store_id: storeId, hash_prefix: hashBytes.slice(0, 4) });
      if (!resp.intention) continue;
      rows.push(resp);
    } catch (e) {
      rows.push({ _error: e.message, _hash: h });
    }
  }

  return html`
    <${DebugNav} storeId=${storeId} debugView="intentions" />
    <table>
      <tr><th>Hash</th><th>Author</th><th>Store Prev</th><th>Wall Time</th><th>Ops</th></tr>
      ${rows.map(r => r._error
        ? html`<tr><td class="mono">${r._hash.slice(0, 16)}...</td><td colspan="4" class="muted">${r._error}</td></tr>`
        : html`
          <tr>
            <td class="mono">${hex(r.intention.hash)}</td>
            <td class="mono">${hex(r.intention.author)}</td>
            <td class="mono">${hex(r.intention.store_prev) || '-'}</td>
            <td>${r.intention.timestamp ? fmtTime(r.intention.timestamp.wall_time) : '-'}</td>
            <td class="mono pre-wrap">${fmtOps(r.intention.ops)}</td>
          </tr>
        `
      )}
    </table>
  `;
}

async function loadDebugFloating(storeId) {
  const floating = (await sdk.api.store.FloatingIntentions({ store_id: storeId })).items || [];
  return html`
    <${DebugNav} storeId=${storeId} debugView="floating" />
    ${floating.length === 0
      ? html`<div class="empty-state">No floating intentions</div>`
      : html`
        <table>
          <tr><th>Hash</th><th>Author</th><th>Store Prev</th><th>Wall Time</th><th>Received</th></tr>
          ${floating.map(f => {
            const i = f.intention || {};
            const ts = i.timestamp;
            return html`
              <tr>
                <td class="mono">${hex(i.hash) || '-'}</td>
                <td class="mono">${hex(i.author) || '-'}</td>
                <td class="mono">${hex(i.store_prev) || '-'}</td>
                <td>${ts ? fmtTime(ts.wall_time) : '-'}</td>
                <td>${f.received_at ? fmtTime(f.received_at) : '-'}</td>
              </tr>
            `;
          })}
        </table>
        <div class="muted table-count">${floating.length} floating intention${floating.length !== 1 ? 's' : ''}</div>
      `
    }
  `;
}

export function IntentionDetail({ intention, ops, hexStr, error }) {
  if (error) {
    return html`
      <div class="action-bar"><button class="btn" onClick=${() => S.clearPanelOverride()}>Back</button></div>
      <div class="card"><p class="err">${error}</p></div>
    `;
  }
  if (!intention) {
    return html`
      <div class="action-bar"><button class="btn" onClick=${() => S.clearPanelOverride()}>Back</button></div>
      <div class="card"><p class="err">No intention found matching '${hexStr}'</p></div>
    `;
  }

  const i = intention;
  const condHashes = i.condition?.v1?.hashes || [];
  const condStr = condHashes.length > 0 ? condHashes.map(h => hex(h)).join(', ') : '-';
  const ts = i.timestamp;

  return html`
    <div class="action-bar"><button class="btn" onClick=${() => S.clearPanelOverride()}>Back</button></div>
    <div class="card">
      <h3>Intention</h3>
      <div class="kv">
        <div class="k">Hash</div><div class="v mono">${hex(i.hash)}</div>
        <div class="k">Author</div><div class="v mono">${hex(i.author)}</div>
        <div class="k">Wall Time</div><div class="v">${ts ? fmtTime(ts.wall_time) : '-'}</div>
        <div class="k">Store Prev</div><div class="v mono">${hex(i.store_prev) || '-'}</div>
        <div class="k">Condition</div><div class="v mono">${condStr}</div>
        <div class="k">Ops</div><div class="v mono pre-wrap">${fmtOps(ops)}</div>
      </div>
    </div>
  `;
}
