import { html, useState, useEffect, useRef, FieldInput, collectFormFields } from './util.js';
import { closeModal, modal, toast, refreshStores, refreshApps, stores, activeStoreId, setPanelOverride } from '../state.js';
import { uuidFromBytes, uuidToBytes, bytesFromHex, getRootStores } from '../helpers.js';
import { getSchema, encodeMessage } from '../schema.js';
import { navigate } from '../router.js';
import { runExecAndShow, doSubscribe, uploadBundleToStore } from './actions.js';
import { sdk } from '../sdk.js';

// Look up a value from a BundleManifest (repeated sections/entries structure).
function manifestGet(manifest, sectionName, key) {
  const section = manifest?.sections?.find(s => s.name === sectionName);
  return section?.entries?.find(e => e.key === key)?.value;
}

function ModalActions({ onSubmit, label, danger }) {
  const cls = danger ? 'btn btn-danger' : 'btn btn-primary';
  return html`
    <div class="modal-actions">
      <button class="btn" onClick=${closeModal}>Cancel</button>
      ${onSubmit ? html`<button class=${cls} onClick=${onSubmit}>${label}</button>` : null}
    </div>
  `;
}

export function ModalContainer() {
  const currentModal = modal.value;
  if (!currentModal) return html`<div id="modal-container"></div>`;

  return html`
    <div id="modal-container">
      <div class="modal-overlay" onClick=${(e) => { if (e.target === e.currentTarget) closeModal(); }}>
        <div class="modal">
          <${ModalContent} type=${currentModal.type} props=${currentModal.props} />
        </div>
      </div>
    </div>
  `;
}

function ModalContent({ type, props }) {
  switch (type) {
    case 'createStore': return html`<${CreateStoreModal} parentId=${props?.parentId} />`;
    case 'joinStore': return html`<${JoinStoreModal} />`;
    case 'rename': return html`<${RenameModal} storeId=${props.storeId} />`;
    case 'delete': return html`<${DeleteModal} storeId=${props.storeId} />`;
    case 'invite': return html`<${InviteModal} token=${props.token} />`;
    case 'exec': return html`<${ExecModal} name=${props.name} storeId=${props.storeId} />`;
    case 'subscribe': return html`<${SubscribeModal} ...${props} />`;
    case 'inspectIntention': return html`<${InspectIntentionModal} />`;
    case 'registerApp': return html`<${RegisterAppModal} registryStoreId=${props?.registryStoreId} />`;
    case 'unregisterApp': return html`<${UnregisterAppModal} subdomain=${props.subdomain} registryStoreId=${props.registryStoreId} />`;
    case 'selectRootStore': return html`<${SelectRootStoreModal} file=${props.file} />`;
    default: return null;
  }
}

function CreateStoreModal({ parentId }) {
  const [types, setTypes] = useState(null);
  const nameRef = useRef(null);
  const typeRef = useRef(null);
  const isChild = !!parentId;

  useEffect(() => {
    sdk.api.node.ListStoreTypes({}).then(
      resp => setTypes(resp.items || []),
      err => toast('Failed to load store types: ' + err.message, 'err'),
    );
  }, []);

  const submit = async () => {
    const name = nameRef.current?.value || '';
    const type_ = typeRef.current?.value;
    const req = { name, store_type: type_ };
    if (parentId) req.parent_id = parentId;
    try {
      await sdk.api.store.Create(req);
      closeModal();
      toast(`Store created: ${name || '(unnamed)'} [${type_}]`, 'ok');
      await refreshStores();
    } catch (e) { toast('Create error: ' + e.message, 'err'); }
  };

  const title = isChild ? 'Create Child Store' : 'Create Root Store';
  if (!types) return html`<h2>${title}</h2><div class="muted">Loading...</div>`;

  return html`
    <h2>${title}</h2>
    ${isChild ? html`<p>Parent: <span class="mono">${uuidFromBytes(parentId)}</span></p>` : null}
    <label>Name (optional)</label>
    <input ref=${nameRef} placeholder="my-store" autofocus />
    <label>Type</label>
    <select ref=${typeRef}>
      ${types.map(t => html`<option value=${t} selected=${!isChild && t === 'core:rootstore'}>${t}</option>`)}
    </select>
    <${ModalActions} onSubmit=${submit} label="Create" />
  `;
}

function JoinStoreModal() {
  const inputRef = useRef(null);

  const submit = async () => {
    const token = inputRef.current?.value?.trim();
    if (!token) return;
    closeModal();
    try {
      const resp = await sdk.api.store.Join({ token });
      const uuid = resp.store_id ? uuidFromBytes(resp.store_id) : '';
      toast(`Join initiated${uuid ? ': ' + uuid : ''}`, 'ok');
      await refreshStores();
    } catch (e) { toast('Join error: ' + e.message, 'err'); }
  };

  return html`
    <h2>Join Store</h2>
    <label>Invite token</label>
    <input ref=${inputRef} placeholder="Paste invite token" autofocus />
    <${ModalActions} onSubmit=${submit} label="Join" />
  `;
}

function RenameModal({ storeId }) {
  const inputRef = useRef(null);

  const submit = async () => {
    const name = inputRef.current?.value;
    if (!name) return;
    try {
      await sdk.api.store.SetName({ store_id: storeId, name });
      closeModal();
      toast('Store renamed to: ' + name, 'ok');
      await refreshStores();
    } catch (e) { toast('Rename error: ' + e.message, 'err'); }
  };

  return html`
    <h2>Rename Store</h2>
    <label>New name</label>
    <input ref=${inputRef} placeholder="Enter a name" autofocus />
    <${ModalActions} onSubmit=${submit} label="Rename" />
  `;
}

function DeleteModal({ storeId }) {
  const inputRef = useRef(null);
  const uuid = uuidFromBytes(storeId);

  const submit = async () => {
    const parentPrefix = inputRef.current?.value?.trim();
    if (!parentPrefix) { toast('Parent UUID required', 'err'); return; }
    const allStores = stores.value;
    const parentStore = allStores.find(st => uuidFromBytes(st.id).startsWith(parentPrefix));
    if (!parentStore) { toast('Parent store not found', 'err'); return; }
    try {
      await sdk.api.store.Delete({ store_id: parentStore.id, child_id: storeId });
      closeModal();
      toast('Store archived', 'ok');
      navigate('/');
      await refreshStores();
    } catch (e) { toast('Delete error: ' + e.message, 'err'); }
  };

  return html`
    <h2>Delete Store</h2>
    <p>Are you sure you want to archive store ${uuid}?</p>
    <label>Parent store UUID (the store that owns this child)</label>
    <input ref=${inputRef} placeholder="Parent UUID or prefix" autofocus />
    <${ModalActions} onSubmit=${submit} label="Delete" danger />
  `;
}

function InviteModal({ token }) {
  return html`
    <h2>Invite Token</h2>
    <p>Share this token with a peer to invite them.</p>
    <label>Token</label>
    <input value=${token} readonly onClick=${(e) => e.target.select()} />
    <${ModalActions} />
  `;
}

function ExecModal({ name, storeId }) {
  const [schema, setSchema] = useState(null);
  const [loaded, setLoaded] = useState(false);

  useEffect(() => {
    (async () => {
      const s = await getSchema(storeId);
      setSchema(s);
      setLoaded(true);
    })();
  }, [storeId]);

  if (!loaded) return html`<h2>Execute: ${name}</h2><div class="muted">Loading...</div>`;

  if (!schema || !schema.service) {
    return html`<${ExecRawModal} name=${name} storeId=${storeId} />`;
  }

  const method = schema.service.methods[name];
  if (!method) {
    toast(`Method '${name}' not found in schema`, 'err');
    closeModal();
    return null;
  }

  const inputType = schema.root.lookupType(method.requestType);
  if (!inputType || inputType.fieldsArray.length === 0) {
    return html`<${ExecNoArgsModal} name=${name} storeId=${storeId} />`;
  }

  return html`<${ExecFieldsModal} name=${name} storeId=${storeId} inputType=${inputType} />`;
}

function ExecRawModal({ name, storeId }) {
  const inputRef = useRef(null);

  const submit = async () => {
    const hexStr = inputRef.current?.value?.trim() || '';
    let payloadBytes = new Uint8Array(0);
    if (hexStr) payloadBytes = bytesFromHex(hexStr);
    await runExecAndShow(storeId, name, payloadBytes);
  };

  return html`
    <h2>Execute: ${name}</h2>
    <label>Payload (raw bytes, hex-encoded)</label>
    <input ref=${inputRef} placeholder="hex bytes" autofocus />
    <${ModalActions} onSubmit=${submit} label="Execute" />
  `;
}

function ExecNoArgsModal({ name, storeId }) {
  const submit = () => runExecAndShow(storeId, name, new Uint8Array(0));

  return html`
    <h2>Execute: ${name}</h2>
    <p>This method takes no arguments.</p>
    <${ModalActions} onSubmit=${submit} label="Execute" />
  `;
}

function ExecFieldsModal({ name, storeId, inputType }) {
  const formRef = useRef(null);

  const submit = async () => {
    const values = collectFormFields(formRef);
    const payloadBytes = encodeMessage(inputType, values);
    await runExecAndShow(storeId, name, payloadBytes);
  };

  return html`
    <h2>Execute: ${name}</h2>
    <div ref=${formRef}>
      ${inputType.fieldsArray.map((field, i) =>
        html`<${FieldInput} field=${field} cssClass="exec-field" autofocus=${i === 0} />`
      )}
    </div>
    <${ModalActions} onSubmit=${submit} label="Execute" />
  `;
}

function SubscribeModal({ streamName, storeId, paramType, eventSchema }) {
  const formRef = useRef(null);

  const submit = () => {
    let params = new Uint8Array(0);
    if (paramType && formRef.current) {
      try {
        params = encodeMessage(paramType, collectFormFields(formRef));
      } catch (e) { /* encode failed */ }
    }
    closeModal();
    doSubscribe(storeId, streamName, params, eventSchema);
  };

  return html`
    <h2>Subscribe: ${streamName}</h2>
    <div ref=${formRef}>
      ${paramType ? paramType.fieldsArray.map((field, i) =>
        html`<${FieldInput} field=${field} cssClass="stream-field" autofocus=${i === 0} />`
      ) : null}
    </div>
    <${ModalActions} onSubmit=${submit} label="Subscribe" />
  `;
}

function InspectIntentionModal() {
  const inputRef = useRef(null);

  const submit = async () => {
    const hexStr = inputRef.current?.value?.trim();
    if (!hexStr) return;
    const hashBytes = bytesFromHex(hexStr);
    const currentStoreId = activeStoreId.value;
    closeModal();
    try {
      const resp = await sdk.api.store.GetIntention({ store_id: currentStoreId, hash_prefix: hashBytes });
      setPanelOverride({ type: 'intention', intention: resp.intention, ops: resp.intention.ops || [], hexStr });
    } catch (e) {
      setPanelOverride({ type: 'intention', intention: null, ops: [], hexStr, error: e.message });
    }
  };

  return html`
    <h2>Inspect Intention</h2>
    <label>Hash prefix (hex)</label>
    <input ref=${inputRef} placeholder="e.g. a1b2c3" autofocus />
    <${ModalActions} onSubmit=${submit} label="Inspect" />
  `;
}

// ============================================================================
// App Modals — Register + Unregister (all via gRPC)
// ============================================================================

function StoreOptions({ stores: storeList }) {
  return storeList.map(s => {
    const uuid = uuidFromBytes(s.id);
    const label = s.name ? s.name + ' (' + uuid.slice(0,8) + ')' : uuid;
    return html`<option value=${uuid}>${label}</option>`;
  });
}

function RegisterAppModal({ registryStoreId }) {
  const subdomainRef = useRef(null);
  const rootStores = getRootStores(stores.value);
  const firstRootUuid = rootStores.length > 0 ? uuidFromBytes(rootStores[0].id) : '';
  const [selectedRegId, setSelectedRegId] = useState(
    registryStoreId ? uuidFromBytes(registryStoreId) : firstRootUuid
  );
  
  const [selectedAppId, setSelectedAppId] = useState('');
  const [storeMode, setStoreMode] = useState('new');
  const [selectedStoreId, setSelectedStoreId] = useState('');
  const [bundles, setBundles] = useState([]);

  // Load bundles from selected root store via typed store methods
  useEffect(() => {
    if (!selectedRegId) { setBundles([]); return; }
    (async () => {
      const store = await sdk.openStore(selectedRegId);
      const resp = await store.ListBundles({});
      setBundles(resp.items || []);
    })();
  }, [selectedRegId]);

  // Filter stores to those compatible with the selected bundle's store_type.
  // No bundle selected → no stores shown. Missing store_type → no stores shown.
  const selectedBundle = bundles.find(b => b.app_id === selectedAppId);
  const bundleStoreType = manifestGet(selectedBundle?.manifest, 'app', 'store_type');
  const compatibleStores = bundleStoreType
    ? stores.value.filter(s => s.store_type === bundleStoreType)
    : [];

  const onRegChange = (e) => {
    setSelectedRegId(e.target.value);
    setSelectedAppId('');
  };

  const onAppChange = (e) => {
    const val = e.target.value;
    setSelectedAppId(val);
    if (val && subdomainRef.current && !subdomainRef.current.value) {
      // Strip any prefix (e.g. "example:kanban" → "kanban")
      subdomainRef.current.value = val.includes(':') ? val.split(':').pop() : val;
    }
  };

  const submit = async () => {
    if (!selectedRegId) { toast('Select a root store', 'err'); return; }
    const subdomain = subdomainRef.current?.value?.trim();
    if (!subdomain) { toast('Subdomain is required', 'err'); return; }
    if (!selectedAppId) { toast('Select an app type', 'err'); return; }
    if (storeMode === 'existing' && !selectedStoreId) { toast('Select a data store', 'err'); return; }

    try {
      let storeId;
      if (storeMode === 'existing') {
        storeId = uuidToBytes(selectedStoreId);
      } else {
        // Create a new child store matching the app's required store_type
        if (!bundleStoreType) { toast('Bundle has no store_type in manifest', 'err'); return; }
        const createResp = await sdk.api.store.Create({
          name: subdomain,
          store_type: bundleStoreType,
          parent_id: uuidToBytes(selectedRegId),
        });
        storeId = createResp.id;
      }
      const store = await sdk.openStore(selectedRegId);
      await store.RegisterApp({ subdomain, app_id: selectedAppId, store_id: storeId });
      closeModal();
      toast('App registered: ' + subdomain + '.' + location.hostname, 'ok');
      await refreshStores();
      await refreshApps();
    } catch (e) { toast('Register error: ' + e.message, 'err'); }
  };

  const regFixed = !!registryStoreId;

  return html`
    <h2>Create App</h2>
    ${regFixed ? html`
      <p class="muted small">in ${rootStores.find(s => uuidFromBytes(s.id) === selectedRegId)?.name || selectedRegId.slice(0, 8)}</p>
    ` : html`
      <label>Root Store</label>
      <select value=${selectedRegId} onChange=${onRegChange}>
        <option value="">Select a root store...</option>
        <${StoreOptions} stores=${rootStores} />
      </select>
    `}
    ${selectedRegId && bundles.length === 0 ? html`
      <p class="muted">No app bundles in this store. Drop a .zip or .lattice file onto the page to upload.</p>
      <${ModalActions} />
    ` : html`
      <label>App Type</label>
      <select value=${selectedAppId} onChange=${onAppChange}>
        <option value="">Select an app...</option>
        ${bundles.map(b => html`<option value=${b.app_id}>${manifestGet(b.manifest, 'app', 'name') || b.app_id}</option>`)}
      </select>
      <label>Subdomain</label>
      <input ref=${subdomainRef} placeholder="e.g. inventory" />
      <p class="muted small">Served at <em>subdomain</em>.${location.hostname}:${location.port}</p>
      <label>Data Store</label>
      <div class="radio-group">
        <label class="radio-label">
          <input type="radio" name="store-mode" value="new"
            checked=${storeMode === 'new'} onChange=${() => setStoreMode('new')} />
          Create new child store
        </label>
        <label class="radio-label">
          <input type="radio" name="store-mode" value="existing"
            checked=${storeMode === 'existing'} onChange=${() => setStoreMode('existing')} />
          Use existing store
        </label>
      </div>
      ${storeMode === 'existing' && html`
        <select value=${selectedStoreId} onChange=${(e) => setSelectedStoreId(e.target.value)}>
          <option value="">Select a store...</option>
          <${StoreOptions} stores=${compatibleStores} />
        </select>
      `}
      <${ModalActions} onSubmit=${submit} label="Create App" />
    `}
  `;
}

function SelectRootStoreModal({ file }) {
  const rootStores = getRootStores(stores.value);
  const [selected, setSelected] = useState('');

  const submit = async () => {
    const match = rootStores.find(s => uuidFromBytes(s.id) === selected);
    if (!match) { toast('Select a root store', 'err'); return; }
    closeModal();
    await uploadBundleToStore(match.id, file);
  };

  return html`
    <h2>Select Root Store</h2>
    <p>Multiple root stores found. Which one should receive this bundle?</p>
    <select value=${selected} onChange=${(e) => setSelected(e.target.value)}>
      <option value="">Select a root store...</option>
      <${StoreOptions} stores=${rootStores} />
    </select>
    <${ModalActions} onSubmit=${submit} label="Upload" />
  `;
}

function UnregisterAppModal({ subdomain, registryStoreId }) {
  const submit = async () => {
    try {
      const store = await sdk.openStore(uuidFromBytes(registryStoreId));
      await store.RemoveApp({ subdomain });
      closeModal();
      toast('App removed: ' + subdomain, 'ok');
      await refreshApps();
    } catch (e) { toast('Remove error: ' + e.message, 'err'); }
  };

  return html`
    <h2>Unregister App</h2>
    <p>Remove <strong>${subdomain}</strong> from the mesh? This deletes the app definition.</p>
    <p class="muted">Data stores are not affected.</p>
    <${ModalActions} onSubmit=${submit} label="Unregister" danger />
  `;
}
