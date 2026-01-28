# Lattice Bindings API

This document describes the native interface exposed to Swift (via UniFFI) and Kotlin. It mirrors the capability of the Lattice RPC API but is designed for direct in-process usage.

## Generating Bindings

To generate Swift and Kotlin bindings:

1. ensure you have initialized the submodules (if any) and have a standard Rust dev environment.
2. Run the generation script:

```bash
./scripts/generate-bindings.sh
```

This will output:
- `generated-bindings/swift/`: Swift source and modulemap
- `generated-bindings/kotlin/`: Kotlin source

## Core Types

### `Lattice`

The main entry point, wrapping the Lattice Runtime.

```rust
// Swift: Lattice
class Lattice {
    // Constructor: Starts the runtime.
    // data_dir: Optional path. If nil, uses platform default (e.g. app sandbox).
    init(data_dir: String?) throws
    
    // Identity
    func nodeId() -> String
    func setName(name: String) throws
    
    // Mesh Management
    func meshList() throws -> [MeshInfo]
    func meshCreate() throws -> MeshInfo
    func meshStatus(meshId: String) throws -> MeshInfo
    func meshJoin(token: String) throws -> JoinResponse
    func meshInvite(meshId: String) throws -> String
    func meshPeers(meshId: String) throws -> [PeerInfo]
    func meshRevoke(meshId: String, peerKey: Data) throws
    
    // Store Management
    func storeCreate(meshId: String, name: String?, storeType: String) throws -> StoreInfo
    func storeList(meshId: String) throws -> [StoreInfo]
    func storeStatus(storeId: String) throws -> StoreStatus
    func storeDelete(storeId: String) throws
    func storeSync(storeId: String) throws
    func storeDebug(storeId: String) throws -> DebugInfo
    func storeHistory(storeId: String, key: String?) throws -> [HistoryEntry]
    func storeAuthorState(storeId: String, author: Data?) throws -> [AuthorState]
    func storeOrphanCleanup(storeId: String) throws -> CleanupResult
    
    // Dynamic Execution
    func storeExec(storeId: String, method: String, payload: Data) throws -> Data
    func storeListMethods(storeId: String) throws -> [MethodInfo]
    func storeGetDescriptor(storeId: String) throws -> DescriptorResponse
    
    // Events
    func subscribe() -> LatticeEventStream
}
```

## Data Types

### `MeshInfo`
```rust
struct MeshInfo {
    id: String
    alias: String
    peerCount: UInt32
    storeCount: UInt32
}
```

### `StoreInfo`
```rust
struct StoreInfo {
    id: String
    name: String?
    storeType: String
    archived: Bool
}

### `StoreStatus`
```rust
struct StoreStatus {
    id: String
    storeType: String
    authorCount: UInt32
    logFileCount: UInt32
    logBytes: UInt64
    orphanCount: UInt32
}
```
```

### `JoinResponse`
```rust
struct JoinResponse {
    meshId: String
    status: String
}
```

### `PeerInfo`
```rust
struct PeerInfo {
    publicKey: Data
    status: String
    online: Bool
    name: String?
    lastSeenMs: UInt64?
}
```

### `HistoryEntry`
```rust
struct HistoryEntry {
    seq: UInt64
    author: Data
    payload: Data
    timestamp: UInt64
    hash: Data
    prevHash: Data
    causalDeps: [Data]
    summary: String
}
```

### `AuthorState`
```rust
struct AuthorState {
    publicKey: Data
    seq: UInt64
    hash: Data
}
```

## Event Stream

### `LatticeEventStream`
An async stream of events from the backend.

```rust
// Swift
class LatticeEventStream {
    func next() async -> BackendEvent?
}
```

### `BackendEvent`
Enum representing possible events.

```rust
enum BackendEvent {
    case meshReady(meshId: String)
    case storeReady(meshId: String, storeId: String)
    case joinFailed(meshId: String, reason: String)
    case syncResult(storeId: String, peersSynced: UInt32, entriesSent: UInt64, entriesReceived: UInt64)
}
```
