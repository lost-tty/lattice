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
      <h3 class="section-heading">Commands</h3>
      ${commands.map(m => html`<${MethodCard} method=${m} schema=${schema} storeId=${storeId} />`)}
    ` : null}
    ${queries.length > 0 ? html`
      <h3 class="section-heading${commands.length > 0 ? ' mt-section' : ''}">Queries</h3>
      ${queries.map(m => html`<${MethodCard} method=${m} schema=${schema} storeId=${storeId} />`)}
    ` : null}
  `;
}

function MethodCard({ method, schema, storeId }) {
  let params = null;
  if (schema?.service) {
    const md = schema.service.methods[method.name];
    if (md) params = Schema.describeFields(schema.root, md.requestType);
  }

  return html`
    <div class="card card-compact">
      <div class="card-row">
        <div>
          <span class="mono mono-medium">${method.name}</span>
          <span class="muted ml-desc">${method.description}</span>
        </div>
        <button class="btn btn-primary" onClick=${() => S.showModal('exec', { name: method.name, storeId })}>Execute</button>
      </div>
      ${params ? html`
        <div class="param-list">
          ${params.map(p => html`
            <span class="badge badge-blue">${p.name}</span>
            <span class="muted param-type">${p.typeName}</span>
          `)}
        </div>
      ` : null}
    </div>
  `;
}
