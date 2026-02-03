# Lattice Store Implementation Guide

This document outlines the contract for implementing a new store type in Lattice (e.g., `KvStore`, `LogStore`).

## Overview

A Lattice Store is a replicated state machine built on the **Handle-Less Architecture**. It consists of:
1.  **State Logic**: Pure business logic (apply ops, query state) wrapped in `PersistentState<T>`.
2.  **Reflection**: `Introspectable` and `Descriptor` for dynamic CLI/RPC support.
3.  **Command Dispatch**: `Dispatcher` trait for executing dynamic commands.
4.  **Streams**: `StreamProvider` for real-time events.

## 1. State Machine Implementation (`StateLogic`)

Implement the `StateLogic` trait (from `lattice-storage/src/state_db.rs`). This decouples your logic from the boilerplate of storage, chain verification, and identity management.

```rust
use lattice_storage::{StateBackend, StateDbError, StateLogic, Op, PersistentState};
use lattice_model::Hash;

pub struct MyState {
    backend: StateBackend,
    // Add internal channels (e.g. broadcast::Sender) here
}

impl StateLogic for MyState {
    type Updates = Vec<MyEvent>; // Your verified change events

    fn backend(&self) -> &StateBackend {
        &self.backend
    }

    /// Decode Op payload and apply mutations to redb table.
    /// Returns: (Notification Events, Identity Diff)
    fn mutate(&self, table: &mut redb::Table<&[u8], &[u8]>, op: &Op) -> Result<(Self::Updates, Hash), StateDbError> {
        // 1. Decode payload (Protobuf)
        let payload = MyPayload::decode(op.payload.as_ref())?;
        
        // 2. Apply changes to table
        // table.insert(key, value)?;
        
        // 3. Calculate Identity XOR Diff (optional, returned as Hash)
        Ok((events, identity_diff))
    }

    /// Notify watchers (outside transaction)
    fn notify(&self, updates: Self::Updates) {
        // Send events to subscribers
    }
}
```

### The `PersistentState` Wrapper
You normally don't implement `StateMachine` directly. Instead, you wrap your logic in `PersistentState<T>`:

```rust
use lattice_storage::setup_persistent_state;

impl MyState {
    pub fn open(id: Uuid, dir: &Path) -> Result<PersistentState<Self>, StateDbError> {
        setup_persistent_state(id, dir, |backend| {
             Self { backend, ... }
        })
    }
}
```

`PersistentState<T>` handles:
- Chain verification (prev_hash links)
- Idempotency checks
- Rolling state hash updates
- Atomic commits
- Snapshots/Restores via the backend

## 2. Reflection & Introspection

Lattice uses `prost-reflect` to allow the generic CLI and RPC layer to interact with any store without recompilation.

### Service Descriptor
Define your store's API in a `.proto` file:

```protobuf
service MyStore {
  rpc Put (PutRequest) returns (PutResponse);
  rpc Get (GetRequest) returns (GetResponse);
}
```

Embed the FileDescriptorSet in your Rust code and implement `Introspectable`:

```rust
impl Introspectable for MyState {
    fn service_descriptor(&self) -> prost_reflect::ServiceDescriptor {
        // Return the ServiceDescriptor for "MyStore"
        POOL.get_service_by_name("lattice.my.MyStore").unwrap()
    }
    
    // ... implement other methods like command_docs, field_formats ...
}
```

## 3. Command Dispatch (`Dispatcher`)

The `Dispatcher` trait maps dynamic command strings (e.g., "Put") to your logic.

```rust
use lattice_store_base::{Dispatcher, dispatch::dispatch_method};

impl Dispatcher for MyState {
    fn dispatch<'a>(
        &'a self,
        writer: &'a dyn StateWriter, // Injection for creating ops
        method_name: &'a str,
        request: DynamicMessage,
    ) -> Pin<Box<dyn Future<Output = Result<DynamicMessage, ...>> + Send + 'a>> {
        let desc = self.service_descriptor();
        Box::pin(async move {
            match method_name {
                "Put" => dispatch_method(method_name, request, desc, |req| self.handle_put(writer, req)).await,
                "Get" => dispatch_method(method_name, request, desc, |req| self.handle_get(req)).await,
                _ => Err("Unknown method".into()),
            }
        })
    }
}
```

## 4. Initialization (`StoreOpener`)

To register your store with `lattice-node`, provide a `StoreOpener` factory.

```rust
// In your crate (creating the StoreHandle)
pub fn opener(registry: &StoreRegistry) -> Box<dyn StoreOpener> {
    // Return a closure or struct that calls MyState::open() 
    // and returns Store<PersistentState<MyState>> cast as Arc<dyn StoreHandle>
}
```

## 5. Directory Structure Recommendation

```
lattice-mystore/
├── src/
│   ├── lib.rs
│   ├── state.rs      # StateLogic, Introspectable, Dispatcher implementation
│   ├── proto/        # Generated protobuf code
│   └── tests/        # Integration tests
├── build.rs          # Proto compilation
└── Cargo.toml
```
