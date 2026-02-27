---
title: "Native FFI Bindings"
---

Lattice is designed to be embedded directly into mobile and desktop applications. The `lattice-bindings` crate leverages Mozilla's UniFFI to generate native Swift and Kotlin interfaces.

## The `Lattice` Singleton

In your mobile application, you initialize the core engine:

```swift
// Swift Example
let dataDir = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0].path
let lattice = try Lattice.new(dataDir: dataDir)

// Start the runtime (blocks until ready)
try lattice.start()
```

This single instance hosts the `StoreManager` and the `NetworkService` running inside asynchronous Tokio threads.

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
// Subscribe to all events
let stream = try lattice.subscribe()

for await event in stream {
    switch event {
    case .storeReady(let storeId):
        print("Store \(storeId) finished bootstrapping!")
    case .joinFailed(let reason):
        print("Join failed: \(reason)")
    case .syncResult(let storeId, let peersSynced, let sent, let recv):
        print("Synced! Received \(recv) new intentions")
    }
}
```

Note: The `BackendEvent` enum currently includes `StoreReady`, `JoinFailed`, and `SyncResult` variants. `PeerConnected` does not exist in the current implementation.

## Handling Identifiers

To prevent string-parsing errors in mobile apps, the FFI boundary uses standard Strings for IDs. Under the hood, UniFFI translates these to the appropriate Rust types (`Uuid`).

## Dynamic Store Operations

For typed store operations (get, put, watch), use the dynamic store API through the backend. The store's protobuf schema is fetched at runtime, enabling mobile apps to discover and invoke methods without recompilation.
