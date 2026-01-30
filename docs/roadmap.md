# Lattice Roadmap

## Completed

| Milestone        | Summary                                                                                                                                       |
|------------------|-----------------------------------------------------------------------------------------------------------------------------------------------|
| **M1–M3**        | Single-node log with SigChain/HLC, DAG conflict resolution (LWW), async actor pattern, two-node sync via Iroh, Mesh API with token-based join |
| **M4**           | Generic RSM platform: extracted `lattice-model`, `lattice-kvstore`, WAL-first persistence, gRPC introspection                                 |
| **M5**           | Atomic multi-key transactions: `batch().put(k1,v1).put(k2,v2).commit()`                                                                       |
| **M6**           | Multi-store: root store as control plane, StoreManager with live reconciliation, per-store gossip                                             |
| **Decoupling**   | Node/Net separation via `NodeProvider` traits, `NetEvent` channel owned by net layer, unified StoreManager                                    |
| **Kernel Audit** | Orphan resolution refactor, `MAX_CAUSAL_DEPS` limit, zero `.unwrap()` policy, clean module hierarchy                                          |

## Milestone 7: Client/Daemon Split & Service Architecture

**Goal**: Refactor the codebase into a clean Library/Daemon/Client split, enabling both headless operation and embedded usage (iOS), then build the Service Architecture on top.

**Architecture Split**:
- `lattice-node` (Library): The core logic. Embeddable in iOS/GUI apps.
- `lattice-daemon` (Binary): The headless Unix daemon. Runs the Library + RPC Server.
- `lattice-cli` (Binary): The control interface. Connects to Daemon via RPC.

### 7A: Library Extraction
- [x] Ensure `lattice-node` is a pure library (no CLI/binary code)
- [x] Move `main.rs` orchestration logic out of library

### 7B: Daemon (`latticed`)
- [x] New `lattice-daemon` crate with `latticed` binary
- [x] Hosts `Node`, P2P Networking, Storage
- [x] Exposes gRPC API over UDS (`latticed.sock` in data dir)
- [x] File permissions (protected by parent directory)
- [x] Logging/tracing setup with verbosity flag
- [x] Graceful shutdown with signal handling

### 7C: RPC Protocol (`lattice-rpc`)
- [x] Define `NodeService`, `MeshService`, `StoreService`, `DynamicStoreService` in daemon.proto
- [x] Implement `lattice-rpc` crate with tonic UDS server
- [x] Implement MeshService RPCs (Create, List, GetStatus, Join, Peers, Invite, Revoke)
- [x] Implement StoreService RPCs (Create, List, GetStatus, Delete, Sync, Debug, History, AuthorState, OrphanCleanup)
- [x] Implement DynamicStoreService (Exec, ListMethods)

### 7D: CLI Client
- [x] `LatticeBackend` trait abstraction (backend.rs)
- [x] `InProcessBackend` - wraps Node/MeshService (embedded mode)
- [x] `RpcBackend` - wraps RpcClient (daemon mode)
- [x] --daemon/-d flag selects RPC mode
- [x] All command handlers use &dyn LatticeBackend
- [x] Dynamic command RPC encoding (store_exec via RPC)

### 7E: RPC Event Streaming
- [x] gRPC streaming endpoint for `NodeEvent` subscription (new stores, meshes, join results)
- [x] `store sync` RPC returns actual sync results (MeshService passed to RPC server)

### 7F: Post-Unification Cleanup & Optimization
- [x] **Refactor `RpcBackend`**: Remove `Mutex<RpcClient>` bottleneck (use cheap `Clone`)
- [x] **Secure Socket**: Set `0600` permissions on UDS (currently world-readable based on umask)
- [x] **Fix Connection Leaks**: `subscribe()` clones existing client (cheap) instead of opening new connection
- [x] **Optimize Dynamic Exec**: Cache `FileDescriptorSet` in `RpcBackend` to avoid fetching on every command
- [x] **Reduce Boilerplate**: Unify DTOs via `TryFrom` traits in `conversions.rs`
- [x] **Error Handling**: Replace string errors with proper `ErrorCode` enum in Protobuf

---

## Milestone 8: Reflection & Introspection

**Goal:** Enable powerful, generic UIs (like the iOS App and CLI) to introspect any Lattice Store and dynamically build rich interfaces without hardcoded logic. Address the "Shallow Introspection" gap by providing deep type schemas and semantic hints.

**Patterns:**
1.  **Deep Type Exposure:** `store_get_type_schema(name)` returns full struct/enum definition.
2.  **Semantic UI Hints:** `.proto` options (`ui.widget="slider"`) drive UI rendering.
3.  **Command vs Query:** Explicitly distinguish Read/Write methods for Dashboard vs Action UI.

### 8A: Unified API Generation (`lattice-api`)
- [x] **Centralized DTOs**: Move `NodeStatus`, `MeshInfo`, etc., to `daemon.proto` as single source of truth.
- [x] **Create `lattice-api` crate**:
  - [x] Configure `prost-build` to derive `uniffi::Record` on generated structs.
  - [x] Zero-copy reuse in `lattice-node` and `lattice-bindings` (skip duplicate generation).
- [x] **Architecture Refactor**:
  - [x] `LatticeBackend` trait in `lattice-api` (services use `Arc<dyn LatticeBackend>`).
  - [x] RPC services (`NodeServiceImpl`) accept `Backend` trait object.
  - [x] `lattice-runtime` provides `InProcessBackend` and `RpcBackend` implementations.

### 8B: Enhanced Reflection (Bindings)
- [x] **Update `FieldType`**: specific variants `Message(String)` and `Enum(String)` carrying type names.
- [x] **Implement `store_inspect_type(name)`**: Returns recursive schema info.
- [x] **TypeNotFound Error**: Specific error variant for type lookup failures.
- [x] **Recursion Depth Limit**: Max 16 levels to prevent stack overflow.
- [x] **Enum Name Resolution**: Resolves enum values to names via FieldDescriptor.

### 8C: Generic UI Support
- [x] **Structured Results**: `store_exec_dynamic` returns recursive `ReflectValue` (instead of raw `Vec<u8>`).
- [x] **Unicode Width**: CLI tables use `unicode-width` for proper alignment.

### 8D: Store Watchers & Event Streaming
> **Note:** `KvHandle::watch(pattern)` already exists internally. This milestone exposes it via RPC/FFI with a **reflectable** design so any state machine can define streams.

**Design**: Extend introspection pattern to streams (like `ListMethods()` for commands):
- `ListStreams(store_id)` → returns available subscriptions with schemas
- `Subscribe(store_id, stream_name, params)` → generic streaming endpoint

**Examples of RSM-defined streams:**
- **KvStore**: `watch(pattern)`, `scan_changes(prefix)`
- **LogStore**: `tail(from_seq)`, `follow()`
- **Custom RSMs**: Define arbitrary event sources

**Tasks:**
- [ ] **Define `StoreStream` introspection**: name, param schema, event schema
- [ ] **Define `StoreEvent` proto**: Generic wrapper or per-stream typed events
- [ ] **Expose `store_subscribe(store_id, stream, params)` RPC**: Streaming endpoint
- [ ] **Trait extension**: `StateMachine::streams()` returns available stream descriptors
- [ ] **LatticeBackend**: Add `subscribe(store_id, stream, params) -> Stream<StoreEvent>`
- [ ] **FFI bindings**: Type-safe stream subscriptions

---

## Milestone 9: Abstract Storage Backend

**Goal:** Abstract the underlying key-value database so both `LogStore` and `KvStore` use a generic interface instead of `redb` directly. Enables swappable backends (redb, SQLite, in-memory, flash storage for embedded).

```
┌─────────────────────────────────────────┐
│  State Machines (LogStore, KvStore)     │  → writes Collection::Data
├─────────────────────────────────────────┤
│  Kernel (SigChainManager)               │  → writes Collection::Meta
├─────────────────────────────────────────┤
│  StorageBackend trait + Collections     │  ← This milestone
├─────────────────────────────────────────┤
│  RedbBackend │ MemoryBackend │ Flash    │
└─────────────────────────────────────────┘
```

**Column Families (Namespace Isolation):**
| Collection | Purpose | Who writes |
|------------|---------|------------|
| `Data` (0) | User key-value space | State machine |
| `Meta` (1) | Frontiers, tips, state hash | Kernel |
| `Index` (2) | Sidecar indexes (Negentropy) | Kernel |

- [ ] **Define Backend Traits** (`lattice-model`):
  - `Collection` enum: `Data = 0`, `Meta = 1`, `Index = 2`
  - `StorageBackend` trait: `get(col, key)`, `transaction()`
  - `StorageTransaction` trait: `get`, `set`, `delete`, `scan`, `commit`
- [ ] **Implement RedbBackend** (production):
  - Map collections to separate redb tables
  - Atomic transactions across all collections
- [ ] **Implement MemoryBackend** (tests):
  - Prefix-encode collection ID: `[col_id][key]`
  - BTreeMap-based for fast unit tests
- [ ] **Kernel owns `Collection::Meta`**:
  - After `apply_op()`, kernel writes chain tips and frontiers
  - State machine only sees `Collection::Data`
  - Clean separation: kernel owns chain, state machine owns data
- [ ] **Refactor LogStore & KvStore**:
  - Replace direct `redb` calls with `StorageBackend` trait
  - State machines write to `Collection::Data` only

## Milestone 10: The Weaver Protocol (Data Model Refactor)

**Goal:** Implement the "Ly" data model by splitting the monolithic `Entry` into `Intention` (Transport-Agnostic Signal) and `Witness` (Chain-Specific Ordering). This decoupling enables "Ghost Parent" resolution and selective data sharing.

> **See:** [Ly Architecture](ly_architecture.md) for full protocol details.

### 10A: The Intention
- [ ] Define `Intention` struct: `{ payload: Vec<u8>, author: PubKey, seq: u64, prev_hash: Hash, sig: Signature }`
- [ ] Implement `Intention` validation (author signature check + seq/hash continuity)
- [ ] Ensure `Intention` is valid independent of any chain (floating message)

### 10B: The Witness
- [ ] Define `Witness` event in `Entry`: `Entry { parent: Hash, event: Witness(Intention), sig: MySig }`
- [ ] Update `SigChain` to append `Witness` events
- [ ] Implement "Ingest/Digest" envelope pattern for Gossip

---

## Milestone 11: Concrete RootStore Implementation

**Goal:** Move root store functionality from Node into a specialized store implementation with proper business logic.

> Currently, peer/store management logic is scattered in `Node` and `Mesh`. This milestone consolidates it into a proper state machine with typed operations.

### 11A: RootStore State Machine
- [ ] Define `RootOp` enum: `AddPeer`, `RevokePeer`, `DeclareStore`, `RemoveStore`
- [ ] Create `RootState` implementing `StateMachine` trait
- [ ] `apply(op, store: &mut dyn StateStorage)` enforces invariants (e.g., "only admins can add peers")

### 11B: Refactor Node/Mesh
- [ ] `PeerManager` submits `RootOp::AddPeer` instead of writing raw keys
- [ ] `StoreManager` submits `RootOp::DeclareStore` instead of direct writes
- [ ] Remove ad-hoc `/nodes/*` and `/stores/*` key manipulation
- [ ] Unified Store Registry: `Node` becomes single source of truth (eliminate race conditions with `MeshNetwork`)

### 11C: Access Control
- [ ] Define admin role in root store (`/acl/admins/{pubkey}`)
- [ ] Policy enforcement in `RootState::apply()`

---

## Milestone 12: Negentropy Sync Protocol

**Goal:** Replace O(n) vector clock sync with sub-linear bandwidth using range-based set reconciliation.

Range-based set reconciliation using hash fingerprints. Used by Nostr ecosystem.

### 12A: Infrastructure
- [ ] Add hash→entry index (for efficient fetch-by-hash)
- [ ] Implement negentropy fingerprint generation per store

### 12B: Protocol Migration
- [ ] Replace `SyncState` protocol with negentropy exchange
- [ ] Decouple `seq` from network sync protocol (keep internal only)
- [ ] Update `FetchRequest` to use hashes instead of seq ranges

**Current `seq` Dependencies to Migrate:**
| Component | Current | Negentropy Approach |
|-----------|---------|---------------------|
| `SyncState.diff()` | `MissingRange{from_seq, to_seq}` | Hash fingerprint exchange → list of missing hashes |
| `FetchRequest.ranges` | `{author, from_seq, to_seq}` | Fetch by hash directly |
| `Log::iter_range()` | Range by seq | Need hash→entry index for lookup |
| `GapInfo` | Triggers sync when `seq > next_seq` | "Missing prev_hash X" → fetch by hash |

**What to Keep:**
- `seq` for **local sigchain validation** (prevents insertion attacks, enforces append-only)
- `ChainTip.seq` as internal implementation detail

- **Ref:** [Negentropy Protocol](https://github.com/hoytech/negentropy)

---

## Milestone 13: Wasm Runtime

**Goal:** Replace hardcoded state machine logic with dynamic Wasm modules.

### 13A: Wasm Integration
- [ ] Integrate `wasmtime` into the Kernel
- [ ] Define minimal Host ABI: `kv_get`, `kv_set`, `log_append`, `get_head`
- [ ] Replace hardcoded `KvStore::apply()` with `WasmRuntime::call_apply()`
- [ ] Map Host Functions to `StorageBackend` calls (M8 prerequisite)

### 13B: Data Structures & Verification
- [ ] Finalize `Entry`, `SignedEntry`, `Hash`, `PubKey` structs for Wasm boundary
- [ ] Wasm-side SigChain verification (optional, for paranoid clients)
- **Deliverable:** A "Counter" Wasm module that increments a value when it receives an Op

---

## Milestone 14: Content-Addressable Store (CAS)

**Goal:** Blob storage for large files (prerequisite for Lattice Drive). We use **CIDs (Content Identifiers)** for self-describing formats and IPFS/IPLD compatibility.

### 14A: CAS Integration
- [ ] `lattice-cas` crate with pluggable backend (local FS, Garage S3)
- [ ] `put_blob(data) -> CID`, `get_blob(CID) -> data`

### 14B: Metadata & Pinning
- [ ] `/cas/pins/{node_id}/{hash}` in root store
- [ ] Pin reconciler: watch pins, trigger fetch

### 14C: CLI
- [ ] `cas put`, `cas get`, `cas pin`, `cas ls`

---

## Milestone 15: Lattice Drive MVP

**Goal:** A functioning file sync tool that proves the architecture.

> **Depends on:** M13 (Wasm) + M14 (CAS)

### 15A: Filesystem Logic
- [ ] Write `filesystem.wasm` (in Rust, compiled to wasm32-wasi)
- [ ] Map file operations (`write`, `mkdir`, `rename`) to Log Ops
- [ ] Store file content in CAS, reference hashes in sigchain

### 15B: FUSE Interface
- [ ] Write `lattice-fs` using the `fuser` crate
- [ ] FUSE talks to Daemon via same gRPC interface as CLI
- [ ] Mount the Store as a folder on Linux/macOS
- **Demo:** `cp photo.jpg ~/lattice/` → Syncs to second node

---

## Milestone 16: Embedded Proof ("Lattice Nano")

**Goal:** Running the Kernel on the RP2350.

> Because CLI is already separated from Daemon (M7) and storage is abstracted (M8), only the Daemon needs porting.

### 16A: `no_std` Refactoring
- [ ] Split `lattice-kernel` into `core` (logic) and `std` (IO)
- [ ] Replace `wasmtime` (JIT) with `wasmi` (Interpreter) for embedded target
- [ ] Port storage layer to `sequential-storage` (Flash) via `StorageBackend`

### 16B: The "Continuity" Demo
- [ ] Build physical USB stick prototype
- [ ] Implement BLE/Serial transport
- **The Reveal:** Sync a file from Laptop → Stick → Phone without Internet

---

## Milestone 17: Log Lifecycle & Pruning

**Goal:** Enable long-running nodes to manage log growth through snapshots, pruning, and finality checkpoints.

> **See:** [DAG-Based Pruning Architecture](pruning.md) for details on Ancestry-based metrics and Log Rewriting.

### 17A: Snapshotting
- [ ] `state.snapshot()` when log grows large
- [ ] Store snapshots in `snapshot.db`
- [ ] Bootstrap new peers from snapshot instead of full log replay

### 17B: Waterlevel Pruning
- [ ] Calculate stability frontier (min seq acknowledged by all peers)
- [ ] `truncate_prefix(seq)` for old log entries
- [ ] Preserve entries newer than frontier

### 17C: Checkpointing / Finality
- [ ] Periodically finalize state hash (protect against "Deep History Attacks")
- [ ] Signed checkpoint entries in sigchain
- [ ] Nodes reject entries that contradict finalized checkpoints

### 17D: Hash Index Optimization
- [ ] Replace in-memory `HashSet<Hash>` with on-disk index (redb) or Bloom Filter
- [ ] Support 100M+ entries without excessive RAM

---

## Technical Debt

- [ ] **REGRESSION**: history command list filtering (backend side) capability
- [ ] **REGRESSION**: Graceful reconnect after sleep/wake (may fix gossip regression)
- [ ] **Store Name Lookup Optimization**: `find_store_name()` in `store_service.rs` and `backend_inprocess.rs` does O(meshes × stores) linear search. Store names live in mesh root KV stores (StoreDeclaration). Consider caching in StoreManager or adding index.
- [ ] **Data Directory Lock File**: Investigate lock file mechanism to prevent multiple processes from using the same data directory simultaneously (daemon + embedded app conflict). Options: flock, PID file, or socket-based detection.
- [ ] **Denial of Service (DoS) via Gossip**: Implement rate limiting in GossipManager and drop messages from peers who send invalid data repeatedly.
- [ ] **Payload Validation Strategy**: Decide where semantic validation occurs and what happens on failure. Options: build-time only, versioned rules, entry replacement, or separate chain/payload advancement. See `test_rejected_entry_breaks_chain` in `lattice-kvstore/src/kv.rs`.

---

## Future

- TTL expiry for long-lived orphans (received_at timestamp now tracked)
- Mobile clients (iOS/Android)
- Node key rotation
- Secure storage of node key (Keychain, TPM)
- **Salted Gossip ALPN**: Use `/config/salt` from root store to salt the gossip ALPN per mesh (improves privacy by isolating mesh traffic).
- **HTTP API**: External access to stores via REST/gRPC-Web (design TBD based on store types)
- **Hierarchical Store Model**: Tree of stores where any store can spawn child stores with scoped permissions
