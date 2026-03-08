// ============================================================================
// API — generated gRPC stubs + hand-written helpers
//
// API.node.GetStatus(), API.store.List({parent_id: bytes}), etc. are built
// at boot time from the proto service reflection.  Each stub encodes the
// request, calls RPC.call, and decodes the response automatically.
//
// The few functions below add logic that can't be generated (tree walks,
// error unwrapping, subscribe payload building).
// ============================================================================

// Populated by buildAPI() at bootstrap from the proto descriptor.
const API = {};

// Proto types namespace — set by initTypes() after loading descriptors.
let T = null;

function setProtoTypes(types) { T = types; }

// Service short-name → protobufjs Service reflection object.
const SERVICE_ALIAS = { node: 'NodeService', store: 'StoreService', dynamic: 'DynamicStoreService' };

// Build API stubs from the apiRoot's service definitions.
// Called once at bootstrap after Root.fromDescriptor().
function buildAPI(apiRoot) {
  const pkg = 'lattice.daemon.v1';
  for (const [alias, svcName] of Object.entries(SERVICE_ALIAS)) {
    const svc = apiRoot.lookupService(`${pkg}.${svcName}`);
    const ns = {};
    for (const method of svc.methodsArray) {
      if (method.responseStream) continue; // skip server-streaming (Subscribe)
      const reqType = apiRoot.lookupType(method.resolvedRequestType.fullName);
      const resType = apiRoot.lookupType(method.resolvedResponseType.fullName);
      ns[method.name] = async (fields = {}) => {
        const payload = reqType.encode(reqType.create(fields)).finish();
        const buf = await RPC.call(alias, method.name, payload);
        return resType.decode(buf);
      };
    }
    API[alias] = ns;
  }
}

// ============================================================================
// Convenience helpers used by app.js
// ============================================================================

async function listStores() {
  const roots = (await API.store.List({})).stores || [];
  const result = [];
  const seen = new Set();
  async function walk(stores, depth) {
    for (const s of stores) {
      const uuid = Helpers.uuidFromBytes(s.id);
      if (seen.has(uuid)) continue;
      seen.add(uuid);
      result.push({ ...s, depth });
      try {
        const children = (await API.store.List({ parent_id: s.id })).stores || [];
        if (children.length > 0) await walk(children, depth + 1);
      } catch (e) { /* store may not support children */ }
    }
  }
  await walk(roots, 0);
  return result;
}

async function execStore(uuidBytes, method, payloadBytes) {
  const resp = await API.dynamic.Exec({
    store_id: uuidBytes, method, payload: payloadBytes || new Uint8Array(0),
  });
  if (resp.error) throw new Error(resp.error);
  return resp.result || new Uint8Array(0);
}

function buildSubscribePayload(uuidBytes, streamName, params) {
  return T.SubscribeRequest.encode(
    T.SubscribeRequest.create({ store_id: uuidBytes, stream_name: streamName, params: params || new Uint8Array(0) })
  ).finish();
}
