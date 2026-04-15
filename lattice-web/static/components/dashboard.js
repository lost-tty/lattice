// Dashboard — landing page focused on apps and meshes.

import { html, useState, useEffect } from './util.js';
import { nodeStatus, stores, apps as appsSignal, showModal, toast } from '../state.js';
import { sdk } from '../sdk.js';
import { uuidFromBytes, hexFromBytes, appHref } from '../helpers.js';
import { navigate } from '../router.js';
import { Icon } from './util.js';
import { NodeName, toggleApp } from './sidebar.js';

export function Dashboard() {
  const ns = nodeStatus.value;
  const storeList = stores.value;
  const appList = appsSignal.value;
  const [peerCounts, setPeerCounts] = useState({});

  useEffect(() => {
    if (!sdk) return;
    const roots = storeList.filter(s => (s.depth || 0) === 0);
    Promise.all(roots.map(async s => {
      try {
        const resp = await sdk.api.store.ListPeers({ store_id: s.id });
        return [uuidFromBytes(s.id), (resp.items || []).length];
      } catch { return [uuidFromBytes(s.id), 0]; }
    })).then(counts => {
      setPeerCounts(Object.fromEntries(counts));
    });
  }, [storeList]);

  const meshes = storeList.filter(s => (s.depth || 0) === 0);

  return html`
    <div id="panel">
      <div class="dashboard">

        <h2><${NodeName} name=${ns?.display_name} /></h2>

        <div class="dashboard-section">
          <div class="dashboard-section-header">
            <h3>Apps</h3>
            ${meshes.length > 0 && html`
              <button class="btn btn-sm" onClick=${() => showModal('registerApp')}>
                <${Icon} name="plus" size=${12} /> New App
              </button>
            `}
          </div>
          ${appList.length > 0 ? html`
            <div class="dashboard-grid">
              ${appList.map(app => {
                const enabled = app.enabled !== false;
                return html`
                  <div class="dashboard-card">
                    <div class="dashboard-card-header">
                      <span class="dashboard-card-title">${app.subdomain}</span>
                      <span class="badge ${enabled ? 'badge-green' : 'badge-muted'}">
                        ${enabled ? 'active' : 'disabled'}
                      </span>
                    </div>
                    <div class="dashboard-card-meta">${app.app_id}</div>
                    <div class="dashboard-card-actions">
                      ${enabled
                        ? html`<a class="dashboard-card-link" href=${appHref(app.subdomain)} target="_blank" rel="noopener">Open</a>`
                        : html`<span />`
                      }
                      <div class="dashboard-card-btns">
                        <button class="btn btn-sm" onClick=${() => toggleApp(app, !enabled)}>
                          ${enabled ? 'Disable' : 'Enable'}
                        </button>
                        <button class="btn btn-sm btn-danger" onClick=${() => showModal('unregisterApp', { subdomain: app.subdomain, registryStoreId: app.registry_store_id })}>
                          Remove
                        </button>
                      </div>
                    </div>
                  </div>
                `;
              })}
            </div>
          ` : html`
            <div class="dashboard-empty">
              <p>No apps deployed yet.</p>
              ${meshes.length > 0
                ? html`<p class="muted">Create an app or drag & drop a bundle (.zip / .lattice).</p>`
                : html`<p class="muted">Create or join a mesh first, then deploy apps.</p>`
              }
            </div>
          `}
        </div>

        <div class="dashboard-section">
          <div class="dashboard-section-header">
            <h3>Meshes</h3>
            <button class="btn btn-sm" onClick=${() => showModal('createStore')}>
              <${Icon} name="plus" size=${12} /> Create
            </button>
            <button class="btn btn-sm" onClick=${() => showModal('joinStore')}>
              <${Icon} name="log-in" size=${12} /> Join
            </button>
          </div>
          ${meshes.length > 0 ? html`
            <div class="dashboard-grid">
              ${meshes.map(s => {
                const uuid = uuidFromBytes(s.id);
                const peers = peerCounts[uuid] || 0;
                // Count children: stores after this root with depth > 0, until the next root
                const rootIdx = storeList.indexOf(s);
                let childCount = 0;
                for (let i = rootIdx + 1; i < storeList.length && (storeList[i].depth || 0) > 0; i++) childCount++;
                return html`
                  <div class="dashboard-card clickable" onClick=${() => navigate('/store/' + uuid)}>
                    <div class="dashboard-card-header">
                      <span class="dashboard-card-title">${s.name || uuid}</span>
                    </div>
                    <div class="dashboard-card-stats">
                      <span>${peers} peer${peers !== 1 ? 's' : ''}</span>
                      <span>${childCount} store${childCount !== 1 ? 's' : ''}</span>
                    </div>
                  </div>
                `;
              })}
            </div>
          ` : html`
            <div class="dashboard-empty">
              <p>Not part of any mesh yet.</p>
              <p class="muted">Create a new root store or join an existing one with an invite token.</p>
            </div>
          `}
        </div>

        <div class="dashboard-footer">
          ${ns?.public_key ? html`<span><span class="muted">Node ID </span><span class="mono copyable" title="Click to copy" onClick=${() => {
            navigator.clipboard.writeText(hexFromBytes(ns.public_key));
            toast('Copied to clipboard', 'ok');
          }}>${hexFromBytes(ns.public_key)}</span></span>` : null}
          ${ns?.version ? html`<span>v${ns.version}</span>` : null}
        </div>

      </div>
    </div>
  `;
}
