import { html, useState, useEffect } from './util.js';
import { panelOverride, stores, clearPanelOverride } from '../state.js';
import { uuidFromBytes } from '../helpers.js';
import { navigate } from '../router.js';
import { loadDetails } from './details.js';
import { loadPeers } from './peers.js';
import { loadMethods } from './methods.js';
import { loadStreams } from './streams.js';
import { loadHistory } from './history.js';
import { loadDebug, IntentionDetail } from './debug.js';
import { loadSystem } from './system.js';
import { Dashboard } from './dashboard.js';

const TABS = ['details', 'peers', 'methods', 'streams', 'history', 'debug', 'system'];

export function TabBar({ storeId, activeTab }) {
  if (!storeId) return html`<div id="tabs"></div>`;
  const uuid = uuidFromBytes(storeId);
  return html`
    <div id="tabs">
      ${TABS.map(t => html`
        <div
          class="tab ${t === activeTab ? 'active' : ''}"
          onClick=${() => navigate('/store/' + uuid + (t === 'details' ? '' : '/' + t))}
        >${t[0].toUpperCase() + t.slice(1)}</div>
      `)}
    </div>
  `;
}

export function Panel({ storeId, tab, debugView }) {
  const po = panelOverride.value;

  if (!storeId) {
    return html`<${Dashboard} />`;
  }

  if (po) {
    return html`<div id="panel"><${PanelOverride} override=${po} /></div>`;
  }

  // Use stores signal in key so panel refreshes when store data changes
  const sv = stores.value.length;

  return html`
    <div id="panel">
      <${AsyncPanel} storeId=${storeId} tab=${tab} debugView=${debugView}
        key=${uuidFromBytes(storeId) + ':' + tab + ':' + debugView + ':' + sv} />
    </div>
  `;
}

// Wrapper that handles loading + error for async panel content.
function AsyncPanel({ storeId, tab, debugView }) {
  const [content, setContent] = useState(null);
  const [error, setError] = useState(null);

  useEffect(() => {
    let cancelled = false;
    setContent(null);
    setError(null);

    (async () => {
      try {
        let result;
        switch (tab) {
          case 'details': result = await loadDetails(storeId); break;
          case 'peers': result = await loadPeers(storeId); break;
          case 'methods': result = await loadMethods(storeId); break;
          case 'streams': result = await loadStreams(storeId); break;
          case 'history': result = await loadHistory(storeId); break;
          case 'debug': result = await loadDebug(storeId, debugView); break;
          case 'system': result = await loadSystem(storeId); break;
        }
        if (!cancelled) setContent(result);
      } catch (e) {
        if (!cancelled) setError(e.message);
      }
    })();

    return () => { cancelled = true; };
  }, [storeId, tab, debugView]);

  if (error) {
    return html`<div class="card"><div class="text-error">Error: ${error}</div></div>`;
  }

  if (!content) {
    return html`<div class="loading-state">Loading...</div>`;
  }

  return content;
}

function PanelOverride({ override }) {
  const goBack = () => clearPanelOverride();

  if (override.type === 'execResult') {
    return html`
      <div class="action-bar">
        <button class="btn" onClick=${goBack}>Back</button>
      </div>
      <div class="card">
        <h3>${override.method}</h3>
        ${override.body}
      </div>
    `;
  }

  if (override.type === 'execError') {
    return html`
      <div class="action-bar">
        <button class="btn" onClick=${goBack}>Back</button>
      </div>
      <div class="card">
        <h3>${override.method}</h3>
        <p class="err">${override.message}</p>
      </div>
    `;
  }

  if (override.type === 'intention') {
    return html`<${IntentionDetail} intention=${override.intention} ops=${override.ops} hexStr=${override.hexStr} error=${override.error} />`;
  }

  return null;
}
