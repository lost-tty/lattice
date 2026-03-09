async function loadMethods(storeId) {
  const methods = (await API.dynamic.ListMethods({ id: storeId })).methods || [];
  const schema = await Schema.getSchema(storeId);
  return html`<${MethodsView} methods=${methods} schema=${schema} storeId=${storeId} />`;
}

function MethodsView({ methods, schema, storeId }) {
  if (methods.length === 0) {
    return html`<div class="empty-state">No methods available</div>`;
  }

  const commands = methods.filter(m => m.kind === 0);
  const queries = methods.filter(m => m.kind === 1);

  return html`
    ${commands.length > 0 ? html`
      <h3 style="margin-bottom:0.6rem">Commands</h3>
      ${commands.map(m => html`<${MethodCard} method=${m} schema=${schema} storeId=${storeId} />`)}
    ` : null}
    ${queries.length > 0 ? html`
      <h3 style="margin:${commands.length > 0 ? '1.2rem' : '0'} 0 0.6rem">Queries</h3>
      ${queries.map(m => html`<${MethodCard} method=${m} schema=${schema} storeId=${storeId} />`)}
    ` : null}
  `;
}

function MethodCard({ method, schema, storeId }) {
  let params = null;
  if (schema?.service) {
    const md = schema.service.methods[method.name];
    if (md) {
      const inputType = schema.root.lookupType(md.requestType);
      if (inputType && inputType.fieldsArray.length > 0) {
        params = inputType.fieldsArray.map(f => ({
          name: f.name, typeName: Schema.fieldTypeName(f),
        }));
      }
    }
  }

  return html`
    <div class="card" style="padding:16px 20px">
      <div style="display:flex;align-items:center;justify-content:space-between">
        <div>
          <span class="mono" style="font-weight:500">${method.name}</span>
          <span class="muted" style="margin-left:12px">${method.description}</span>
        </div>
        <button class="btn btn-primary" onClick=${() => S.showModal('exec', { name: method.name, storeId })}>Execute</button>
      </div>
      ${params ? html`
        <div style="margin-top:8px;display:flex;gap:8px;flex-wrap:wrap">
          ${params.map(p => html`
            <span class="badge badge-blue">${p.name}</span>
            <span class="muted" style="font-size:12px">${p.typeName}</span>
          `)}
        </div>
      ` : null}
    </div>
  `;
}
