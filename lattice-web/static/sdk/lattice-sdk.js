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

  // --- Shared constants ---

  const SERVICES = {
    node: 'lattice.daemon.v1.NodeService',
    store: 'lattice.daemon.v1.StoreService',
    dynamic: 'lattice.daemon.v1.DynamicStoreService',
  };

  function resolveService(s) { return SERVICES[s] || s; }

  const RECONNECT_MAX = 30000;

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

  // ============================================================================
  // Public API
  // ============================================================================

  async function connect(options) {
    const opts = options || {};
    const statusCb = opts.onStatus || null;
    const extraProtos = opts.extraProtos || [];

    // --- Connection-scoped state ---
    let ws = null;
    let nextId = 1;
    const pending = new Map();
    const streamHandlers = new Map();
    let reconnectDelay = 1000;
    let closed = false;
    const activeStreams = new Map();

    // Load proto descriptors
    const urls = ['/proto/api.bin', '/proto/tunnel.bin'].concat(extraProtos);
    const responses = await Promise.all(urls.map(function (url) { return fetch(url); }));
    for (let i = 0; i < responses.length; i++) {
      if (!responses[i].ok) throw new Error('Failed to load descriptor: ' + urls[i]);
    }
    const buffers = await Promise.all(responses.map(function (r) { return r.arrayBuffer(); }));

    const apiRoot = protobuf.Root.fromDescriptor(new Uint8Array(buffers[0]));
    const tunnelRoot = protobuf.Root.fromDescriptor(new Uint8Array(buffers[1]));
    const extraRoots = [];
    for (let j = 2; j < buffers.length; j++) {
      extraRoots.push(protobuf.Root.fromDescriptor(new Uint8Array(buffers[j])));
    }

    // Set up WS envelope types
    const WsRequest = tunnelRoot.lookupType('lattice.web.WsRequest');
    const WsResponse = tunnelRoot.lookupType('lattice.web.WsResponse');

    // --- WebSocket RPC internals ---

    function wsConnect(retryOnFail) {
      return new Promise(function (resolve, reject) {
        statusCb?.('connecting');
        let settled = false;

        const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
        ws = new WebSocket(proto + '//' + location.host + '/ws');
        ws.binaryType = 'arraybuffer';

        ws.onopen = function () {
          settled = true;
          reconnectDelay = 1000;

          // Re-subscribe all active streams before firing statusCb,
          // so any new subscriptions created by statusCb aren't clobbered.
          const oldIds = Array.from(activeStreams.keys());
          const streamsToReconnect = oldIds.map(function (k) { return activeStreams.get(k); });
          for (let i = 0; i < oldIds.length; i++) activeStreams.delete(oldIds[i]);

          for (let i = 0; i < streamsToReconnect.length; i++) {
            const reg = streamsToReconnect[i];
            const newId = nextId++;
            reg.ref.currentId = newId;
            streamHandlers.set(newId, { onEvent: reg.onEvent, onEnd: reg.onEnd });
            const buf = WsRequest.encode(WsRequest.create({
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
          setTimeout(function () {
            wsConnect(true).catch(function (err) {
              console.debug('[LatticeSDK] reconnect failed:', err.message || err);
            });
          }, reconnectDelay + jitter);
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
      const ref = { currentId: id };
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
      const id = ref.currentId;
      streamHandlers.delete(id);
      activeStreams.delete(id);
    }

    // --- API stub builder ---

    function buildApiStubs(root) {
      const pkg = 'lattice.daemon.v1';
      const aliases = {
        node: 'NodeService',
        store: 'StoreService',
        dynamic: 'DynamicStoreService',
      };
      const api = {};
      for (const [alias, svcName] of Object.entries(aliases)) {
        const svc = root.lookupService(pkg + '.' + svcName);
        const ns = {};
        for (const method of svc.methodsArray) {
          if (method.responseStream) continue;
          const reqType = root.lookupType(method.resolvedRequestType.fullName);
          const resType = root.lookupType(method.resolvedResponseType.fullName);
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

    // --- Build a Store client ---

    function buildStore(storeId, storeIdBytes, serviceName, schemaRoot, api, proto) {
      // Fetch and cache stream info for subscribe
      let cachedStreams = null;

      const store = {
        storeId: storeId,
        serviceName: serviceName,
        schema: schemaRoot,

        async methods() {
          const resp = await api.dynamic.ListMethods({ id: storeIdBytes });
          return (resp.items || []).map(function (m) {
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
          cachedStreams = (resp.items || []).map(function (s) {
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
          let paramType = null;
          let eventType = null;

          // Resolve param/event types from cached stream info
          if (cachedStreams) {
            const info = cachedStreams.find(function (s) { return s.name === streamName; });
            if (info) {
              if (info.paramSchema) {
                try { paramType = schemaRoot.lookupType(info.paramSchema); } catch (e) {}
              }
              if (info.eventSchema) {
                try { eventType = schemaRoot.lookupType(info.eventSchema); } catch (e) {}
              }
            }
          }

          let paramsBytes = new Uint8Array(0);
          if (paramType) {
            paramsBytes = paramType.encode(paramType.create(params || {})).finish();
          }

          const SubscribeRequest = proto.lookup('lattice.daemon.v1.SubscribeRequest');
          const StoreEvent = proto.lookup('lattice.daemon.v1.StoreEvent');

          const subPayload = SubscribeRequest.encode(SubscribeRequest.create({
            store_id: storeIdBytes,
            stream_name: streamName,
            params: paramsBytes,
          })).finish();

          const ref = rpcSubscribe('dynamic', 'Subscribe', subPayload, function (eventData) {
            try {
              const envelope = StoreEvent.decode(eventData);
              const payload = envelope.payload;
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
      const svc = schemaRoot.lookupService(serviceName);
      for (const method of svc.methodsArray) {
        if (method.responseStream) continue;
        const reqType = schemaRoot.lookupType(method.resolvedRequestType.fullName);
        const resType = schemaRoot.lookupType(method.resolvedResponseType.fullName);
        store[method.name] = async function (fields) {
          const payload = reqType.encode(reqType.create(fields || {})).finish();
          const resp = await api.dynamic.Exec({
            store_id: storeIdBytes,
            method: method.name,
            payload: payload,
          });
          if (resp.error) throw new Error(resp.error);
          const result = resp.result || new Uint8Array(0);
          return resType.decode(result);
        };
      }

      return store;
    }

    // Build API stubs
    const api = buildApiStubs(apiRoot);

    // Proto type lookup across all loaded roots
    const allRoots = [apiRoot].concat(extraRoots);
    const proto = {
      api: apiRoot,
      tunnel: tunnelRoot,
      extras: extraRoots,
      lookup: function (name) {
        for (let k = 0; k < allRoots.length; k++) {
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
        const storeIdBytes = uuidToBytes(storeId);
        const schema = await loadStoreSchema(api, storeIdBytes, storeId);
        const store = buildStore(
          storeId, storeIdBytes, schema.serviceName,
          schema.schemaRoot, api, proto
        );
        // Pre-fetch stream info
        await store.streams().catch(function () {});
        return store;
      },

      openAppStore: async function () {
        const resp = await fetch('/sdk/store-id');
        if (!resp.ok) throw new Error('LatticeSDK: failed to fetch store ID');
        const storeId = (await resp.text()).trim();
        return this.openStore(storeId);
      },
    };
  }

  window.LatticeSDK = { connect: connect };
})();
