// ============================================================================
// State — reactive signals for Preact components
//
// Uses @preact/signals. Components that read signal.value automatically
// re-render when the value changes. No manual pub/sub or hooks needed.
// ============================================================================

const { signal, computed, batch } = preactSignals;

const S = (() => {
  // --- Signals ---
  const status      = signal({ state: 'connecting', text: 'Connecting' });
  const nodeStatus  = signal(null);
  const stores      = signal([]);
  const activeStoreId = signal(null);
  const activeTab   = signal('details');
  const debugView   = signal('tips');
  const modal       = signal(null);
  const toasts      = signal([]);
  const subscriptions = signal(new Map());
  const panelOverride = signal(null);
  const refreshCounter = signal(0);

  let nextToastId = 1;
  let nextSubId = 1;

  // --- Toast ---
  function toast(text, cls) {
    const id = nextToastId++;
    toasts.value = [...toasts.value, { id, text, cls: cls || '' }];
    setTimeout(() => {
      toasts.value = toasts.value.filter(t => t.id !== id);
    }, 4000);
  }

  // --- Status (called by rpc.js) ---
  function setStatus(s, text) {
    status.value = { state: s, text };
  }

  // --- Navigation ---
  function selectStore(uuid) {
    batch(() => {
      activeStoreId.value = Helpers.uuidToBytes(uuid);
      panelOverride.value = null;
    });
  }

  function switchTab(tab) {
    batch(() => {
      activeTab.value = tab;
      panelOverride.value = null;
    });
  }

  function switchDebugView(view) {
    batch(() => {
      debugView.value = view;
      panelOverride.value = null;
    });
  }

  // --- Modal ---
  function showModal(type, props) {
    modal.value = { type, props: props || {} };
  }

  function closeModal() {
    modal.value = null;
  }

  // --- Data refresh ---
  async function refresh() {
    try {
      const [ns, st] = await Promise.all([
        API.node.GetStatus({}),
        listStores(),
      ]);
      batch(() => {
        nodeStatus.value = ns;
        stores.value = st;
        refreshCounter.value = refreshCounter.value + 1;
      });
    } catch (e) {
      toast('Refresh error: ' + e.message, 'err');
    }
  }

  // --- Subscriptions ---
  function getNextSubId() { return nextSubId++; }

  function addSubscription(id, sub) {
    const m = new Map(subscriptions.value);
    m.set(id, sub);
    subscriptions.value = m;
  }

  function removeSubscription(id) {
    const m = new Map(subscriptions.value);
    m.delete(id);
    subscriptions.value = m;
  }

  function updateSubscription(id, patch) {
    const cur = subscriptions.value.get(id);
    if (!cur) return;
    const m = new Map(subscriptions.value);
    m.set(id, { ...cur, ...patch });
    subscriptions.value = m;
  }

  // --- Panel override (exec results, intention inspect) ---
  function setPanelOverride(override) {
    panelOverride.value = override;
  }

  function clearPanelOverride() {
    panelOverride.value = null;
  }

  return {
    // Signals (read via .value in components for auto-tracking)
    status, nodeStatus, stores, activeStoreId, activeTab,
    debugView, modal, toasts, subscriptions, panelOverride,
    refreshCounter,
    // Actions
    toast, setStatus,
    selectStore, switchTab, switchDebugView,
    showModal, closeModal,
    refresh,
    getNextSubId, addSubscription, removeSubscription, updateSubscription,
    setPanelOverride, clearPanelOverride,
  };
})();
