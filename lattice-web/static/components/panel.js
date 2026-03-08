const TABS = ['details', 'peers', 'methods', 'streams', 'debug', 'system'];

function TabBar() {
  const activeStoreId = S.activeStoreId.value;
  if (!activeStoreId) return null;
  const activeTab = S.activeTab.value;
  return html`
    <div id="tabs">
      ${TABS.map(t => html`
        <div
          class="tab ${t === activeTab ? 'active' : ''}"
          onClick=${() => S.switchTab(t)}
        >${t}</div>
      `)}
    </div>
  `;
}

function Panel() {
  const activeStoreId = S.activeStoreId.value;
  const panelOverride = S.panelOverride.value;

  if (!activeStoreId) {
    return html`<div id="panel"><div class="empty-state">Select a store from the sidebar</div></div>`;
  }

  if (panelOverride) {
    return html`<div id="panel"><${PanelOverride} override=${panelOverride} /></div>`;
  }

  const activeTab = S.activeTab.value;
  const debugView = S.debugView.value;
  const rc = S.refreshCounter.value;

  return html`
    <div id="panel">
      <${AsyncPanel} key=${Helpers.uuidFromBytes(activeStoreId) + ':' + activeTab + ':' + debugView + ':' + rc} />
    </div>
  `;
}

// Wrapper that handles loading + error for async panel content.
function AsyncPanel() {
  const [content, setContent] = useState(null);
  const [error, setError] = useState(null);

  // Read signal values once at mount (key= already ensures remount on change)
  const sid = S.activeStoreId.value;
  const tab = S.activeTab.value;
  const dv = S.debugView.value;

  useEffect(() => {
    let cancelled = false;
    setContent(null);
    setError(null);

    (async () => {
      try {
        let result;
        switch (tab) {
          case 'details': result = await loadDetails(sid); break;
          case 'peers': result = await loadPeers(sid); break;
          case 'methods': result = await loadMethods(sid); break;
          case 'streams': result = await loadStreams(sid); break;
          case 'debug': result = await loadDebug(sid, dv); break;
          case 'system': result = await loadSystem(sid); break;
        }
        if (!cancelled) setContent(result);
      } catch (e) {
        if (!cancelled) setError(e.message);
      }
    })();

    return () => { cancelled = true; };
  }, [sid, tab, dv]);

  if (error) {
    return html`<div class="card"><div style="color:var(--red)">Error: ${error}</div></div>`;
  }

  if (!content) {
    return html`<div style="color:var(--muted);padding:20px">Loading...</div>`;
  }

  return content;
}

function PanelOverride({ override }) {
  if (override.type === 'execResult') {
    return html`
      <div class="action-bar">
        <button class="btn" onClick=${() => S.switchTab('methods')}>Back</button>
      </div>
      <div class="card">
        <h3>${override.method}</h3>
        <div dangerouslySetInnerHTML=${{ __html: override.body }} />
      </div>
    `;
  }

  if (override.type === 'execError') {
    return html`
      <div class="action-bar">
        <button class="btn" onClick=${() => S.switchTab('methods')}>Back</button>
      </div>
      <div class="card">
        <h3>${override.method}</h3>
        <p class="err">${override.message}</p>
      </div>
    `;
  }

  if (override.type === 'intention') {
    return html`<${IntentionDetail} intention=${override.intention} hexStr=${override.hexStr} error=${override.error} />`;
  }

  return null;
}
