// ============================================================================
// WebSocket RPC Client
//
// Uses protobufjs WsRequest/WsResponse types for envelope encode/decode.
// ============================================================================

const RPC = (() => {
  let ws = null;
  let nextId = 1;
  const pending = new Map();
  const streamHandlers = new Map();

  // These are set by init() after proto loading
  let WsRequest = null;
  let WsResponse = null;

  // Map short service aliases to fully-qualified gRPC service names.
  // The tunnel passes these through to tonic verbatim.
  const SERVICE_NAMES = {
    node: 'lattice.daemon.v1.NodeService',
    store: 'lattice.daemon.v1.StoreService',
    dynamic: 'lattice.daemon.v1.DynamicStoreService',
  };

  function resolveService(s) { return SERVICE_NAMES[s] || s; }

  function setTypes(req, resp) {
    WsRequest = req;
    WsResponse = resp;
  }

  function connect() {
    const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
    const url = `${proto}//${location.host}/ws`;
    ws = new WebSocket(url);
    ws.binaryType = 'arraybuffer';
    ws.onopen = () => {
      S.setStatus('ok', 'Connected');
      init().catch(e => console.error('[ws] init failed:', e));
    };
    ws.onclose = (e) => {
      S.setStatus('err', 'Disconnected');
      for (const [id, p] of pending) {
        p.reject(new Error('WebSocket disconnected'));
      }
      pending.clear();
      for (const [id, handler] of streamHandlers) {
        handler.onEnd?.();
      }
      streamHandlers.clear();
      setTimeout(connect, 2000);
    };
    ws.onerror = () => {
      S.setStatus('err', 'Error');
    };
    ws.onmessage = (e) => {
      const resp = WsResponse.decode(new Uint8Array(e.data));
      const id = resp.id.toNumber ? resp.id.toNumber() : Number(resp.id);

      // Stream event or stream end
      if ((resp.stream && resp.stream.length > 0) || resp.stream_end) {
        const handler = streamHandlers.get(id);
        if (handler) {
          if (resp.stream_end) { handler.onEnd?.(); streamHandlers.delete(id); }
          else handler.onEvent(resp.stream);
        }
        return;
      }

      const p = pending.get(id);
      if (p) {
        pending.delete(id);
        if (resp.error) {
          p.reject(new Error(resp.error));
        } else {
          p.resolve(resp.payload || new Uint8Array(0));
        }
      }
    };
  }

  // Send a unary RPC, returns promise of Uint8Array result
  function call(service, method, payload = new Uint8Array(0)) {
    const fqService = resolveService(service);
    return new Promise((resolve, reject) => {
      const id = nextId++;
      pending.set(id, { resolve, reject });
      const buf = WsRequest.encode(WsRequest.create({
        id, service: fqService, method, payload,
      })).finish();
      ws.send(buf);
    });
  }

  // Start a server-streaming RPC, onEvent receives Uint8Array
  function subscribe(service, method, payload, onEvent, onEnd) {
    const fqService = resolveService(service);
    const id = nextId++;
    streamHandlers.set(id, { onEvent, onEnd });
    const buf = WsRequest.encode(WsRequest.create({
      id, service: fqService, method, payload, server_streaming: true,
    })).finish();
    ws.send(buf);
    return id;
  }

  // Stop receiving events for a stream (client-side only)
  function unsubscribe(id) {
    streamHandlers.delete(id);
  }

  return { connect, call, subscribe, unsubscribe, setTypes };
})();
