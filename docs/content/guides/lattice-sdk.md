---
title: "Lattice SDK"
weight: 1
---

The Lattice SDK is a browser-side JavaScript library used by both the management UI and hosted apps. It speaks HTTP RPC + SSE to the Lattice web server, handles protobuf schema loading, and generates API stubs from the loaded service definitions.

## Loading the SDK

The SDK requires protobufjs as a vendor dependency. Both scripts are served by the Lattice web server.

```html
<script src="/sdk/vendor/protobuf.min.js"></script>
<script src="/sdk/vendor/protobuf-descriptor.js"></script>
<script src="/sdk/lattice-sdk.js"></script>
```

The SDK attaches a single global: `window.LatticeSDK`.

## Connecting

```js
const sdk = await LatticeSDK.connect({
  onStatus(status) {
    // 'connecting', 'connected', 'disconnected', 'error'
    console.log(status);
  },
  extraProtos: ['/proto/weaver.bin'],  // optional additional descriptors
});
```

`connect()` is async. It fetches protobuf descriptors and builds API stubs for all gRPC services. The promise resolves once initialization completes; no persistent connection is opened — every RPC is its own HTTP request.

`onStatus` reflects SDK state, not server reachability. Drive a real "is the server reachable" badge from the lifecycle of a long-lived `sdk.rpc.subscribe(...)` (e.g. `node.Subscribe`) — its `onOpen`/`onError`/`onEnd` callbacks fire when the SSE stream opens, errors, or ends.

## API Stubs

`sdk.api` has stubs for all unary RPC methods, built from the protobuf service definitions.

```js
// NodeService
const status = await sdk.api.node.GetStatus({});
const types = await sdk.api.node.ListStoreTypes({});

// StoreService
const stores = await sdk.api.store.List({});
await sdk.api.store.Create({ name: 'my-store', store_type: 'core:kvstore' });

// DynamicStoreService
const methods = await sdk.api.dynamic.ListMethods({ id: storeIdBytes });
const resp = await sdk.api.dynamic.Exec({ store_id: storeIdBytes, method: 'Get', payload });
```

Fields are plain objects matching the protobuf request message. Return values are decoded protobuf response messages.

## Raw RPC

`sdk.rpc` provides low-level RPC access for cases where the auto-generated stubs don't apply (e.g., server-streaming RPCs). Unary calls go to `POST /rpc/{service}/{method}`; subscriptions go to `POST /sse/{service}/{method}` and read the SSE response.

```js
// Unary call
const result = await sdk.rpc.call('node', 'GetStatus', payload);

// Server-streaming
const sub = sdk.rpc.subscribe('node', 'Subscribe', new Uint8Array(0),
  function onEvent(data) { /* Uint8Array */ },
  function onEnd() { /* stream ended */ },
  function onError(msg) { /* server-side gRPC error */ },
  function onOpen() { /* SSE response established */ },
);

// Stop receiving events (aborts the underlying fetch)
sdk.rpc.unsubscribe(sub);
```

Service names can be short aliases (`'node'`, `'store'`, `'dynamic'`) or fully-qualified (`'lattice.daemon.v1.NodeService'`).

Subscriptions created via `sdk.rpc.subscribe` are automatically re-established on reconnect.

## Proto Type Lookup

`sdk.proto` provides access to the loaded protobuf roots for manual type encoding/decoding.

```js
// Look up a type across all loaded roots (api + extras)
const NodeEvent = sdk.proto.lookup('lattice.daemon.v1.NodeEvent');
const decoded = NodeEvent.decode(bytes);

// Direct root access
sdk.proto.api     // api.bin root
sdk.proto.extras  // array of extra roots from extraProtos option
```

## Store Client

Open a store to call its methods and subscribe to its streams.

### `sdk.openStore(id)`

Open a store by UUID. Used by the management UI when exploring a specific store.

```js
const store = await sdk.openStore('a1b2c3d4-...');
console.log(store.serviceName);  // "lattice.kv.KvStore"
```

### `sdk.openAppStore()`

Auto-detect the store from the current subdomain. Used by hosted apps -- the server maps the subdomain to a store ID.

```js
const store = await sdk.openAppStore();
```

### Auto-generated methods

Store objects have named methods for every unary RPC in the store's protobuf service, generated from the schema at open time. Fields are encoded using the method's request proto. The response is decoded and returned as a plain object.

```js
const enc = new TextEncoder();
const dec = new TextDecoder();

await store.Put({ key: enc.encode('hello'), value: enc.encode('world') });

const resp = await store.Get({ key: enc.encode('hello') });
console.log(dec.decode(resp.value));
```

This works the same way as `sdk.api.*` stubs — method names come from the protobuf service definition.

### `store.subscribe(streamName, params, onEvent)`

Subscribe to an event stream. Returns an unsubscribe function.

```js
const unsub = store.subscribe('watch', { prefix: enc.encode('user:') }, function (event) {
  console.log(dec.decode(event.key), event.deleted);
});

unsub();
```

### `store.methods()` / `store.streams()`

Discover available methods and streams at runtime.

```js
const methods = await store.methods();
// [{ name: 'Put', kind: 'command', description: '...' }, ...]

const streams = await store.streams();
// [{ name: 'watch', paramSchema: 'lattice.kv.WatchParams', eventSchema: '...', description: '...' }]
```

### `store.schema`

The protobuf `Root` for the store's schema. Use for manual type lookups.

## App Bundle Format

Apps are deployed as zip archives containing a `manifest.toml` and static assets:

```
my-app.zip
  manifest.toml
  index.html
  app.js
  style.css
```

### manifest.toml

```toml
[app]
id = "inventory"
name = "Inventory Manager"
version = "1.0"
store_type = "core:kvstore"
```

### Registering an app

Use the CLI:

```
lattice app register --subdomain inventory --app-id inventory --store <data-store-uuid>
```

## Complete Example

A minimal counter app using a KV store:

```html
<!DOCTYPE html>
<html>
<head><meta charset="utf-8"><title>Counter</title></head>
<body>
  <h1 id="count">Loading...</h1>
  <button id="inc">+1</button>
  <span id="status"></span>

  <script src="/sdk/vendor/protobuf.min.js"></script>
  <script src="/sdk/vendor/protobuf-descriptor.js"></script>
  <script src="/sdk/lattice-sdk.js"></script>
  <script>
    (async () => {
      const enc = new TextEncoder();
      const dec = new TextDecoder();

      const sdk = await LatticeSDK.connect({
        onStatus(s) { document.getElementById('status').textContent = s; }
      });
      const store = await sdk.openAppStore();

      async function getCounter() {
        const resp = await store.Get({ key: enc.encode('counter') });
        if (!resp.value || resp.value.length === 0) return 0;
        return JSON.parse(dec.decode(resp.value));
      }

      async function render() {
        document.getElementById('count').textContent = await getCounter();
      }

      document.getElementById('inc').addEventListener('click', async () => {
        const current = await getCounter();
        await store.Put({
          key: enc.encode('counter'),
          value: enc.encode(JSON.stringify(current + 1)),
        });
        await render();
      });

      store.subscribe('watch', { prefix: enc.encode('counter') }, () => render());
      await render();
    })();
  </script>
</body>
</html>
```

## Security Notes

- Apps served at subdomains can only access `DynamicStoreService`. The RPC bridge rejects calls to `NodeService` or `StoreService` from app subdomains.
- Each app's RPC + SSE calls are scoped to its bound store; cross-store access from the subdomain is rejected.
- App bindings are node-local. A remote peer cannot force your node to serve an app.

## Type Reference

```typescript
// LatticeSDK.connect(options?) → Promise<SDK>
interface SDK {
  api: {
    node: Record<string, (fields?) => Promise<object>>;
    store: Record<string, (fields?) => Promise<object>>;
    dynamic: Record<string, (fields?) => Promise<object>>;
  };
  rpc: {
    call(service: string, method: string, payload?: Uint8Array): Promise<Uint8Array>;
    subscribe(service: string, method: string, payload: Uint8Array,
              onEvent: (data: Uint8Array) => void, onEnd?: () => void): object;
    unsubscribe(ref: object): void;
  };
  proto: {
    api: ProtoRoot;
    tunnel: ProtoRoot;
    extras: ProtoRoot[];
    lookup(name: string): ProtoType;
  };
  openStore(id: string): Promise<Store>;
  openAppStore(): Promise<Store>;
}

interface Store {
  readonly storeId: string;
  readonly serviceName: string;
  readonly schema: ProtoRoot;
  methods(): Promise<MethodInfo[]>;
  streams(): Promise<StreamInfo[]>;
  subscribe(stream: string, params: object, onEvent: (event: object) => void): () => void;
  // Plus auto-generated methods from the store's protobuf service, e.g.:
  // Put(fields?: object): Promise<object>;
  // Get(fields?: object): Promise<object>;
  [method: string]: (fields?: object) => Promise<object>;
}

interface MethodInfo { name: string; description: string; kind: 'command' | 'query'; }
interface StreamInfo { name: string; description: string; paramSchema: string; eventSchema: string; }

type StatusCallback = (status: 'connecting' | 'connected' | 'disconnected' | 'error') => void;
```
