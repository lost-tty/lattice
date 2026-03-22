// Bootstrap — SDK connect + Preact mount.

import { html, render } from './vendor/preact.mjs';
import { setStatus, connect } from './state.js';
import { LatticeSDK, setSdk } from './sdk.js';
import { App } from './components/app.js';

(async function bootstrap() {
  try {
    const instance = await LatticeSDK.connect({
      extraProtos: ['/proto/weaver.bin'],
      onStatus(status) {
        setStatus(
          status === 'connected' ? 'ok' : 'err',
          status.charAt(0).toUpperCase() + status.slice(1),
        );
      },
    });

    setSdk(instance);
    render(html`<${App} />`, document.getElementById('app'));
    await connect();
  } catch (e) {
    console.error('[boot] FATAL:', e);
    const el = document.getElementById('app');
    el.textContent = '';
    const card = document.createElement('div');
    card.className = 'card boot-error';
    const p1 = document.createElement('p');
    p1.className = 'text-error';
    p1.textContent = 'Failed to initialize: ' + e.message;
    const p2 = document.createElement('p');
    p2.textContent = 'Try refreshing the page.';
    card.appendChild(p1);
    card.appendChild(p2);
    el.appendChild(card);
  }
})();
