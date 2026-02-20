# Native Bindings (FFI)

Lattice is designed to be embedded directly into mobile and desktop applications. The `lattice-bindings` crate leverages Mozilla's UniFFI to generate native Swift and Kotlin interfaces.

## The `Lattice` Singleton

In your mobile application, you initialize the core engine.

```swift
// Swift Example
let dataDir = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0].path
let lattice = try Lattice.new(storePath: dataDir)

// Wait for the background setup to complete
try lattice.waitReady()
```

This single instance hosts the `StoreManager` and the `NetworkService` running inside asynchronous Tokio threads.

## Store Management & FFI

You can directly interact with stores. Because the Swift/Kotlin runtimes are separated from Rust, `lattice-bindings` provides a simplified `StoreStatus` response.

```swift
let storeInfo = try lattice.storeCreate(storeId: "1234-5678", name: "My Notes", storeType: "kv")

// Join an existing store via an invite token
let response = try lattice.peerJoin(token: "invite-token-abc123")
```

## Event Streaming

UI frameworks (like SwiftUI) need to know when data changes without polling the database. The `lattice-bindings` crate exposes a global event stream.

```swift
// Subscribe to all events across all active stores (or pass a specific storeId)
let stream = try lattice.subscribeEvents(storeId: nil)

for await event in stream {
    switch event {
    case .storeReady(let storeId):
        print("Store \(storeId) finished bootstrapping!")
    case .peerConnected(let peerId, let storeId):
        print("Peer \(peerId) came online in \(storeId)")
    case .syncResult(let storeId, let peersSynced, let sent, let recv):
        print("Synced! Received \(recv) new intentions")
    default:
        break
    }
}
```

## Handling Identifiers

To prevent string-parsing errors in mobile apps, the FFI boundary relies strictly on Native Object Types or standard Strings for IDs. Under the hood, UniFFI translates `storeId` (Strings) directly to Redb `Uuid` primary keys.
