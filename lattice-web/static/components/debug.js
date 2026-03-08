async function loadDebug(storeId, debugView) {
  switch (debugView) {
    case 'tips': return await loadDebugTips(storeId);
    case 'log': return await loadDebugLog(storeId);
    case 'intentions': return await loadDebugIntentions(storeId);
    case 'floating': return await loadDebugFloating(storeId);
  }
}

function DebugNav() {
  const dv = S.debugView.value;
  const views = ['tips', 'log', 'intentions', 'floating'];
  return html`
    <div class="action-bar">
      ${views.map(v => html`
        <button
          class="btn${v === dv ? ' btn-primary' : ''}"
          onClick=${() => S.switchDebugView(v)}
        >${v}</button>
      `)}
      <button class="btn" onClick=${() => S.showModal('inspectIntention')}>inspect</button>
    </div>
  `;
}

async function loadDebugTips(storeId) {
  const authors = (await API.store.Debug({ id: storeId })).authors || [];
  return html`
    <${DebugNav} />
    ${authors.length === 0
      ? html`<div class="empty-state">No author state</div>`
      : html`
        <table>
          <tr><th>Author</th><th>Tip</th></tr>
          ${authors.map(a => html`
            <tr>
              <td class="mono">${Helpers.pubkeyShort(a.public_key)}</td>
              <td class="mono">${hex(a.hash) || '-'}</td>
            </tr>
          `)}
        </table>
      `
    }
  `;
}

async function loadDebugLog(storeId) {
  const entries = (await API.store.WitnessLog({ store_id: storeId })).entries || [];
  return html`
    <${DebugNav} />
    ${entries.length === 0
      ? html`<div class="empty-state">No witness log entries</div>`
      : html`
        <table>
          <tr><th>Seq</th><th>Hash</th><th>Intention</th><th>Wall Time</th><th>Prev</th><th>Sig</th></tr>
          ${entries.map(w => {
            let intentionHash = '-', wallTime = '-', prevHash = '-';
            if (w.content && w.content.length > 0) {
              try {
                const wc = T.WitnessContent.decode(w.content);
                intentionHash = hex(wc.intention_hash) || '-';
                wallTime = wc.wall_time ? fmtTime(wc.wall_time) : '-';
                prevHash = hex(wc.prev_hash) || '-';
              } catch (e) { /* decode failed */ }
            }
            return html`
              <tr>
                <td>${w.seq}</td>
                <td class="mono">${hex(w.hash) || '-'}</td>
                <td class="mono">${intentionHash}</td>
                <td>${wallTime}</td>
                <td class="mono">${prevHash}</td>
                <td class="mono">${hex(w.signature) || '-'}</td>
              </tr>
            `;
          })}
        </table>
      `
    }
  `;
}

async function loadDebugIntentions(storeId) {
  const entries = (await API.store.WitnessLog({ store_id: storeId })).entries || [];
  if (entries.length === 0) {
    return html`<${DebugNav} /><div class="empty-state">No intentions</div>`;
  }

  const intentionHashes = [];
  for (const w of entries) {
    if (w.content && w.content.length > 0) {
      try {
        const wc = T.WitnessContent.decode(w.content);
        if (wc.intention_hash && wc.intention_hash.length > 0) {
          const h = hex(wc.intention_hash);
          if (!intentionHashes.includes(h)) intentionHashes.push(h);
        }
      } catch (e) { /* skip */ }
    }
  }

  if (intentionHashes.length === 0) {
    return html`<${DebugNav} /><div class="empty-state">No intention hashes found in witness log</div>`;
  }

  const rows = [];
  for (const h of intentionHashes) {
    try {
      const resp = await API.store.GetIntention({ store_id: storeId, hash_prefix: Helpers.bytesFromHex(h.slice(0, 8)) });
      const i = resp.intention;
      if (!i) continue;
      rows.push(i);
    } catch (e) {
      rows.push({ _error: e.message, _hash: h });
    }
  }

  return html`
    <${DebugNav} />
    <table>
      <tr><th>Hash</th><th>Author</th><th>Store Prev</th><th>Wall Time</th><th>Ops</th></tr>
      ${rows.map(i => i._error
        ? html`<tr><td class="mono">${i._hash.slice(0, 16)}...</td><td colspan="4" class="muted">${i._error}</td></tr>`
        : html`
          <tr>
            <td class="mono">${hex(i.hash)}</td>
            <td class="mono">${hex(i.author)}</td>
            <td class="mono">${hex(i.store_prev) || '-'}</td>
            <td>${i.timestamp ? fmtTime(i.timestamp.wall_time) : '-'}</td>
            <td class="mono">${i.ops ? i.ops.length + 'b' : '-'}</td>
          </tr>
        `
      )}
    </table>
  `;
}

async function loadDebugFloating(storeId) {
  const floating = (await API.store.FloatingIntentions({ id: storeId })).intentions || [];
  return html`
    <${DebugNav} />
    ${floating.length === 0
      ? html`<div class="empty-state">No floating intentions</div>`
      : html`
        <table>
          <tr><th>Hash</th><th>Author</th><th>Store Prev</th><th>Wall Time</th><th>Received</th></tr>
          ${floating.map(f => {
            const i = f.intention || {};
            const ts = i.timestamp;
            return html`
              <tr>
                <td class="mono">${hex(i.hash) || '-'}</td>
                <td class="mono">${hex(i.author) || '-'}</td>
                <td class="mono">${hex(i.store_prev) || '-'}</td>
                <td>${ts ? fmtTime(ts.wall_time) : '-'}</td>
                <td>${f.received_at ? fmtTime(f.received_at) : '-'}</td>
              </tr>
            `;
          })}
        </table>
        <div class="muted" style="margin-top:8px">${floating.length} floating intention${floating.length !== 1 ? 's' : ''}</div>
      `
    }
  `;
}

function IntentionDetail({ intention, hexStr, error }) {
  if (error) {
    return html`
      <div class="action-bar"><button class="btn" onClick=${() => S.clearPanelOverride()}>Back</button></div>
      <div class="card"><p class="err">${error}</p></div>
    `;
  }
  if (!intention) {
    return html`
      <div class="action-bar"><button class="btn" onClick=${() => S.clearPanelOverride()}>Back</button></div>
      <div class="card"><p class="err">No intention found matching '${hexStr}'</p></div>
    `;
  }

  const i = intention;
  const condHashes = i.condition?.v1?.hashes || [];
  const condStr = condHashes.length > 0 ? condHashes.map(h => hex(h)).join(', ') : '-';
  const ts = i.timestamp;

  return html`
    <div class="action-bar"><button class="btn" onClick=${() => S.clearPanelOverride()}>Back</button></div>
    <div class="card">
      <h3>Intention</h3>
      <div class="kv">
        <div class="k">Hash</div><div class="v mono">${hex(i.hash)}</div>
        <div class="k">Author</div><div class="v mono">${hex(i.author)}</div>
        <div class="k">Wall Time</div><div class="v">${ts ? fmtTime(ts.wall_time) : '-'}</div>
        <div class="k">Store Prev</div><div class="v mono">${hex(i.store_prev) || '-'}</div>
        <div class="k">Condition</div><div class="v mono">${condStr}</div>
        <div class="k">Signature</div><div class="v mono">${hex(i.signature) || '-'}</div>
        <div class="k">Ops Payload</div><div class="v mono">${hex(i.ops) || '-'}</div>
      </div>
    </div>
  `;
}
