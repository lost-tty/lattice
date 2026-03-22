import { html, useState, useEffect } from './util.js';
import { applyRoute, stores, activeStoreId as activeStoreIdSignal, activeTab as activeTabSignal, debugView as debugViewSignal, toast, showModal } from '../state.js';
import { uuidFromBytes, getRootStores } from '../helpers.js';
import { parse, listen } from '../router.js';
import { uploadBundleToStore } from './actions.js';
import { Header } from './header.js';
import { Sidebar } from './sidebar.js';
import { TabBar, Panel } from './panel.js';
import { Toasts } from './toasts.js';
import { ModalContainer } from './modals.js';

export function App() {
  const [dragging, setDragging] = useState(false);

  // Apply initial route and listen for changes
  useEffect(() => {
    applyRoute(parse());
    return listen((route) => applyRoute(route));
  }, []);

  // Read navigation signals (updated by applyRoute)
  const activeStoreId = activeStoreIdSignal.value;
  const activeTab = activeTabSignal.value;
  const debugViewVal = debugViewSignal.value;

  const onDragOver = (e) => {
    e.preventDefault();
    e.dataTransfer.dropEffect = 'copy';
    setDragging(true);
  };

  const onDragLeave = () => setDragging(false);

  const onDrop = async (e) => {
    e.preventDefault();
    setDragging(false);

    const file = [...e.dataTransfer.files].find(f => f.name.endsWith('.zip'));
    if (!file) {
      toast('Drop a .zip app bundle', 'err');
      return;
    }

    const rootStores = getRootStores(stores.value);

    // Determine target root store
    let targetStore = null;

    // 1. Use active store if it's a root store
    if (activeStoreId) {
      const activeUuid = uuidFromBytes(activeStoreId);
      targetStore = rootStores.find(s => uuidFromBytes(s.id) === activeUuid);
    }

    // 2. If only one root store exists, use it
    if (!targetStore && rootStores.length === 1) {
      targetStore = rootStores[0];
    }

    // 3. Multiple root stores and none selected — let user choose
    if (!targetStore && rootStores.length > 1) {
      showModal('selectRootStore', { file });
      return;
    }

    if (!targetStore) {
      toast('No root store found. Create one first.', 'err');
      return;
    }

    await uploadBundleToStore(targetStore.id, file);
  };

  return html`
    <div
      class="app-root ${dragging ? 'drag-over' : ''}"
      onDragOver=${onDragOver}
      onDragLeave=${onDragLeave}
      onDrop=${onDrop}
    >
      <${Header} />
      <div id="main">
        <${Sidebar} activeStoreId=${activeStoreId} />
        <div id="content">
          <${TabBar} storeId=${activeStoreId} activeTab=${activeTab} />
          <${Panel} storeId=${activeStoreId} tab=${activeTab} debugView=${debugViewVal} />
        </div>
      </div>
      <${Toasts} />
      <${ModalContainer} />
      ${dragging && html`<div class="drop-overlay">Drop app bundle (.zip)</div>`}
    </div>
  `;
}
