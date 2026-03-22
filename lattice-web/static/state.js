// ============================================================================
// State — reactive signals + event-driven data layer
//
// Uses @preact/signals. Components that read signal.value automatically
// re-render when the value changes.
//
// Data flow:
//   1. connect() does the initial load (node status, stores, apps)
//   2. Node event stream updates stores on store_ready/sync_result
//   3. Root store watch-apps streams update apps on register/remove
//   4. Mutations (toggle, create, etc.) update via the same streams
// ============================================================================

import { signal, batch } from './vendor/preact.mjs';
import { sdk } from './sdk.js';
import { uuidToBytes, uuidFromBytes, isRootStore } from './helpers.js';

// --- Signals ---
export const status      = signal({ state: 'connecting', text: 'Connecting' });
export const nodeStatus  = signal(null);
export const stores      = signal([]);
export const apps        = signal([]);
export const activeStoreId = signal(null);
export const activeTab   = signal('details');
export const debugView   = signal('tips');
export const modal       = signal(null);
export const toasts      = signal([]);
export const subscriptions = signal(new Map());
export const panelOverride = signal(null);

let nextToastId = 1;
let nextSubId = 1;

// Track which root stores we're watching so we don't double-subscribe
const watchedRootStores = new Set();

// --- Toast ---
export function toast(text, cls) {
  const id = nextToastId++;
  toasts.value = [...toasts.value, { id, text, cls: cls || '' }];
  setTimeout(() => {
    toasts.value = toasts.value.filter(t => t.id !== id);
  }, 4000);
}

// --- Status ---
export function setStatus(s, text) {
  status.value = { state: s, text };
}

// --- Navigation (driven by Router) ---
export function applyRoute(route) {
  batch(() => {
    panelOverride.value = null;
    if (route.page === 'store') {
      activeStoreId.value = uuidToBytes(route.storeId);
      activeTab.value = route.tab || 'details';
      debugView.value = route.debugView || 'tips';
    } else {
      activeStoreId.value = null;
    }
  });
}

// --- Modal ---
export function showModal(type, props) {
  modal.value = { type, props: props || {} };
}

export function closeModal() {
  modal.value = null;
}

// ============================================================================
// Event-driven data layer
// ============================================================================

/** Initial load + subscribe to all event streams. Called once after SDK connect. */
export async function connect() {
  await refreshAll();
  subscribeNodeEvents();
  subscribeAppWatchers();
}

/** Fetch everything. Used for initial load only. */
async function refreshAll() {
  try {
    const [ns, st, ap] = await Promise.all([
      sdk.api.node.GetStatus({}),
      listAllStores(),
      listApps(),
    ]);
    batch(() => {
      nodeStatus.value = ns;
      stores.value = st;
      apps.value = ap;
    });
  } catch (e) {
    toast('Load error: ' + e.message, 'err');
  }
}

/** Re-fetch just the store tree and update the signal. */
async function refreshStores() {
  try {
    stores.value = await listAllStores();
    // Check if new root stores appeared — subscribe to their watch streams
    subscribeAppWatchers();
  } catch (e) {
    toast('Store refresh error: ' + e.message, 'err');
  }
}

/** Re-fetch just the apps list and update the signal. */
async function refreshApps() {
  try {
    apps.value = await listApps();
  } catch (e) {
    toast('Apps refresh error: ' + e.message, 'err');
  }
}

/** Re-fetch node status. */
async function refreshNodeStatus() {
  try {
    nodeStatus.value = await sdk.api.node.GetStatus({});
  } catch {}
}

// --- Exposed for mutations that need to trigger updates ---
// After a mutation (create store, toggle app, etc.), call the
// appropriate targeted refresh instead of refreshing everything.
export { refreshStores, refreshApps, refreshNodeStatus };

// --- Node event stream ---
function subscribeNodeEvents() {
  const NodeEvent = sdk.proto.lookup('lattice.daemon.v1.NodeEvent');

  sdk.rpc.subscribe('node', 'Subscribe', new Uint8Array(0), function (eventData) {
    const ev = NodeEvent.decode(eventData);
    if (ev.store_ready) {
      toast('Store ready: ' + uuidFromBytes(ev.store_ready.store_id), 'ok');
      refreshStores();
    } else if (ev.join_failed) {
      toast('Join failed: ' + (ev.join_failed.reason || 'unknown'), 'err');
    } else if (ev.sync_result) {
      const sr = ev.sync_result;
      toast('Sync: ' + (sr.peers_synced || 0) + ' peers, ' + (sr.entries_sent || 0) + ' sent, ' + (sr.entries_received || 0) + ' received', 'ok');
      refreshStores();
    }
  }, function () {
    toast('Node event stream ended', 'err');
  });
}

// --- Root store watch-apps streams ---
function subscribeAppWatchers() {
  const storeList = stores.value;
  for (const s of storeList) {
    if (!isRootStore(s)) continue;
    const uuid = uuidFromBytes(s.id);
    if (watchedRootStores.has(uuid)) continue;
    watchedRootStores.add(uuid);
    subscribeWatchApps(s.id, uuid);
  }
}

function subscribeWatchApps(storeIdBytes, uuid) {
  const SubscribeRequest = sdk.proto.lookup('lattice.daemon.v1.SubscribeRequest');
  const StoreEvent = sdk.proto.lookup('lattice.daemon.v1.StoreEvent');

  const payload = SubscribeRequest.encode(SubscribeRequest.create({
    store_id: storeIdBytes,
    stream_name: 'watch-apps',
    params: new Uint8Array(0),
  })).finish();

  sdk.rpc.subscribe('dynamic', 'Subscribe', payload, function (eventData) {
    // Any app change → refresh the apps list
    StoreEvent.decode(eventData); // decode to validate, but we just refresh
    refreshApps();
  }, function () {
    // Stream ended — remove from watched so we can re-subscribe
    watchedRootStores.delete(uuid);
  });
}

// ============================================================================
// Data fetchers (pure, no side effects on signals)
// ============================================================================

async function listApps() {
  const resp = await sdk.api.node.ListActiveApps({});
  return (resp.bindings || []).sort((a, b) =>
    (a.subdomain || '').localeCompare(b.subdomain || '')
  );
}

async function listAllStores() {
  const roots = (await sdk.api.store.List({})).stores || [];
  const result = [];
  const seen = new Set();
  async function walk(list, depth) {
    for (const s of list) {
      const uuid = uuidFromBytes(s.id);
      if (seen.has(uuid)) continue;
      seen.add(uuid);
      result.push(Object.assign({}, s, { depth }));
      try {
        const children = (await sdk.api.store.List({ parent_id: s.id })).stores || [];
        if (children.length > 0) await walk(children, depth + 1);
      } catch {}
    }
  }
  await walk(roots, 0);
  return result;
}

// --- Subscriptions (user-initiated stream subscriptions, e.g. from Streams tab) ---
export function getNextSubId() { return nextSubId++; }

export function addSubscription(id, sub) {
  const m = new Map(subscriptions.value);
  m.set(id, sub);
  subscriptions.value = m;
}

export function removeSubscription(id) {
  const m = new Map(subscriptions.value);
  m.delete(id);
  subscriptions.value = m;
}

export function updateSubscription(id, patch) {
  const cur = subscriptions.value.get(id);
  if (!cur) return;
  const m = new Map(subscriptions.value);
  m.set(id, { ...cur, ...patch });
  subscriptions.value = m;
}

// --- Panel override (exec results, intention inspect) ---
export function setPanelOverride(override) {
  panelOverride.value = override;
}

export function clearPanelOverride() {
  panelOverride.value = null;
}
