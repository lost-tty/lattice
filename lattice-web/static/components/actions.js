// Imperative actions triggered by UI components.
// These call SDK stubs and update state via signals.

import {
  toast, showModal, closeModal, setPanelOverride,
  getNextSubId, addSubscription, removeSubscription, updateSubscription,
  subscriptions,
} from '../state.js';
import { getSchema, formatFields, decodeMessage } from '../schema.js';
import { uuidFromBytes, displayBytes } from '../helpers.js';
import { html, tryRenderTable, fmtFieldValue, hex } from './util.js';
import { sdk } from '../sdk.js';

export async function doSync(storeId) {
  try {
    await sdk.api.store.Sync({ store_id: storeId });
    toast('Sync initiated', 'ok');
  } catch (e) { toast('Sync error: ' + e.message, 'err'); }
}

export async function doInvite(storeId) {
  try {
    const resp = await sdk.api.store.Invite({ store_id: storeId });
    showModal('invite', { token: resp.token });
  } catch (e) { toast('Invite error: ' + e.message, 'err'); }
}

export async function doRevokePeer(storeId, publicKey) {
  try {
    await sdk.api.store.RevokePeer({ store_id: storeId, peer_key: publicKey });
    toast('Peer revoked', 'ok');
  } catch (e) { toast('Revoke error: ' + e.message, 'err'); }
}

export async function showSubscribeStream(storeId, stream) {
  const schema = await getSchema(storeId);
  let paramType = null;
  if (stream.param_schema && schema?.root) {
    try { paramType = schema.root.lookupType(stream.param_schema); } catch (e) {}
  }

  if (!paramType || paramType.fieldsArray.length === 0) {
    doSubscribe(storeId, stream.name, new Uint8Array(0), stream.event_schema);
    return;
  }

  showModal('subscribe', {
    streamName: stream.name,
    storeId,
    paramType,
    eventSchema: stream.event_schema || '',
  });
}

export function doSubscribe(storeId, streamName, params, eventSchemaName) {
  const subId = getNextSubId();
  const SubscribeRequest = sdk.proto.lookup('lattice.daemon.v1.SubscribeRequest');
  const StoreEvent = sdk.proto.lookup('lattice.daemon.v1.StoreEvent');

  const payload = SubscribeRequest.encode(SubscribeRequest.create({
    store_id: storeId, stream_name: streamName, params: params || new Uint8Array(0),
  })).finish();

  const sub = { storeId, streamName, eventSchemaName, events: [], rpcId: null };

  sub.rpcId = sdk.rpc.subscribe('dynamic', 'Subscribe', payload, async function (eventData) {
    const storeEvent = StoreEvent.decode(eventData);
    const eventPayload = storeEvent.payload;

    let display = '';
    try {
      const schema = await getSchema(storeId);
      if (eventSchemaName && schema?.root) {
        const eventType = schema.root.lookupType(eventSchemaName);
        const obj = decodeMessage(eventType, eventPayload);
        const fields = formatFields(eventType, obj);
        display = fields.map(function (fld) { return fld.name + ': ' + fmtFieldValue(fld); }).join('  ');
      }
    } catch (e) { /* fall through */ }

    if (!display) {
      const raw = eventPayload instanceof Uint8Array ? eventPayload : new Uint8Array(0);
      display = displayBytes(raw) || hex(raw);
    }

    const events = [].concat(subscriptions.value.get(subId)?.events || [], [{ time: Date.now(), display: display }]);
    if (events.length > 200) events.shift();
    updateSubscription(subId, { events: events });
  }, function () {
    toast("Stream '" + streamName + "' ended", 'err');
    removeSubscription(subId);
  });

  addSubscription(subId, sub);
  toast("Subscribed to '" + streamName + "'", 'ok');
}

export function clearSubEvents(subId) {
  updateSubscription(subId, { events: [] });
}

export function doUnsubscribe(subId) {
  const sub = subscriptions.value.get(subId);
  if (!sub) return;
  if (sub.rpcId != null) sdk.rpc.unsubscribe(sub.rpcId);
  removeSubscription(subId);
  toast("Unsubscribed from '" + sub.streamName + "'", 'ok');
}

// Execute a store method and display the result in the panel override.
export async function runExecAndShow(storeId, name, payloadBytes) {
  closeModal();
  try {
    const resp = await sdk.api.dynamic.Exec({
      store_id: storeId, method: name, payload: payloadBytes || new Uint8Array(0),
    });
    if (resp.error) throw new Error(resp.error);
    await showExecResult(name, resp.result || new Uint8Array(0), storeId);
  } catch (e) { setPanelOverride({ type: 'execError', method: name, message: e.message }); }
}

// Exec result display — builds a Preact vnode for the panel override.
export async function showExecResult(method, resultBytes, storeId) {
  const schema = await getSchema(storeId);
  const methodDesc = schema?.service?.methods[method];
  let body = null;

  if (methodDesc) {
    try {
      const outputType = schema.root.lookupType(methodDesc.responseType);
      const obj = decodeMessage(outputType, resultBytes);
      const fields = formatFields(outputType, obj);
      if (fields.length > 0) {
        body = tryRenderTable(fields);
        if (!body) {
          body = html`<div class="kv">${fields.map(function (f) {
            return html`<div class="k">${f.name}</div><div class="v mono">${fmtFieldValue(f)}</div>`;
          })}</div>`;
        }
      }
    } catch (e) { /* decode failed */ }
  }

  if (!body) {
    if (!resultBytes || resultBytes.length === 0) {
      body = html`<p class="muted">Empty response</p>`;
    } else {
      body = html`<pre class="mono">${displayBytes(resultBytes)}</pre>`;
    }
  }

  setPanelOverride({ type: 'execResult', method: method, body: body });
}

export async function uploadBundleToStore(storeIdBytes, file) {
  try {
    const store = await sdk.openStore(uuidFromBytes(storeIdBytes));
    const bytes = new Uint8Array(await file.arrayBuffer());
    const resp = await store.UploadBundle({ data: bytes });
    toast('Uploaded bundle: ' + (resp.app_id || file.name), 'ok');
  } catch (e) {
    toast('Upload failed: ' + e.message, 'err');
  }
}
