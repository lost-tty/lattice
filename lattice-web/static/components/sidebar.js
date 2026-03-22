import { nodeStatus, stores, apps as appsSignal, showModal, toast, refreshNodeStatus, refreshApps } from '../state.js';
import { uuidFromBytes } from '../helpers.js';
import { navigate } from '../router.js';
import { html, useState, useEffect, useRef, Icon } from './util.js';
import { sdk } from '../sdk.js';

export function NodeName({ name }) {
  const [editing, setEditing] = useState(false);
  const inputRef = useRef(null);

  const save = async () => {
    const val = inputRef.current?.value?.trim();
    setEditing(false);
    if (!val || val === name) return;
    try {
      await sdk.api.node.SetName({ name: val });
      toast('Node renamed', 'ok');
      await refreshNodeStatus();
    } catch (e) { toast('Rename error: ' + e.message, 'err'); }
  };

  if (editing) {
    return html`<input ref=${inputRef} class="inline-edit"
      value=${name || ''} autofocus
      onClick=${(e) => e.stopPropagation()}
      onKeyDown=${(e) => { if (e.key === 'Enter') save(); if (e.key === 'Escape') setEditing(false); }}
      onBlur=${save}
    />`;
  }

  return html`<span class="editable" onClick=${(e) => { e.stopPropagation(); setEditing(true); }}>${name || '(unnamed)'}</span>`;
}

export function Sidebar({ activeStoreId }) {
  const ns = nodeStatus.value;
  const storeList = stores.value;
  const activeUuid = activeStoreId ? uuidFromBytes(activeStoreId) : null;

  return html`
    <nav>
      <div id="node-info" class="${!activeUuid ? 'active' : ''}" onClick=${() => navigate('/')}>
        <div class="label">Node</div>
        <div class="value">${ns?.display_name || '(unnamed)'}</div>
      </div>
      <${AppsList} />
      <div class="sidebar-section">
        <span class="label">Stores</span>
        <button class="btn-icon" title="Join Store" onClick=${() => showModal('joinStore')}><${Icon} name="log-in" /></button>
        <button class="btn-icon" title="Create Root Store" onClick=${() => showModal('createStore')}><${Icon} name="plus" /></button>
      </div>
      <div id="store-list" class="sidebar-list">
        ${storeList.map(s => {
          const uuid = uuidFromBytes(s.id);
          const isActive = uuid === activeUuid;
          const depth = s.depth || 0;
          return html`
            <div
              class="sidebar-item ${isActive ? 'active' : ''} ${depth > 0 ? 'nested' : ''} ${s.archived ? 'archived' : ''}"
              style="--depth:${depth}"
              onClick=${() => navigate('/store/' + uuid)}
            >
              <span class="name">${s.name || uuid}</span>
              <span class="meta">${s.store_type}${s.archived ? ' (archived)' : ''}</span>
            </div>
          `;
        })}
      </div>

    </nav>
  `;
}

// ============================================================================
// Apps section
// ============================================================================

export async function toggleApp(app, enabled) {
  try {
    await sdk.api.node.ToggleApp({
      subdomain: app.subdomain,
      enabled,
      registry_store_id: app.registry_store_id,
    });
    toast(enabled ? 'App enabled' : 'App disabled', 'ok');
    await refreshApps();
  } catch (e) { toast('Toggle error: ' + e.message, 'err'); }
}

function AppsList() {
  const appList = appsSignal.value;

  return html`
    <div class="sidebar-section">
      <span class="label">Apps</span>
      <button class="btn-icon" title="Create App" onClick=${() => showModal('registerApp')}><${Icon} name="plus" /></button>
    </div>
    ${appList.length > 0 && html`<div id="app-list" class="sidebar-list">
      ${appList.map(app => {
        const enabled = app.enabled !== false;
        return html`
          <div class="sidebar-item app-item ${!enabled ? 'disabled' : ''}">
            <a class="name ${!enabled ? 'muted' : ''}"
               href="${enabled ? location.protocol + '//' + app.subdomain + '.' + location.host + '/' : '#'}"
               target=${enabled ? '_blank' : ''}
               rel="noopener"
               onClick=${!enabled ? (e) => e.preventDefault() : null}
            >${app.subdomain}</a>
            <span class="meta">${app.app_id || ''}${!enabled ? ' (off)' : ''}</span>
            <button class="btn-icon" title=${enabled ? 'Disable' : 'Enable'}
              onClick=${(e) => { e.stopPropagation(); toggleApp(app, !enabled); }}
            ><${Icon} name=${enabled ? 'pause' : 'play'} /></button>
            <button class="btn-icon" title="Remove"
              onClick=${(e) => { e.stopPropagation(); showModal('unregisterApp', { subdomain: app.subdomain, registryStoreId: app.registry_store_id }); }}
            ><${Icon} name="close" /></button>
          </div>
        `;
      })}
    </div>`}
  `;
}
