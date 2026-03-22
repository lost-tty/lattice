import { html, useState, useEffect, useRef } from './util.js';
import * as Helpers from '../helpers.js';
import * as S from '../state.js';
import * as Schema from '../schema.js';
import { showSubscribeStream, clearSubEvents, doUnsubscribe } from './actions.js';
import { sdk } from '../sdk.js';

export async function loadStreams(storeId) {
  const streams = (await sdk.api.dynamic.ListStreams({ id: storeId })).streams || [];
  return html`<${StreamsView} streams=${streams} storeId=${storeId} />`;
}

function StreamsView({ streams, storeId }) {
  // Read subscriptions reactively so stream events update live
  const subscriptions = S.subscriptions.value;
  const storeUuid = Helpers.uuidFromBytes(storeId);
  const activeSubs = [];
  for (const [id, sub] of subscriptions) {
    if (Helpers.uuidFromBytes(sub.storeId) === storeUuid) {
      activeSubs.push({ id, ...sub });
    }
  }

  if (streams.length === 0 && activeSubs.length === 0) {
    return html`<div class="empty-state">No streams available</div>`;
  }

  return html`
    ${streams.length > 0 ? html`
      <h3 class="section-heading">Available Streams</h3>
      ${streams.map(s => html`<${StreamCard} stream=${s} storeId=${storeId} />`)}
    ` : null}
    ${activeSubs.length > 0 ? html`
      <h3 class="section-heading${streams.length > 0 ? ' mt-section' : ''}">Active Subscriptions</h3>
      ${activeSubs.map(sub => html`<${SubscriptionCard} sub=${sub} />`)}
    ` : null}
  `;
}

function StreamCard({ stream, storeId }) {
  const [params, setParams] = useState(null);

  useEffect(() => {
    (async () => {
      if (!stream.param_schema) return;
      try {
        const schema = await Schema.getSchema(storeId);
        if (schema?.root) {
          const fields = Schema.describeFields(schema.root, stream.param_schema);
          if (fields) setParams(fields);
        }
      } catch (e) { /* type not found */ }
    })();
  }, [stream.param_schema, storeId]);

  return html`
    <div class="card card-compact">
      <div class="card-row">
        <div>
          <span class="mono mono-medium">${stream.name}</span>
          <span class="muted ml-desc">${stream.description}</span>
        </div>
        <button class="btn btn-primary" onClick=${() => showSubscribeStream(storeId, stream)}>Subscribe</button>
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

function SubscriptionCard({ sub }) {
  const eventsRef = useRef(null);

  useEffect(() => {
    if (eventsRef.current) eventsRef.current.scrollTop = eventsRef.current.scrollHeight;
  }, [sub.events.length]);

  return html`
    <div class="card stream-sub">
      <div class="card-row sub-header">
        <div>
          <span class="mono mono-medium">${sub.streamName}</span>
          <span class="badge badge-green ml-xs">
            ${sub.events.length} event${sub.events.length !== 1 ? 's' : ''}
          </span>
        </div>
        <div class="btn-group">
          <button class="btn" onClick=${() => clearSubEvents(sub.id)}>Clear</button>
          <button class="btn btn-danger" onClick=${() => doUnsubscribe(sub.id)}>Unsubscribe</button>
        </div>
      </div>
      <div class="stream-events" ref=${eventsRef}>
        ${sub.events.length === 0
          ? html`<div class="muted placeholder-text">Waiting for events...</div>`
          : sub.events.map(ev => html`
              <div class="stream-event">
                <span class="muted">${new Date(ev.time).toLocaleTimeString()}</span>
                <span class="mono">${ev.display}</span>
              </div>
            `)
        }
      </div>
    </div>
  `;
}
