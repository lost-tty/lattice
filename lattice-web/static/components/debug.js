import { html, hex, fmtTime, fmtOps, extractIntentionHashes } from './util.js';
import * as Helpers from '../helpers.js';
import * as S from '../state.js';
import * as Router from '../router.js';
import { sdk } from '../sdk.js';

export async function loadDebug(storeId, debugView) {
  switch (debugView) {
    case 'tips': return await loadDebugTips(storeId);
    case 'log': return await loadDebugLog(storeId);
    case 'intentions': return await loadDebugIntentions(storeId);
    case 'floating': return await loadDebugFloating(storeId);
  }
}

function DebugNav({ storeId, debugView }) {
  const uuid = Helpers.uuidFromBytes(storeId);
  const views = ['tips', 'log', 'intentions', 'floating'];
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
  const authors = (await sdk.api.store.Debug({ id: storeId })).authors || [];
  return html`
    <${DebugNav} storeId=${storeId} debugView="tips" />
    ${authors.length === 0
      ? html`<div class="empty-state">No author state</div>`
      : html`
        <table>
          <tr><th>Author</th><th>Tip</th></tr>
          ${authors.map(a => html`
            <tr>
              <td class="mono">${Helpers.pubkeyShort(a.public_key)}</td>
              <td class="mono">${hex(a.hash) || '-'}</td>
            </tr>
          `)}
        </table>
      `
    }
  `;
}

async function loadDebugLog(storeId) {
  const entries = (await sdk.api.store.WitnessLog({ store_id: storeId })).entries || [];
  const WitnessContent = sdk.proto.lookup('lattice.weaver.WitnessContent');
  return html`
    <${DebugNav} storeId=${storeId} debugView="log" />
    ${entries.length === 0
      ? html`<div class="empty-state">No witness log entries</div>`
      : html`
        <table>
          <tr><th>Seq</th><th>Hash</th><th>Intention</th><th>Wall Time</th><th>Prev</th><th>Sig</th></tr>
          ${entries.map(w => {
            let intentionHash = '-', wallTime = '-', prevHash = '-';
            if (w.content && w.content.length > 0) {
              try {
                const wc = WitnessContent.decode(w.content);
                intentionHash = hex(wc.intention_hash) || '-';
                wallTime = wc.wall_time ? fmtTime(wc.wall_time) : '-';
                prevHash = hex(wc.prev_hash) || '-';
              } catch (e) { /* decode failed */ }
            }
            return html`
              <tr>
                <td>${w.seq}</td>
                <td class="mono">${hex(w.hash) || '-'}</td>
                <td class="mono">${intentionHash}</td>
                <td>${wallTime}</td>
                <td class="mono">${prevHash}</td>
                <td class="mono">${hex(w.signature) || '-'}</td>
              </tr>
            `;
          })}
        </table>
      `
    }
  `;
}

async function loadDebugIntentions(storeId) {
  const entries = (await sdk.api.store.WitnessLog({ store_id: storeId })).entries || [];
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
      rows.push({ intention: resp.intention, ops: resp.ops || [] });
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
            <td class="mono pre-wrap">${fmtOps(r.ops)}</td>
          </tr>
        `
      )}
    </table>
  `;
}

async function loadDebugFloating(storeId) {
  const floating = (await sdk.api.store.FloatingIntentions({ id: storeId })).intentions || [];
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
        <div class="k">Signature</div><div class="v mono">${hex(i.signature) || '-'}</div>
        <div class="k">Ops</div><div class="v mono pre-wrap">${fmtOps(ops)}</div>
      </div>
    </div>
  `;
}
