import { html, hex } from './util.js';
import * as Helpers from '../helpers.js';
import * as S from '../state.js';
import { doSync, doInvite, doAck } from './actions.js';
import { sdk } from '../sdk.js';
import { computeUpset, UpsetPlot } from './upset.js';

export async function loadDetails(storeId) {
  const sid = { store_id: storeId };
  const [details, meta, peerStrategyResp, observationsResp] = await Promise.all([
    sdk.api.store.GetDetails(sid),
    sdk.api.store.GetStatus(sid),
    sdk.api.store.GetPeerStrategy(sid).catch(() => ({})),
    sdk.api.store.GetAuthorStateObservations(sid).catch(() => ({})),
  ]);
  const peerStrategy = peerStrategyResp.strategy || '';
  const uuid = Helpers.uuidFromBytes(storeId);

  return html`<${DetailsView}
    uuid=${uuid} details=${details} meta=${meta}
    peerStrategy=${peerStrategy} storeId=${storeId}
    observationsResp=${observationsResp}
  />`;
}

function UpsetCard({ observationsResp }) {
  const observations = observationsResp.items || [];
  const observers = observationsResp.observers || [];
  const totalsList = observationsResp.author_totals || [];

  if (observers.length === 0 || totalsList.length === 0) return null;

  const nameFor = new Map();
  for (const obs of observers) if (obs.peer_name) nameFor.set(hex(obs.public_key), obs.peer_name);

  const { hulls, buckets, totalIntentions } = computeUpset(observations, observers, totalsList);
  if (hulls.length === 0 || buckets.length === 0) return null;

  const allHullsKey = hulls.map((_, i) => i).join(',');
  const prunable = buckets.find(b => b.members.join(',') === allHullsKey)?.count ?? 0;

  return html`
    <div class="card">
      <h3>Mesh Convergence</h3>
      <${UpsetPlot} hulls=${hulls} buckets=${buckets} nameFor=${nameFor} />
      <div class="muted table-count">
        ${`${totalIntentions} intention(s), ${hulls.length} hull(s), ${buckets.length} distinct membership set(s), ${prunable} prunable`}
      </div>
    </div>
  `;
}

function DetailsView({ uuid, details, meta, peerStrategy, storeId, observationsResp }) {
  const appliedSeq = details.last_applied_seq || 0;
  const headSeq = details.witness_head_seq || 0;
  const stalled = headSeq > 0 && appliedSeq !== headSeq;

  // Show "Create App" for root KV stores
  const stores = S.stores.value || [];
  const storeInfo = stores.find(s => Helpers.uuidFromBytes(s.id) === uuid);
  const isRootStore = storeInfo && Helpers.isRootStore(storeInfo);

  return html`
    <div class="action-bar">
      <button class="btn" onClick=${() => doSync(storeId)}>Sync</button>
      <button class="btn" onClick=${() => doAck(storeId)}>Ack</button>
      <button class="btn" onClick=${() => doInvite(storeId)}>Invite Peer</button>
      <button class="btn" onClick=${() => S.showModal('createStore', { parentId: storeId })}>Create Child Store</button>
      ${isRootStore && html`<button class="btn" onClick=${() => S.showModal('registerApp', { registryStoreId: storeId })}>Create App</button>`}
      <button class="btn" onClick=${() => S.showModal('rename', { storeId })}>Rename</button>
      <button class="btn btn-danger" onClick=${() => S.showModal('delete', { storeId })}>Delete</button>
    </div>
    ${stalled ? html`
      <div class="card card-warning">
        <span class="badge badge-yellow">stalled</span>
        ${' '}Applied seq ${appliedSeq} is behind witness head seq ${headSeq} (${headSeq - appliedSeq} behind)
      </div>
    ` : null}
    <div class="card">
      <h3>Store Info</h3>
      <div class="kv">
        <div class="k">ID</div><div class="v mono">${uuid}</div>
        <div class="k">Type</div><div class="v">${meta.store_type || ''}</div>
        <div class="k">Peer Strategy</div><div class="v">${peerStrategy || '-'}</div>
        <div class="k">Authors</div><div class="v">${details.author_count || 0}</div>
        <div class="k">Intentions</div><div class="v">${details.intention_count || 0}</div>
        <div class="k">Witnesses</div><div class="v">${details.witness_count || 0}</div>
        <div class="k">Last Applied Seq</div><div class="v">${appliedSeq}</div>
        <div class="k">Last Applied Hash</div><div class="v mono">${hex(details.last_applied_hash) || '-'}</div>
        <div class="k">Witness Head Seq</div><div class="v">${headSeq}</div>
        <div class="k">Witness Head Hash</div><div class="v mono">${hex(details.witness_head_hash) || '-'}</div>
      </div>
    </div>
    <${UpsetCard} observationsResp=${observationsResp} />
  `;
}
