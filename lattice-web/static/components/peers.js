import { html, fmtTime } from './util.js';
import * as Helpers from '../helpers.js';
import { doRevokePeer } from './actions.js';
import { sdk } from '../sdk.js';

export async function loadPeers(storeId) {
  const peers = (await sdk.api.store.ListPeers({ store_id: storeId })).items || [];
  return html`<${PeersView} peers=${peers} storeId=${storeId} />`;
}

// Display order; unknown statuses are appended in first-seen order.
const STATUS_ORDER = ['active', 'invited', 'dormant', 'revoked'];

// Status capabilities drive which columns render. A status a peer could
// currently be connected under is "reachable"; anything not yet revoked
// can be revoked.
const REACHABLE = new Set(['active', 'dormant']);
const REVOCABLE = new Set(['active', 'invited', 'dormant']);

function titleCase(s) { return s.charAt(0).toUpperCase() + s.slice(1); }

function PeersView({ peers, storeId }) {
  if (peers.length === 0) {
    return html`<div class="empty-state">No peers</div>`;
  }
  const groups = new Map(STATUS_ORDER.map(s => [s, []]));
  for (const p of peers) {
    if (!groups.has(p.status)) groups.set(p.status, []);
    groups.get(p.status).push(p);
  }
  const sections = [...groups].filter(([, ps]) => ps.length > 0);
  return html`
    ${sections.map(([status, ps], i) => html`
      <h3 class="section-heading${i > 0 ? ' mt-section' : ''}">${titleCase(status)}</h3>
      <${StatusTable} status=${status} peers=${ps} storeId=${storeId} />
    `)}
  `;
}

function StatusTable({ status, peers, storeId }) {
  const showReach = REACHABLE.has(status);
  const showRevoke = REVOCABLE.has(status);
  return html`
    <table>
      <tr>
        <th>Name</th><th>Public Key</th>
        ${showReach ? html`<th>Online</th><th>Last Seen</th>` : null}
        ${showRevoke ? html`<th></th>` : null}
      </tr>
      ${peers.map(p => html`
        <tr>
          <td>${p.name || '-'}</td>
          <td class="mono">${Helpers.pubkeyShort(p.public_key)}</td>
          ${showReach ? html`
            <td>${p.online
              ? html`<span class="badge badge-green">yes</span>`
              : html`<span class="badge badge-red">no</span>`}</td>
            <td>${p.last_seen_ms ? fmtTime(p.last_seen_ms) : '-'}</td>
          ` : null}
          ${showRevoke ? html`
            <td><button class="btn btn-danger" onClick=${() => doRevokePeer(storeId, p.public_key)}>Revoke</button></td>
          ` : null}
        </tr>
      `)}
    </table>
  `;
}
