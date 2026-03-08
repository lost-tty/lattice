async function loadSystem(storeId) {
  const entries = (await API.store.SystemList({ id: storeId })).entries || [];
  if (entries.length === 0) {
    return html`<div class="empty-state">No system entries</div>`;
  }
  return html`
    <table>
      <tr><th>Key</th><th>Value</th></tr>
      ${entries.map(e => html`
        <tr>
          <td class="mono">${e.key}</td>
          <td class="mono">${hex(e.value)}</td>
        </tr>
      `)}
    </table>
  `;
}
