function Toasts() {
  const toasts = S.toasts.value;
  return html`
    <div id="toasts">
      ${toasts.map(t => html`
        <div key=${t.id} class="toast ${t.cls}">${t.text}</div>
      `)}
    </div>
  `;
}
