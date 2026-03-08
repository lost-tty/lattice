async function loadPeers(storeId) {
  const peers = (await API.store.ListPeers({ id: storeId })).peers || [];
  return html`<${PeersView} peers=${peers} storeId=${storeId} />`;
}

function PeersView({ peers, storeId }) {
  if (peers.length === 0) {
    return html`<div class="empty-state">No peers</div>`;
  }
  return html`
    <table>
      <tr><th>Name</th><th>Public Key</th><th>Status</th><th>Online</th><th>Last Seen</th><th></th></tr>
      ${peers.map(p => html`
        <tr>
          <td>${p.name || '-'}</td>
          <td class="mono">${Helpers.pubkeyShort(p.public_key)}</td>
          <td><span class="badge ${p.status === 'Active' ? 'badge-green' : p.status === 'Revoked' ? 'badge-red' : 'badge-yellow'}">${p.status}</span></td>
          <td>${p.online
            ? html`<span class="badge badge-green">yes</span>`
            : html`<span class="badge badge-red">no</span>`}</td>
          <td>${p.last_seen_ms ? fmtTime(p.last_seen_ms) : '-'}</td>
          <td>${p.status !== 'Revoked' ? html`
            <button class="btn btn-danger" onClick=${() => doRevokePeer(storeId, p.public_key)}>Revoke</button>
          ` : null}</td>
        </tr>
      `)}
    </table>
  `;
}
