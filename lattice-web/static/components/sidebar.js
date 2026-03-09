function NodeName({ name }) {
  const [editing, setEditing] = useState(false);
  const inputRef = useRef(null);

  const save = async () => {
    const val = inputRef.current?.value?.trim();
    setEditing(false);
    if (!val || val === name) return;
    try {
      await API.node.SetName({ name: val });
      S.toast('Node renamed', 'ok');
      await S.refresh();
    } catch (e) { S.toast('Rename error: ' + e.message, 'err'); }
  };

  if (editing) {
    return html`<input ref=${inputRef} class="inline-edit"
      value=${name || ''} autofocus
      onKeyDown=${(e) => { if (e.key === 'Enter') save(); if (e.key === 'Escape') setEditing(false); }}
      onBlur=${save}
    />`;
  }

  return html`<span class="editable" onClick=${() => setEditing(true)}>${name || '(unnamed)'}</span>`;
}

function Sidebar() {
  const nodeStatus = S.nodeStatus.value;
  const stores = S.stores.value;
  const activeStoreId = S.activeStoreId.value;
  const activeUuid = activeStoreId ? Helpers.uuidFromBytes(activeStoreId) : null;

  return html`
    <nav>
      <div id="node-info">
        <div class="label">Node</div>
        <div class="value"><${NodeName} name=${nodeStatus?.display_name} /></div>
        <div class="label">ID</div>
        <div class="value mono">${nodeStatus ? Helpers.pubkeyShort(nodeStatus.public_key) : '-'}</div>
      </div>
      <div class="label" style="padding:0.8rem 1rem 0.3rem">Stores</div>
      <div id="store-list">
        ${stores.map(s => {
          const uuid = Helpers.uuidFromBytes(s.id);
          const isActive = uuid === activeUuid;
          const depth = s.depth || 0;
          return html`
            <div
              class="store-item ${isActive ? 'active' : ''} ${depth > 0 ? 'nested' : ''}"
              style="margin-left:${depth * 16}px"
              onClick=${() => S.selectStore(uuid)}
            >
              <span class="name">${s.name || uuid}</span>
              <span class="meta">${s.store_type}${s.archived ? ' (archived)' : ''}</span>
            </div>
          `;
        })}
      </div>
      <footer>
        <button onClick=${() => S.showModal('createStore')}>New Store</button>
        <button onClick=${() => S.showModal('joinStore')}>Join Store</button>
      </footer>
    </nav>
  `;
}
