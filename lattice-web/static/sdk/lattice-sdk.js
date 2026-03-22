// ============================================================================
// Lattice SDK — Unified Client
//
// Used by both the management UI and hosted apps.
//
//   const sdk = await LatticeSDK.connect(options);
//   sdk.api.node.GetStatus()           // auto-generated stubs
//   sdk.rpc.subscribe(...)             // raw WebSocket RPC
//   sdk.proto.lookup('...')            // proto type lookup
//   const store = await sdk.openStore(uuid);     // explicit store
//   const store = await sdk.openAppStore();       // auto-detect from subdomain
//   await store.Put({ key, value });              // auto-generated store methods
//
// Requires: window.protobuf (protobuf.min.js + protobuf-descriptor.js)
// ============================================================================

(function () {
  'use strict';

  // --- UUID helpers ---

  function uuidToBytes(uuid) {
    const hex = uuid.replace(/-/g, '');
    const bytes = new Uint8Array(16);
    for (let i = 0; i < 16; i++) bytes[i] = parseInt(hex.substr(i * 2, 2), 16);
    return bytes;
  }

  // --- WebSocket RPC internals ---

  let ws = null;
  let nextId = 1;
  const pending = new Map();
  const streamHandlers = new Map();
  let WsRequest = null;
  let WsResponse = null;
  let statusCb = null;
  let reconnectDelay = 1000;
  let closed = false;
  const RECONNECT_MAX = 30000;

  // Active stream subscriptions — re-subscribed after reconnect
  const activeStreams = new Map();

  const SERVICES = {
    node: 'lattice.daemon.v1.NodeService',
    store: 'lattice.daemon.v1.StoreService',
    dynamic: 'lattice.daemon.v1.DynamicStoreService',
  };

  function resolveService(s) { return SERVICES[s] || s; }

  function wsConnect(retryOnFail) {
    return new Promise(function (resolve, reject) {
      statusCb?.('connecting');
      var settled = false;

      const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
      ws = new WebSocket(proto + '//' + location.host + '/ws');
      ws.binaryType = 'arraybuffer';

      ws.onopen = function () {
        settled = true;
        reconnectDelay = 1000;

        // Re-subscribe all active streams before firing statusCb,
        // so any new subscriptions created by statusCb aren't clobbered.
        var oldIds = Array.from(activeStreams.keys());
        var streamsToReconnect = oldIds.map(function (k) { return activeStreams.get(k); });
        for (var i = 0; i < oldIds.length; i++) activeStreams.delete(oldIds[i]);

        for (var i = 0; i < streamsToReconnect.length; i++) {
          var reg = streamsToReconnect[i];
          var newId = nextId++;
          reg.ref.currentId = newId;
          streamHandlers.set(newId, { onEvent: reg.onEvent, onEnd: reg.onEnd });
          var buf = WsRequest.encode(WsRequest.create({
            id: newId,
            service: resolveService(reg.service),
            method: reg.method,
            payload: reg.payload,
            server_streaming: true,
          })).finish();
          ws.send(buf);
          activeStreams.set(newId, reg);
        }

        statusCb?.('connected');
        resolve();
      };

      ws.onclose = function () {
        statusCb?.('disconnected');
        for (const [, p] of pending) p.reject(new Error('WebSocket disconnected'));
        pending.clear();
        // Don't fire onEnd for active streams — they will be re-subscribed on reconnect.
        // Only clear the handler map so stale IDs don't dispatch events.
        streamHandlers.clear();

        if (closed) return;

        if (!settled && !retryOnFail) {
          reject(new Error('WebSocket connection failed'));
          return;
        }

        // Exponential backoff with jitter
        const jitter = Math.random() * reconnectDelay * 0.3;
        setTimeout(function () { wsConnect(true).catch(function () {}); }, reconnectDelay + jitter);
        reconnectDelay = Math.min(reconnectDelay * 2, RECONNECT_MAX);
      };

      ws.onerror = function () {
        statusCb?.('error');
      };

      ws.onmessage = function (e) {
        const resp = WsResponse.decode(new Uint8Array(e.data));
        const id = typeof resp.id?.toNumber === 'function'
          ? resp.id.toNumber()
          : Number(resp.id);

        // Stream event or end
        const streamData = resp.stream;
        if ((streamData && streamData.length > 0) || resp.stream_end) {
          const handler = streamHandlers.get(id);
          if (handler) {
            if (resp.stream_end) { handler.onEnd?.(); streamHandlers.delete(id); }
            else handler.onEvent(streamData);
          }
          return;
        }

        // Unary response
        const p = pending.get(id);
        if (p) {
          pending.delete(id);
          if (resp.error) p.reject(new Error(resp.error));
          else p.resolve(resp.payload || new Uint8Array(0));
        }
      };
    });
  }

  function rpcCall(service, method, payload) {
    return new Promise(function (resolve, reject) {
      if (!ws || ws.readyState !== WebSocket.OPEN) {
        return reject(new Error('WebSocket not connected'));
      }
      const id = nextId++;
      pending.set(id, { resolve: resolve, reject: reject });
      const buf = WsRequest.encode(WsRequest.create({
        id: id,
        service: resolveService(service),
        method: method,
        payload: payload || new Uint8Array(0),
      })).finish();
      ws.send(buf);
    });
  }

  function rpcSubscribe(service, method, payload, onEvent, onEnd) {
    if (!ws || ws.readyState !== WebSocket.OPEN) {
      console.error('[LatticeSDK] rpcSubscribe called while WebSocket not connected');
      return { currentId: -1 };
    }
    const id = nextId++;
    // Shared ref object so unsubscribe always tracks the latest ID after reconnect remapping
    var ref = { currentId: id };
    streamHandlers.set(id, { onEvent: onEvent, onEnd: onEnd });
    const buf = WsRequest.encode(WsRequest.create({
      id: id,
      service: resolveService(service),
      method: method,
      payload: payload || new Uint8Array(0),
      server_streaming: true,
    })).finish();
    ws.send(buf);

    // Track for re-subscription on reconnect
    activeStreams.set(id, {
      ref: ref,
      service: service,
      method: method,
      payload: payload,
      onEvent: onEvent,
      onEnd: onEnd,
    });

    return ref;
  }

  function rpcUnsubscribe(ref) {
    var id = ref.currentId;
    streamHandlers.delete(id);
    activeStreams.delete(id);
  }

  // --- API stub builder ---

  function buildApiStubs(apiRoot) {
    const pkg = 'lattice.daemon.v1';
    const aliases = {
      node: 'NodeService',
      store: 'StoreService',
      dynamic: 'DynamicStoreService',
    };
    const api = {};
    for (const [alias, svcName] of Object.entries(aliases)) {
      const svc = apiRoot.lookupService(pkg + '.' + svcName);
      const ns = {};
      for (const method of svc.methodsArray) {
        if (method.responseStream) continue;
        const reqType = apiRoot.lookupType(method.resolvedRequestType.fullName);
        const resType = apiRoot.lookupType(method.resolvedResponseType.fullName);
        ns[method.name] = async function (fields) {
          const payload = reqType.encode(reqType.create(fields || {})).finish();
          const buf = await rpcCall(alias, method.name, payload);
          return resType.decode(buf);
        };
      }
      api[alias] = ns;
    }
    return api;
  }

  // --- Store schema loader ---

  async function loadStoreSchema(api, storeIdBytes, storeUuid) {
    const desc = await api.dynamic.GetDescriptor({ id: storeIdBytes });
    const serviceName = desc.service_name || '';

    const resp = await fetch('/proto/store/' + storeUuid);
    if (!resp.ok) throw new Error('Failed to load store schema: ' + resp.status);
    const buf = await resp.arrayBuffer();

    const schemaRoot = protobuf.Root.fromDescriptor(new Uint8Array(buf));
    return { schemaRoot: schemaRoot, serviceName: serviceName };
  }

  // --- Build a Store client ---

  function buildStore(storeId, storeIdBytes, serviceName, schemaRoot, api, proto) {
    // Fetch and cache stream info for subscribe
    let cachedStreams = null;

    var store = {
      storeId: storeId,
      serviceName: serviceName,
      schema: schemaRoot,

      async methods() {
        const resp = await api.dynamic.ListMethods({ id: storeIdBytes });
        return (resp.methods || []).map(function (m) {
          return {
            name: m.name,
            description: m.description || '',
            kind: m.kind === 1 ? 'query' : 'command',
          };
        });
      },

      async streams() {
        if (cachedStreams) return cachedStreams;
        const resp = await api.dynamic.ListStreams({ id: storeIdBytes });
        cachedStreams = (resp.streams || []).map(function (s) {
          return {
            name: s.name,
            description: s.description || '',
            paramSchema: s.param_schema || '',
            eventSchema: s.event_schema || '',
          };
        });
        return cachedStreams;
      },

      subscribe(streamName, params, onEvent) {
        var paramType = null;
        var eventType = null;

        // Resolve param/event types from cached stream info
        if (cachedStreams) {
          var info = cachedStreams.find(function (s) { return s.name === streamName; });
          if (info) {
            if (info.paramSchema) {
              try { paramType = schemaRoot.lookupType(info.paramSchema); } catch (e) {}
            }
            if (info.eventSchema) {
              try { eventType = schemaRoot.lookupType(info.eventSchema); } catch (e) {}
            }
          }
        }

        var paramsBytes = new Uint8Array(0);
        if (paramType) {
          paramsBytes = paramType.encode(paramType.create(params || {})).finish();
        }

        var SubscribeRequest = proto.lookup('lattice.daemon.v1.SubscribeRequest');
        var StoreEvent = proto.lookup('lattice.daemon.v1.StoreEvent');

        var subPayload = SubscribeRequest.encode(SubscribeRequest.create({
          store_id: storeIdBytes,
          stream_name: streamName,
          params: paramsBytes,
        })).finish();

        var ref = rpcSubscribe('dynamic', 'Subscribe', subPayload, function (eventData) {
          try {
            var envelope = StoreEvent.decode(eventData);
            var payload = envelope.payload;
            if (!payload || payload.length === 0) return;
            if (eventType) {
              onEvent(eventType.decode(payload));
            } else {
              onEvent({ payload: payload });
            }
          } catch (e) {
            console.error('[LatticeSDK] stream decode error:', e);
          }
        });

        return function unsubscribe() {
          rpcUnsubscribe(ref);
        };
      },
    };

    // Auto-generate named methods from the store's protobuf service definition
    var svc = schemaRoot.lookupService(serviceName);
    for (const method of svc.methodsArray) {
      if (method.responseStream) continue;
      const reqType = schemaRoot.lookupType(method.resolvedRequestType.fullName);
      const resType = schemaRoot.lookupType(method.resolvedResponseType.fullName);
      store[method.name] = async function (fields) {
        var payload = reqType.encode(reqType.create(fields || {})).finish();
        var resp = await api.dynamic.Exec({
          store_id: storeIdBytes,
          method: method.name,
          payload: payload,
        });
        if (resp.error) throw new Error(resp.error);
        var result = resp.result || new Uint8Array(0);
        return resType.decode(result);
      };
    }

    return store;
  }

  // ============================================================================
  // Public API
  // ============================================================================

  async function connect(options) {
    options = options || {};
    statusCb = options.onStatus || null;
    var extraProtos = options.extraProtos || [];

    // Load proto descriptors
    var urls = ['/proto/api.bin', '/proto/tunnel.bin'].concat(extraProtos);
    var responses = await Promise.all(urls.map(function (url) { return fetch(url); }));
    for (var i = 0; i < responses.length; i++) {
      if (!responses[i].ok) throw new Error('Failed to load descriptor: ' + urls[i]);
    }
    var buffers = await Promise.all(responses.map(function (r) { return r.arrayBuffer(); }));

    var apiRoot = protobuf.Root.fromDescriptor(new Uint8Array(buffers[0]));
    var tunnelRoot = protobuf.Root.fromDescriptor(new Uint8Array(buffers[1]));
    var extraRoots = [];
    for (var j = 2; j < buffers.length; j++) {
      extraRoots.push(protobuf.Root.fromDescriptor(new Uint8Array(buffers[j])));
    }

    // Set up WS envelope types
    WsRequest = tunnelRoot.lookupType('lattice.web.WsRequest');
    WsResponse = tunnelRoot.lookupType('lattice.web.WsResponse');

    // Build API stubs
    var api = buildApiStubs(apiRoot);

    // Proto type lookup across all loaded roots
    var allRoots = [apiRoot].concat(extraRoots);
    var proto = {
      api: apiRoot,
      tunnel: tunnelRoot,
      extras: extraRoots,
      lookup: function (name) {
        for (var k = 0; k < allRoots.length; k++) {
          try { return allRoots[k].lookupType(name); } catch (e) {}
        }
        throw new Error('Type not found: ' + name);
      },
    };

    // Connect WebSocket
    await wsConnect();

    return {
      api: api,

      rpc: {
        call: rpcCall,
        subscribe: rpcSubscribe,
        unsubscribe: rpcUnsubscribe,
      },

      proto: proto,

      close: function () {
        closed = true;
        for (const [, h] of streamHandlers) h.onEnd?.();
        streamHandlers.clear();
        activeStreams.clear();
        for (const [, p] of pending) p.reject(new Error('SDK closed'));
        pending.clear();
        if (ws) { ws.close(); ws = null; }
      },

      openStore: async function (storeId) {
        var storeIdBytes = uuidToBytes(storeId);
        var schema = await loadStoreSchema(api, storeIdBytes, storeId);
        var store = buildStore(
          storeId, storeIdBytes, schema.serviceName,
          schema.schemaRoot, api, proto
        );
        // Pre-fetch stream info
        await store.streams().catch(function () {});
        return store;
      },

      openAppStore: async function () {
        var resp = await fetch('/sdk/store-id');
        if (!resp.ok) throw new Error('LatticeSDK: failed to fetch store ID');
        var storeId = (await resp.text()).trim();
        return this.openStore(storeId);
      },
    };
  }

  window.LatticeSDK = { connect: connect };
})();
