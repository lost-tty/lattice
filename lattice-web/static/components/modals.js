function ModalContainer() {
  const modal = S.modal.value;
  if (!modal) return html`<div id="modal-container"></div>`;

  return html`
    <div id="modal-container">
      <div class="modal-overlay" onClick=${(e) => { if (e.target === e.currentTarget) S.closeModal(); }}>
        <div class="modal">
          <${ModalContent} type=${modal.type} props=${modal.props} />
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
    default: return null;
  }
}

function CreateStoreModal({ parentId }) {
  const [types, setTypes] = useState(null);
  const nameRef = useRef(null);
  const typeRef = useRef(null);
  const isChild = !!parentId;

  useEffect(() => {
    (async () => {
      try {
        const t = (await API.node.ListStoreTypes({})).store_types || [];
        setTypes(t.length > 0 ? t : ['core:kvstore', 'core:logstore']);
      } catch (e) {
        setTypes(['core:kvstore', 'core:logstore']);
      }
    })();
  }, []);

  const submit = async () => {
    const name = nameRef.current?.value || '';
    const type_ = typeRef.current?.value;
    const req = { name, store_type: type_ };
    if (parentId) req.parent_id = parentId;
    try {
      await API.store.Create(req);
      S.closeModal();
      S.toast(`Store created: ${name || '(unnamed)'} [${type_}]`, 'ok');
      await S.refresh();
    } catch (e) { S.toast('Create error: ' + e.message, 'err'); }
  };

  const title = isChild ? 'Create Child Store' : 'Create Root Store';
  if (!types) return html`<h2>${title}</h2><div class="muted">Loading...</div>`;

  return html`
    <h2>${title}</h2>
    ${isChild ? html`<p>Parent: <span class="mono">${Helpers.uuidFromBytes(parentId)}</span></p>` : null}
    <label>Name (optional)</label>
    <input ref=${nameRef} placeholder="my-store" autofocus />
    <label>Type</label>
    <select ref=${typeRef}>
      ${types.map(t => html`<option value=${t}>${t}</option>`)}
    </select>
    <div class="modal-actions">
      <button class="btn" onClick=${S.closeModal}>Cancel</button>
      <button class="btn btn-primary" onClick=${submit}>Create</button>
    </div>
  `;
}

function JoinStoreModal() {
  const inputRef = useRef(null);

  const submit = async () => {
    const token = inputRef.current?.value?.trim();
    if (!token) return;
    S.closeModal();
    try {
      const resp = await API.store.Join({ token });
      const uuid = resp.store_id ? Helpers.uuidFromBytes(resp.store_id) : '';
      S.toast(`Join initiated${uuid ? ': ' + uuid : ''}`, 'ok');
      await S.refresh();
    } catch (e) { S.toast('Join error: ' + e.message, 'err'); }
  };

  return html`
    <h2>Join Store</h2>
    <label>Invite token</label>
    <input ref=${inputRef} placeholder="Paste invite token" autofocus />
    <div class="modal-actions">
      <button class="btn" onClick=${S.closeModal}>Cancel</button>
      <button class="btn btn-primary" onClick=${submit}>Join</button>
    </div>
  `;
}

function RenameModal({ storeId }) {
  const inputRef = useRef(null);

  const submit = async () => {
    const name = inputRef.current?.value;
    if (!name) return;
    try {
      await API.store.SetName({ store_id: storeId, name });
      S.closeModal();
      S.toast('Store renamed to: ' + name, 'ok');
      await S.refresh();
    } catch (e) { S.toast('Rename error: ' + e.message, 'err'); }
  };

  return html`
    <h2>Rename Store</h2>
    <label>New name</label>
    <input ref=${inputRef} placeholder="Enter a name" autofocus />
    <div class="modal-actions">
      <button class="btn" onClick=${S.closeModal}>Cancel</button>
      <button class="btn btn-primary" onClick=${submit}>Rename</button>
    </div>
  `;
}

function DeleteModal({ storeId }) {
  const inputRef = useRef(null);
  const uuid = Helpers.uuidFromBytes(storeId);

  const submit = async () => {
    const parentPrefix = inputRef.current?.value?.trim();
    if (!parentPrefix) { S.toast('Parent UUID required', 'err'); return; }
    const stores = S.stores.value;
    const parentStore = stores.find(st => Helpers.uuidFromBytes(st.id).startsWith(parentPrefix));
    if (!parentStore) { S.toast('Parent store not found', 'err'); return; }
    try {
      await API.store.Delete({ store_id: parentStore.id, child_id: storeId });
      S.closeModal();
      S.toast('Store archived', 'ok');
      S.activeStoreId.value = null;
      await S.refresh();
    } catch (e) { S.toast('Delete error: ' + e.message, 'err'); }
  };

  return html`
    <h2>Delete Store</h2>
    <p>Are you sure you want to archive store ${uuid}?</p>
    <label>Parent store UUID (the store that owns this child)</label>
    <input ref=${inputRef} placeholder="Parent UUID or prefix" autofocus />
    <div class="modal-actions">
      <button class="btn" onClick=${S.closeModal}>Cancel</button>
      <button class="btn btn-danger" onClick=${submit}>Delete</button>
    </div>
  `;
}

function InviteModal({ token }) {
  return html`
    <h2>Invite Token</h2>
    <p>Share this token with a peer to invite them.</p>
    <label>Token</label>
    <input value=${token} readonly onClick=${(e) => e.target.select()} />
    <div class="modal-actions">
      <button class="btn" onClick=${S.closeModal}>Close</button>
    </div>
  `;
}

function ExecModal({ name, storeId }) {
  const [schema, setSchema] = useState(null);
  const [loaded, setLoaded] = useState(false);

  useEffect(() => {
    (async () => {
      const s = await Schema.getSchema(storeId);
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
    S.toast(`Method '${name}' not found in schema`, 'err');
    S.closeModal();
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
    if (hexStr) payloadBytes = Helpers.bytesFromHex(hexStr);
    S.closeModal();
    try {
      const result = await execStore(storeId, name, payloadBytes);
      await showExecResult(name, result, storeId);
    } catch (e) { S.setPanelOverride({ type: 'execError', method: name, message: e.message }); }
  };

  return html`
    <h2>Execute: ${name}</h2>
    <label>Payload (raw bytes, hex-encoded)</label>
    <input ref=${inputRef} placeholder="hex bytes" autofocus />
    <div class="modal-actions">
      <button class="btn" onClick=${S.closeModal}>Cancel</button>
      <button class="btn btn-primary" onClick=${submit}>Execute</button>
    </div>
  `;
}

function ExecNoArgsModal({ name, storeId }) {
  const submit = async () => {
    S.closeModal();
    try {
      const result = await execStore(storeId, name, new Uint8Array(0));
      await showExecResult(name, result, storeId);
    } catch (e) { S.setPanelOverride({ type: 'execError', method: name, message: e.message }); }
  };

  return html`
    <h2>Execute: ${name}</h2>
    <p>This method takes no arguments.</p>
    <div class="modal-actions">
      <button class="btn" onClick=${S.closeModal}>Cancel</button>
      <button class="btn btn-primary" onClick=${submit}>Execute</button>
    </div>
  `;
}

function ExecFieldsModal({ name, storeId, inputType }) {
  const formRef = useRef(null);

  const submit = async () => {
    const values = {};
    if (formRef.current) {
      for (const el of formRef.current.querySelectorAll('[data-field]')) {
        const val = el.value;
        if (val !== '' && val !== undefined) values[el.dataset.field] = val;
      }
    }
    const payloadBytes = Schema.encodeMessage(inputType, values);
    S.closeModal();
    try {
      const result = await execStore(storeId, name, payloadBytes);
      await showExecResult(name, result, storeId);
    } catch (e) { S.setPanelOverride({ type: 'execError', method: name, message: e.message }); }
  };

  return html`
    <h2>Execute: ${name}</h2>
    <div ref=${formRef}>
      ${inputType.fieldsArray.map((field, i) =>
        html`<${FieldInput} field=${field} cssClass="exec-field" autofocus=${i === 0} />`
      )}
    </div>
    <div class="modal-actions">
      <button class="btn" onClick=${S.closeModal}>Cancel</button>
      <button class="btn btn-primary" onClick=${submit}>Execute</button>
    </div>
  `;
}

function SubscribeModal({ streamName, storeId, paramType, eventSchema }) {
  const formRef = useRef(null);

  const submit = () => {
    let params = new Uint8Array(0);
    if (paramType && formRef.current) {
      try {
        const values = {};
        for (const el of formRef.current.querySelectorAll('[data-field]')) {
          const val = el.value;
          if (val !== '' && val !== undefined) values[el.dataset.field] = val;
        }
        params = Schema.encodeMessage(paramType, values);
      } catch (e) { /* encode failed */ }
    }
    S.closeModal();
    doSubscribe(storeId, streamName, params, eventSchema);
  };

  return html`
    <h2>Subscribe: ${streamName}</h2>
    <div ref=${formRef}>
      ${paramType ? paramType.fieldsArray.map((field, i) =>
        html`<${FieldInput} field=${field} cssClass="stream-field" autofocus=${i === 0} />`
      ) : null}
    </div>
    <div class="modal-actions">
      <button class="btn" onClick=${S.closeModal}>Cancel</button>
      <button class="btn btn-primary" onClick=${submit}>Subscribe</button>
    </div>
  `;
}

function InspectIntentionModal() {
  const inputRef = useRef(null);

  const submit = async () => {
    const hexStr = inputRef.current?.value?.trim();
    if (!hexStr) return;
    const hashBytes = Helpers.bytesFromHex(hexStr);
    const activeStoreId = S.activeStoreId.value;
    S.closeModal();
    try {
      const resp = await API.store.GetIntention({ store_id: activeStoreId, hash_prefix: hashBytes });
      S.setPanelOverride({ type: 'intention', intention: resp.intention, ops: resp.ops || [], hexStr });
    } catch (e) {
      S.setPanelOverride({ type: 'intention', intention: null, ops: [], hexStr, error: e.message });
    }
  };

  return html`
    <h2>Inspect Intention</h2>
    <label>Hash prefix (hex)</label>
    <input ref=${inputRef} placeholder="e.g. a1b2c3" autofocus />
    <div class="modal-actions">
      <button class="btn" onClick=${S.closeModal}>Cancel</button>
      <button class="btn btn-primary" onClick=${submit}>Inspect</button>
    </div>
  `;
}
