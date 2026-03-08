async function loadDetails(storeId) {
  const sid = { id: storeId };
  const [details, meta, peerStrategyResp] = await Promise.all([
    API.store.GetDetails(sid),
    API.store.GetStatus(sid),
    API.store.GetPeerStrategy(sid).catch(() => ({})),
  ]);
  const peerStrategy = peerStrategyResp.strategy || '';
  const uuid = Helpers.uuidFromBytes(storeId);

  return html`<${DetailsView}
    uuid=${uuid} details=${details} meta=${meta}
    peerStrategy=${peerStrategy} storeId=${storeId}
  />`;
}

function DetailsView({ uuid, details, meta, peerStrategy, storeId }) {
  return html`
    <div class="action-bar">
      <button class="btn" onClick=${() => doSync(storeId)}>Sync</button>
      <button class="btn" onClick=${() => doInvite(storeId)}>Invite Peer</button>
      <button class="btn" onClick=${() => S.showModal('rename', { storeId })}>Rename</button>
      <button class="btn btn-danger" onClick=${() => S.showModal('delete', { storeId })}>Delete</button>
    </div>
    <div class="card">
      <h3>Store Info</h3>
      <div class="kv">
        <div class="k">ID</div><div class="v mono">${uuid}</div>
        <div class="k">Type</div><div class="v">${meta.store_type || ''}</div>
        <div class="k">Peer Strategy</div><div class="v">${peerStrategy || '-'}</div>
        <div class="k">Authors</div><div class="v">${details.author_count || 0}</div>
        <div class="k">Intentions</div><div class="v">${details.intention_count || 0}</div>
        <div class="k">Witnesses</div><div class="v">${details.witness_count || 0}</div>
        <div class="k">Last Applied Seq</div><div class="v">${details.last_applied_seq || 0}</div>
        <div class="k">Last Applied Hash</div><div class="v mono">${hex(details.last_applied_hash) || '-'}</div>
        <div class="k">Witness Head Seq</div><div class="v">${details.witness_head_seq || 0}</div>
        <div class="k">Witness Head Hash</div><div class="v mono">${hex(details.witness_head_hash) || '-'}</div>
      </div>
    </div>
  `;
}
