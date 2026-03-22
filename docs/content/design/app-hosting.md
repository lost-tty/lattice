---
title: "App Hosting"
status: design
weight: 7
---

> **Status**: Partially implemented. Embedded bundles, subdomain routing, app shell serving, and local bindings work. Store claiming via SystemOp is not yet implemented (currently uses KV keys in root stores).

## Overview

Lattice nodes can host web applications. Each app is a frontend bundle (JS/CSS) served at a subdomain, backed by one or more Lattice stores for data. The design separates two concerns:

1. **App definition** (replicated, in root store): identifies a store as belonging to an app — carries the app-id and maps it to a data store. Visible to all nodes in the mesh.
2. **App binding** (node-local, in meta.db): controls whether *this* node exposes a particular app, and optionally overrides the subdomain to avoid conflicts.

## Concepts

### App Type

A string identifier (e.g., `"inventory"`) that names a kind of application. The node binary ships with embedded bundles for known app types. The `app_type` determines which frontend assets are served and what store interactions the app expects.

To add a new app type today, you add a `rust-embed` struct and a match arm in `get_app_file()`. Future work (Wasm runtime, M22) will make app types dynamic.

### App Definition (Root Store)

An app definition lives in a root `core:kvstore` store as replicated KV entries:

```text
apps/{subdomain}/app-id    →  "inventory"           (app type identifier)
apps/{subdomain}/store-id  →  "uuid-of-data-store"  (which store the app uses)
```

This is mesh-wide knowledge. Every node that has the root store sees these definitions. The subdomain in the key path is the default subdomain for the app.

**Future:** These KV keys will be replaced by a `SystemOp` claim on the store itself (`system/app-id → "inventory"`), so the app identity travels with the store rather than being a separate KV entry. Multiple stores can carry the same `app-id` claim, enabling multi-store apps.

### App Binding (Node-Local)

A thin record in `meta.db` that says "this node exposes this store as an app":

```rust
pub struct AppBinding {
    pub store_id: Uuid,           // which store to expose
    pub subdomain: Option<String>, // override subdomain (or use default)
    pub enabled: bool,            // toggle without deleting
}
```

Bindings are keyed by `store_id`. The `app-id` / `app_type` is not stored in the binding — it comes from the root store definition. The binding only controls local routing.

If `subdomain` is `None`, the default subdomain from the root store definition is used. If set, it overrides the subdomain for this node (e.g., to avoid conflicts when two nodes want different URLs for the same app).

### Relationship

```
                          Mesh (replicated root store)
  ┌─────────────────────────────────────────────────────┐
  │                                                     │
  │   Root KV Store:                                    │
  │     apps/inventory/app-id   → "inventory"           │
  │     apps/inventory/store-id → "abc-123-..."         │
  │                                                     │
  │   Data Store (abc-123-...):                         │
  │     (KV data used by the inventory app)             │
  │                                                     │
  └─────────────────────────────────────────────────────┘

                          Node (local meta.db)
  ┌─────────────────────────────────────────────────────┐
  │                                                     │
  │   app_bindings table:                               │
  │     abc-123-... → { subdomain: None, enabled: true }│
  │                                                     │
  └─────────────────────────────────────────────────────┘

  HTTP: inventory.localhost:8080
    → extract subdomain "inventory"
    → scan root stores: apps/inventory/* → app_id, store_id
    → check meta.db: store abc-123 bound? yes, enabled
    → serve app shell with store_id + app_id
```

## Flows

### Registering an App

The management UI collects: subdomain, app-id, registry store (which root KV store holds the definition), and data store (which store the app uses). The `POST /api/apps/{subdomain}` endpoint:

1. Writes `apps/{subdomain}/app-id` and `apps/{subdomain}/store-id` to the root KV store (replicated)
2. Creates a local `AppBinding` in meta.db (node-local)

Both steps happen in one request. The root store write replicates to all nodes; the binding is only on this node.

### Listing Apps

`GET /api/apps` scans all root KV stores for `apps/` prefixed keys, builds the list of app definitions, then merges each with local binding state (bound/enabled/subdomain override). Every node sees the same definitions but may have different bindings.

### Serving an App

When a request arrives at `inventory.localhost:8080`:

1. Extract subdomain from `Host` header → `"inventory"`
2. Scan root stores for `apps/inventory/*` → get `app_id`, `store_id`
3. Check meta.db: is `store_id` bound and enabled on this node?
4. Serve app shell HTML with `<meta>` tags for `store_id`, `app_id`, `subdomain`
5. App JS loads, reads `<meta>` tags, calls `LatticeSDK.connect()` with the store UUID
6. SDK opens WebSocket to `/ws`, issues gRPC calls to the store

### Unregistering

`DELETE /api/apps/{subdomain}` removes both the root store KV entries (replicated) and the local binding (node-local). Other nodes still see the definition disappear once the KV delete replicates.

### Binding Without Registering

A node can create a local binding for an app that was registered by another node. The definition already exists in the root store; the node just needs to add a binding in meta.db to start serving it.

## Future: Store Claiming via SystemOp

The current KV-key-based definitions are a stepping stone. The target architecture:

1. A store carries an `app-id` key in its **system table** — a `SystemOp` that replicates with the store.
2. `SystemOp::SetAppId("inventory")` claims a store for an app type.
3. `SystemOp::ClearAppId` unclaims it.
4. Multiple stores can carry the same `app-id` — an app discovers all its stores by filtering on the claim.
5. The local `AppBinding` in meta.db remains the same: it controls which claimed stores this node exposes.

This moves the app identity from a separate KV registry into the store itself, so the relationship is intrinsic rather than external.

## REST API

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/apps` | List all app definitions (from root stores) merged with local binding state |
| `GET` | `/api/apps/{subdomain}` | Get a specific app by subdomain |
| `POST` | `/api/apps/{subdomain}` | Register app (write to root store + create local binding) |
| `DELETE` | `/api/apps/{subdomain}` | Unregister app (remove from root store + remove local binding) |

### POST body

```json
{
  "registry_store_id": "uuid of root KV store for the definition",
  "store_id": "uuid of the data store the app uses",
  "app_id": "inventory"
}
```

### GET response (per app)

```json
{
  "subdomain": "inventory",
  "app_id": "inventory",
  "store_id": "abc-123-...",
  "registry_store_id": "def-456-...",
  "bound": true,
  "enabled": true
}
```

## Implementation Status

### Done

- `AppBinding` struct — thin: `{ store_id, subdomain: Option, enabled }` (`lattice-model`)
- `MetaStore` CRUD for `app_bindings` table, keyed by store UUID (`lattice-node`)
- `LatticeBackend` trait methods: `app_set_binding`, `app_remove_binding`, `app_get_binding`, `app_list_bindings`
- `InProcessBackend` implementation (delegates to `MetaStore`)
- Root store KV scanning for app definitions (`apps/` prefix)
- Resolved app merging: root store definition + local binding state
- REST API for app management (`/api/apps`)
- Embedded app bundles (`rust-embed` for inventory app)
- Subdomain extraction from `Host` header
- App shell HTML generation with `<meta>` tags
- WebSocket tunnel shared between management UI and apps
- Lattice SDK reads `<meta>` tags and connects to the correct store

### Not Yet Done

- **Store claiming via SystemOp**: `SetAppId` / `ClearAppId` — requires adding the op variant to the system state machine. Once done, replaces KV scanning.
- **Claim-aware discovery**: filtered store listing by `app-id`
- **Auto-provisioning**: combined create + claim + bind endpoint
- **RPC backend support**: app bindings over RPC (currently returns `NotSupported` — bindings are node-local)
- **Dynamic app types**: loading app bundles at runtime instead of compile-time embedding

## Security Considerations

- **Binding is node-local**: a remote peer cannot force a node to serve an app. The node operator explicitly creates bindings.
- **App definitions are mesh-wide**: any authorized peer can register/unregister apps in the root store. This follows the same trust model as other store operations.
- **Store validation**: the register API validates that the referenced data store exists before writing the definition.
