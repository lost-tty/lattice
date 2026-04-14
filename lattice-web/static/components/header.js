import { status, toggleSidebar, sidebarOpen } from '../state.js';
import { html, Icon } from './util.js';

export function Header() {
  const s = status.value;
  const open = sidebarOpen.value;
  return html`
    <header>
      <button
        class="sidebar-toggle btn-icon"
        aria-label=${open ? 'Close menu' : 'Open menu'}
        aria-expanded=${open}
        onClick=${toggleSidebar}
      ><${Icon} name=${open ? 'close' : 'menu'} size=${20} /></button>
      <h1>Lattice</h1>
      <div id="status">
        <span class="dot ${s.state}"></span>${s.text}
      </div>
    </header>
  `;
}
