---
title: "Native FFI Bindings"
---

Lattice is designed to be embedded directly into mobile and desktop applications. The `lattice-bindings` crate leverages Mozilla's UniFFI to generate native Swift and Kotlin interfaces.

## The `Lattice` Singleton

In your mobile application, you initialize the core engine:

```swift
// Swift Example
let dataDir = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0].path
let lattice = Lattice.new(dataDir: nil)  // constructor is infallible; dataDir is passed to start()

// Start the runtime — data_dir and display name are passed here
try lattice.start(dataDir: dataDir, name: nil)
```

The `Lattice` singleton owns a Tokio runtime and wraps a `lattice_runtime::Runtime`, which in turn holds the `Node` (with `StoreManager`) and `NetworkService`.

## Store Management

You can directly interact with stores:

```swift
// Create a new store
let storeInfo = try lattice.storeCreate(parentId: nil, name: "My Notes", storeType: "core:kvstore")

// Join an existing store via an invite token
let response = try lattice.storeJoin(token: "invite-token-abc123")
```

## Event Streaming

UI frameworks (like SwiftUI) need to know when data changes without polling. The `lattice-bindings` crate exposes a global event stream:

```swift
// Subscribe to all events — returns Arc<LatticeEventStream>
let stream = lattice.subscribe()

// Poll events with next() — returns Optional<BackendEvent>
while let event = await stream.next() {
    switch event {
    case .storeReady(let rootId, let storeId):
        print("Store \(storeId) finished bootstrapping!")
    case .joinFailed(let rootId, let reason):
        print("Join failed: \(reason)")
    case .syncResult(let storeId, let peersSynced, let entriesSent, let entriesReceived):
        print("Synced! Received \(entriesReceived) new intentions")
    }
}
```

`BackendEvent` variants: `StoreReady { root_id, store_id }`, `JoinFailed { root_id, reason }`, `SyncResult { store_id, peers_synced, entries_sent, entries_received }`. Note that `root_id` and `store_id` are `Vec<u8>` (raw UUID bytes), not Strings.

## Handling Identifiers

The FFI boundary uses `String` for input IDs (e.g., `store_id` parameters), which the Rust layer converts to `Uuid` via `Uuid::parse_str()`. Event payloads use `Vec<u8>` for IDs (raw UUID bytes). UniFFI does not perform automatic type conversion — all String-to-Uuid parsing is explicit in the Rust FFI layer.

## Dynamic Store Operations

For typed store operations (get, put, watch), use the dynamic store API through the backend. The store's protobuf schema is fetched at runtime, enabling mobile apps to discover and invoke methods without recompilation.
