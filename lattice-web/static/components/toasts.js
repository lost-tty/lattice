import { toasts } from '../state.js';
import { html } from './util.js';

export function Toasts() {
  const t = toasts.value;
  return html`
    <div id="toasts">
      ${t.map(t => html`
        <div key=${t.id} class="toast ${t.cls}">${t.text}</div>
      `)}
    </div>
  `;
}
