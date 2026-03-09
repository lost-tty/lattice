async function loadStreams(storeId) {
  const streams = (await API.dynamic.ListStreams({ id: storeId })).streams || [];
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
      <h3 style="margin-bottom:0.6rem">Available Streams</h3>
      ${streams.map(s => html`<${StreamCard} stream=${s} storeId=${storeId} />`)}
    ` : null}
    ${activeSubs.length > 0 ? html`
      <h3 style="margin:${streams.length > 0 ? '1.2rem' : '0'} 0 0.6rem">Active Subscriptions</h3>
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
          const paramType = schema.root.lookupType(stream.param_schema);
          if (paramType && paramType.fieldsArray.length > 0) {
            setParams(paramType.fieldsArray.map(f => ({
              name: f.name, typeName: Schema.fieldTypeName(f),
            })));
          }
        }
      } catch (e) { /* type not found */ }
    })();
  }, [stream.param_schema, storeId]);

  return html`
    <div class="card" style="padding:16px 20px">
      <div style="display:flex;align-items:center;justify-content:space-between">
        <div>
          <span class="mono" style="font-weight:500">${stream.name}</span>
          <span class="muted" style="margin-left:12px">${stream.description}</span>
        </div>
        <button class="btn btn-primary" onClick=${() => showSubscribeStream(storeId, stream)}>Subscribe</button>
      </div>
      ${params ? html`
        <div style="margin-top:8px;display:flex;gap:8px;flex-wrap:wrap">
          ${params.map(p => html`
            <span class="badge badge-blue">${p.name}</span>
            <span class="muted" style="font-size:12px">${p.typeName}</span>
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
      <div style="display:flex;align-items:center;justify-content:space-between;margin-bottom:0.6rem">
        <div>
          <span class="mono" style="font-weight:500">${sub.streamName}</span>
          <span class="badge badge-green" style="margin-left:8px">
            ${sub.events.length} event${sub.events.length !== 1 ? 's' : ''}
          </span>
        </div>
        <div style="display:flex;gap:0.5rem">
          <button class="btn" onClick=${() => clearSubEvents(sub.id)}>Clear</button>
          <button class="btn btn-danger" onClick=${() => doUnsubscribe(sub.id)}>Unsubscribe</button>
        </div>
      </div>
      <div class="stream-events" ref=${eventsRef}>
        ${sub.events.length === 0
          ? html`<div class="muted" style="padding:0.4rem 0">Waiting for events...</div>`
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
