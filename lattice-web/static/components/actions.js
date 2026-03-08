// Imperative actions triggered by UI components.
// These call API stubs and update state via signals.

async function doSync(storeId) {
  try {
    await API.store.Sync({ id: storeId });
    S.toast('Sync initiated', 'ok');
  } catch (e) { S.toast('Sync error: ' + e.message, 'err'); }
}

async function doInvite(storeId) {
  try {
    const resp = await API.store.Invite({ id: storeId });
    S.showModal('invite', { token: resp.token });
  } catch (e) { S.toast('Invite error: ' + e.message, 'err'); }
}

async function doRevokePeer(storeId, publicKey) {
  try {
    await API.store.RevokePeer({ store_id: storeId, peer_key: publicKey });
    S.toast('Peer revoked', 'ok');
    await S.refresh();
  } catch (e) { S.toast('Revoke error: ' + e.message, 'err'); }
}

async function showSubscribeStream(storeId, stream) {
  const schema = await Schema.getSchema(storeId);
  let paramType = null;
  if (stream.param_schema && schema?.root) {
    try { paramType = schema.root.lookupType(stream.param_schema); } catch (e) {}
  }

  if (!paramType || paramType.fieldsArray.length === 0) {
    doSubscribe(storeId, stream.name, new Uint8Array(0), stream.event_schema);
    return;
  }

  S.showModal('subscribe', {
    streamName: stream.name,
    storeId,
    paramType,
    eventSchema: stream.event_schema || '',
  });
}

function doSubscribe(storeId, streamName, params, eventSchemaName) {
  const subId = S.getNextSubId();
  const payload = buildSubscribePayload(storeId, streamName, params);
  const sub = { storeId, streamName, eventSchemaName, events: [], rpcId: null };
  const StoreEvent = T.StoreEvent;

  sub.rpcId = RPC.subscribe('dynamic', 'Subscribe', payload, async (eventData) => {
    const storeEvent = StoreEvent.decode(eventData);
    const eventPayload = storeEvent.payload;

    let display = '';
    try {
      const schema = await Schema.getSchema(storeId);
      if (eventSchemaName && schema?.root) {
        const eventType = schema.root.lookupType(eventSchemaName);
        const obj = Schema.decodeMessage(eventType, eventPayload);
        const fields = Schema.formatFields(eventType, obj);
        display = fields.map(fld => `${fld.name}: ${fmtFieldValue(fld)}`).join('  ');
      }
    } catch (e) { /* fall through */ }

    if (!display) {
      const raw = eventPayload instanceof Uint8Array ? eventPayload : new Uint8Array(0);
      const text = new TextDecoder().decode(raw);
      display = /^[\x20-\x7e\n\r\t]*$/.test(text) && text.length > 0 ? text : hex(raw);
    }

    const events = [...(S.subscriptions.value.get(subId)?.events || []), { time: Date.now(), display }];
    if (events.length > 200) events.shift();
    S.updateSubscription(subId, { events });
  }, () => {
    S.toast(`Stream '${streamName}' ended`, 'err');
    S.removeSubscription(subId);
  });

  S.addSubscription(subId, sub);
  S.toast(`Subscribed to '${streamName}'`, 'ok');
}

function clearSubEvents(subId) {
  S.updateSubscription(subId, { events: [] });
}

function doUnsubscribe(subId) {
  const sub = S.subscriptions.value.get(subId);
  if (!sub) return;
  if (sub.rpcId != null) RPC.unsubscribe(sub.rpcId);
  S.removeSubscription(subId);
  S.toast(`Unsubscribed from '${sub.streamName}'`, 'ok');
}

// Exec result display — generates HTML for the panel override.
async function showExecResult(method, resultBytes, storeId) {
  const schema = await Schema.getSchema(storeId);
  const methodDesc = schema?.service?.methods[method];
  let body = '';

  if (methodDesc) {
    try {
      const outputType = schema.root.lookupType(methodDesc.responseType);
      const obj = Schema.decodeMessage(outputType, resultBytes);
      const fields = Schema.formatFields(outputType, obj);
      if (fields.length > 0) {
        body = tryRenderTable(fields) || '';
        if (!body) {
          body = '<div class="kv">' + fields.map(f => {
            return `<div class="k">${f.name}</div><div class="v mono">${fmtFieldValue(f)}</div>`;
          }).join('') + '</div>';
        }
      }
    } catch (e) { /* decode failed */ }
  }

  if (!body) {
    if (!resultBytes || resultBytes.length === 0) {
      body = '<p class="muted">Empty response</p>';
    } else {
      const text = new TextDecoder().decode(resultBytes);
      if (/^[\x20-\x7e\n\r\t]*$/.test(text) && text.length > 0) {
        body = `<pre class="mono">${text}</pre>`;
      } else {
        body = `<pre class="mono">${hex(resultBytes)}</pre>`;
      }
    }
  }

  S.setPanelOverride({ type: 'execResult', method, body });
}
