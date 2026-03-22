import { status } from '../state.js';
import { html } from './util.js';

export function Header() {
  const s = status.value;
  return html`
    <header>
      <h1>Lattice</h1>
      <div id="status">
        <span class="dot ${s.state}"></span>${s.text}
      </div>
    </header>
  `;
}
