// ============================================================================
// App — bootstrap + Preact mount
//
// Loads proto descriptors, builds API stubs, sets up RPC types,
// then renders the Preact component tree.
// ============================================================================

// Called by rpc.js on WebSocket open
async function init() {
  await S.refresh();

  // Subscribe to node events
  const NodeEvent = T.NodeEvent;
  RPC.subscribe('node', 'Subscribe', new Uint8Array(0), (eventData) => {
    const ev = NodeEvent.decode(eventData);
    if (ev.store_ready) {
      S.toast(`Store ready: ${Helpers.uuidFromBytes(ev.store_ready.store_id)}`, 'ok');
      S.refresh();
    } else if (ev.join_failed) {
      S.toast(`Join failed: ${ev.join_failed.reason || 'unknown'}`, 'err');
    } else if (ev.sync_result) {
      const sr = ev.sync_result;
      S.toast(`Sync: ${sr.peers_synced || 0} peers, ${sr.entries_sent || 0} sent, ${sr.entries_received || 0} received`, 'ok');
      S.refresh();
    }
  }, () => {
    S.toast('Event stream ended', 'err');
  });
}

// ============================================================================
// Bootstrap — load descriptors, build API, mount Preact
// ============================================================================

(async function bootstrap() {
  try {
    const [apiResp, tunnelResp, weaverResp] = await Promise.all([
      fetch('/proto/api.bin'),
      fetch('/proto/tunnel.bin'),
      fetch('/proto/weaver.bin'),
    ]);

    if (!apiResp.ok) throw new Error(`Failed to load API descriptor: ${apiResp.status}`);
    if (!tunnelResp.ok) throw new Error(`Failed to load tunnel descriptor: ${tunnelResp.status}`);
    if (!weaverResp.ok) throw new Error(`Failed to load weaver descriptor: ${weaverResp.status}`);

    const [apiBuf, tunnelBuf, weaverBuf] = await Promise.all([
      apiResp.arrayBuffer(),
      tunnelResp.arrayBuffer(),
      weaverResp.arrayBuffer(),
    ]);

    const apiRoot = protobuf.Root.fromDescriptor(new Uint8Array(apiBuf));
    const tunnelRoot = protobuf.Root.fromDescriptor(new Uint8Array(tunnelBuf));
    const weaverRoot = protobuf.Root.fromDescriptor(new Uint8Array(weaverBuf));

    // Set up RPC envelope types
    RPC.setTypes(
      tunnelRoot.lookupType('lattice.web.WsRequest'),
      tunnelRoot.lookupType('lattice.web.WsResponse'),
    );

    // Build auto-generated API stubs
    buildAPI(apiRoot);

    // Types still accessed directly via T.*
    const pkg = 'lattice.daemon.v1';
    const types = { root: apiRoot };
    const missing = [];
    for (const name of ['NodeEvent', 'StoreEvent', 'SubscribeRequest']) {
      try {
        types[name] = apiRoot.lookupType(`${pkg}.${name}`);
      } catch (e) { missing.push(name); }
    }
    try {
      types.WitnessContent = weaverRoot.lookupType('lattice.weaver.WitnessContent');
    } catch (e) { missing.push('WitnessContent'); }

    if (missing.length > 0) console.warn('[boot] missing types:', missing);
    setProtoTypes(types);

    // Mount Preact app
    P.render(P.html`<${App} />`, document.getElementById('app'));

    // Connect WebSocket (calls init() on open)
    RPC.connect();
  } catch (e) {
    console.error('[boot] FATAL:', e);
    document.getElementById('app').innerHTML =
      `<div class="card" style="margin:2rem"><p style="color:var(--red)">Failed to load proto definitions: ${e.message}</p><p>Try refreshing the page.</p></div>`;
  }
})();
