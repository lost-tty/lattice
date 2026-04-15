// ============================================================================
// Lattice SDK — Unified Client
//
// Used by both the management UI and hosted apps.
//
//   const sdk = await LatticeSDK.connect(options);
//   sdk.api.node.GetStatus()           // auto-generated unary stubs
//   sdk.rpc.subscribe(...)             // raw SSE streaming
//   sdk.proto.lookup('...')            // proto type lookup
//   const store = await sdk.openStore(uuid);     // explicit store
//   const store = await sdk.openAppStore();       // auto-detect from subdomain
//   await store.Put({ key, value });              // auto-generated store methods
//
// Wire protocol:
//   POST /rpc/{service}/{method}   — unary;  body = proto in, response = proto out
//   POST /sse/{service}/{method}   — server-streaming; body = proto in,
//                                    response = SSE stream (data: base64-proto)
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

  // --- Base64 (used for SSE event payloads) ---

  function base64ToBytes(b64) {
    const bin = atob(b64);
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
    return out;
  }

  // --- Store schema loader ---

  async function loadStoreSchema(api, storeIdBytes, storeUuid) {
    const desc = await api.dynamic.GetDescriptor({ store_id: storeIdBytes });
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

    // Load proto descriptors
    const urls = ['/proto/api.bin'].concat(extraProtos);
    const responses = await Promise.all(urls.map(function (url) { return fetch(url); }));
    for (let i = 0; i < responses.length; i++) {
      if (!responses[i].ok) throw new Error('Failed to load descriptor: ' + urls[i]);
    }
    const buffers = await Promise.all(responses.map(function (r) { return r.arrayBuffer(); }));

    const apiRoot = protobuf.Root.fromDescriptor(new Uint8Array(buffers[0]));
    const extraRoots = [];
    for (let j = 1; j < buffers.length; j++) {
      extraRoots.push(protobuf.Root.fromDescriptor(new Uint8Array(buffers[j])));
    }

    // --- HTTP RPC internals ---

    async function rpcCall(service, method, payload) {
      const url = '/rpc/' + resolveService(service) + '/' + method;
      let resp;
      try {
        resp = await fetch(url, {
          method: 'POST',
          headers: { 'content-type': 'application/octet-stream' },
          body: payload || new Uint8Array(0),
        });
      } catch (e) {
        statusCb?.('error');
        throw new Error('Network error: ' + (e.message || e));
      }
      if (!resp.ok) {
        const text = await resp.text().catch(function () { return ''; });
        throw new Error(text || ('HTTP ' + resp.status));
      }
      const buf = await resp.arrayBuffer();
      return new Uint8Array(buf);
    }

    /**
     * Open a server-streaming RPC over SSE. Returns a handle whose
     * `unsubscribe()` aborts the underlying fetch — the server-side stream
     * is dropped naturally when the response body reader is canceled.
     *
     * `onOpen` is invoked once when the SSE response arrives successfully
     * (status 200 + readable body). Useful as a connectivity signal for
     * long-lived subscriptions. `onError` is invoked once with the gRPC
     * error message if the stream terminates with a server-side error
     * (e.g. Unimplemented). `onEnd` is invoked exactly once when the
     * stream closes (after `onError` if any).
     */
    function rpcSubscribe(service, method, payload, onEvent, onEnd, onError, onOpen) {
      const url = '/sse/' + resolveService(service) + '/' + method;
      const ctrl = new AbortController();
      const ref = { aborted: false };

      (async () => {
        let resp;
        try {
          resp = await fetch(url, {
            method: 'POST',
            headers: {
              'content-type': 'application/octet-stream',
              'accept': 'text/event-stream',
            },
            body: payload || new Uint8Array(0),
            signal: ctrl.signal,
          });
        } catch (e) {
          if (!ref.aborted) onError?.(e.message || String(e));
          onEnd?.();
          return;
        }
        if (!resp.ok) {
          const text = await resp.text().catch(function () { return ''; });
          onError?.(text || ('HTTP ' + resp.status));
          onEnd?.();
          return;
        }
        if (!resp.body) {
          onEnd?.();
          return;
        }
        onOpen?.();
        const reader = resp.body.getReader();
        const decoder = new TextDecoder();
        let buf = '';
        try {
          while (true) {
            const { done, value } = await reader.read();
            if (done) break;
            // Normalize CRLF and bare CR to LF so the event splitter only
            // has to handle one line-terminator shape (SSE permits all three).
            buf += decoder.decode(value, { stream: true })
              .replace(/\r\n/g, '\n').replace(/\r/g, '\n');
            // SSE events are separated by a blank line ("\n\n").
            let idx;
            while ((idx = buf.indexOf('\n\n')) >= 0) {
              const raw = buf.slice(0, idx);
              buf = buf.slice(idx + 2);
              const evt = parseSseEvent(raw);
              if (evt.event === 'error') {
                onError?.(evt.data);
              } else if (evt.data) {
                try {
                  onEvent(base64ToBytes(evt.data));
                } catch (e) {
                  console.error('[LatticeSDK] decode error:', e);
                }
              }
            }
          }
        } catch (e) {
          if (!ref.aborted) onError?.(e.message || String(e));
        } finally {
          onEnd?.();
        }
      })();

      ref.unsubscribe = function () {
        ref.aborted = true;
        ctrl.abort();
      };
      return ref;
    }

    function rpcUnsubscribe(ref) {
      ref?.unsubscribe?.();
    }

    /** Parse one SSE event block (lines until the blank separator). */
    function parseSseEvent(raw) {
      let event = 'message';
      const dataLines = [];
      const lines = raw.split('\n');
      for (const line of lines) {
        if (line.startsWith(':')) continue; // comment / keep-alive
        const colon = line.indexOf(':');
        const field = colon === -1 ? line : line.slice(0, colon);
        let value = colon === -1 ? '' : line.slice(colon + 1);
        if (value.startsWith(' ')) value = value.slice(1);
        if (field === 'event') event = value;
        else if (field === 'data') dataLines.push(value);
      }
      return { event: event, data: dataLines.join('\n') };
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
          const resp = await api.dynamic.ListMethods({ store_id: storeIdBytes });
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
          const resp = await api.dynamic.ListStreams({ store_id: storeIdBytes });
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
      extras: extraRoots,
      lookup: function (name) {
        for (let k = 0; k < allRoots.length; k++) {
          try { return allRoots[k].lookupType(name); } catch (e) {}
        }
        throw new Error('Type not found: ' + name);
      },
    };

    // No persistent connection to establish — every RPC is a fresh HTTP
    // request. The SDK no longer fires a synthetic `connected` here: that
    // would lie if the server is unreachable. Callers wire status to a
    // long-lived subscription's `onOpen`/`onError`/`onEnd` (see state.js
    // in the management UI) so the badge reflects real reachability.

    return {
      api: api,

      rpc: {
        call: rpcCall,
        subscribe: rpcSubscribe,
        unsubscribe: rpcUnsubscribe,
      },

      proto: proto,

      close: function () {
        statusCb?.('disconnected');
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
