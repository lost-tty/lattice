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

## Milestone 8: Abstract Storage Backend

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

## Milestone 9: Concrete RootStore Implementation

**Goal:** Move root store functionality from Node into a specialized store implementation with proper business logic.

> Currently, peer/store management logic is scattered in `Node` and `Mesh`. This milestone consolidates it into a proper state machine with typed operations.

### 9A: RootStore State Machine
- [ ] Define `RootOp` enum: `AddPeer`, `RevokePeer`, `DeclareStore`, `RemoveStore`
- [ ] Create `RootState` implementing `StateMachine` trait
- [ ] `apply(op, store: &mut dyn StateStorage)` enforces invariants (e.g., "only admins can add peers")

### 9B: Refactor Node/Mesh
- [ ] `PeerManager` submits `RootOp::AddPeer` instead of writing raw keys
- [ ] `StoreManager` submits `RootOp::DeclareStore` instead of direct writes
- [ ] Remove ad-hoc `/nodes/*` and `/stores/*` key manipulation
- [ ] Unified Store Registry: `Node` becomes single source of truth (eliminate race conditions with `MeshNetwork`)

### 9C: Access Control
- [ ] Define admin role in root store (`/acl/admins/{pubkey}`)
- [ ] Policy enforcement in `RootState::apply()`

---

## Milestone 10: Negentropy Sync Protocol

**Goal:** Replace O(n) vector clock sync with sub-linear bandwidth using range-based set reconciliation.

Range-based set reconciliation using hash fingerprints. Used by Nostr ecosystem.

### 9A: Infrastructure
- [ ] Add hash→entry index (for efficient fetch-by-hash)
- [ ] Implement negentropy fingerprint generation per store

### 9B: Protocol Migration
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

## Milestone 11: Wasm Runtime

**Goal:** Replace hardcoded state machine logic with dynamic Wasm modules.

### 11A: Wasm Integration
- [ ] Integrate `wasmtime` into the Kernel
- [ ] Define minimal Host ABI: `kv_get`, `kv_set`, `log_append`, `get_head`
- [ ] Replace hardcoded `KvStore::apply()` with `WasmRuntime::call_apply()`
- [ ] Map Host Functions to `StorageBackend` calls (M8 prerequisite)

### 11B: Data Structures & Verification
- [ ] Finalize `Entry`, `SignedEntry`, `Hash`, `PubKey` structs for Wasm boundary
- [ ] Wasm-side SigChain verification (optional, for paranoid clients)
- **Deliverable:** A "Counter" Wasm module that increments a value when it receives an Op

---

## Milestone 12: Content-Addressable Store (CAS)

**Goal:** Blob storage for large files (prerequisite for Lattice Drive).

### 12A: CAS Integration
- [ ] `lattice-cas` crate with pluggable backend (local FS, Garage S3)
- [ ] `put_blob(data) -> hash`, `get_blob(hash) -> data`

### 12B: Metadata & Pinning
- [ ] `/cas/pins/{node_id}/{hash}` in root store
- [ ] Pin reconciler: watch pins, trigger fetch

### 12C: CLI
- [ ] `cas put`, `cas get`, `cas pin`, `cas ls`

---

## Milestone 13: Lattice Drive MVP

**Goal:** A functioning file sync tool that proves the architecture.

> **Depends on:** M11 (Wasm) + M12 (CAS)

### 13A: Filesystem Logic
- [ ] Write `filesystem.wasm` (in Rust, compiled to wasm32-wasi)
- [ ] Map file operations (`write`, `mkdir`, `rename`) to Log Ops
- [ ] Store file content in CAS, reference hashes in sigchain

### 13B: FUSE Interface
- [ ] Write `lattice-fs` using the `fuser` crate
- [ ] FUSE talks to Daemon via same gRPC interface as CLI
- [ ] Mount the Store as a folder on Linux/macOS
- **Demo:** `cp photo.jpg ~/lattice/` → Syncs to second node

---

## Milestone 14: Embedded Proof ("Lattice Nano")

**Goal:** Running the Kernel on the RP2350.

> Because CLI is already separated from Daemon (M7) and storage is abstracted (M8), only the Daemon needs porting.

### 14A: `no_std` Refactoring
- [ ] Split `lattice-kernel` into `core` (logic) and `std` (IO)
- [ ] Replace `wasmtime` (JIT) with `wasmi` (Interpreter) for embedded target
- [ ] Port storage layer to `sequential-storage` (Flash) via `StorageBackend`

### 14B: The "Continuity" Demo
- [ ] Build physical USB stick prototype
- [ ] Implement BLE/Serial transport
- **The Reveal:** Sync a file from Laptop → Stick → Phone without Internet

---

## Milestone 15: Log Lifecycle & Pruning

**Goal:** Enable long-running nodes to manage log growth through snapshots, pruning, and finality checkpoints.

### 15A: Snapshotting
- [ ] `state.snapshot()` when log grows large
- [ ] Store snapshots in `snapshot.db`
- [ ] Bootstrap new peers from snapshot instead of full log replay

### 15B: Waterlevel Pruning
- [ ] Calculate stability frontier (min seq acknowledged by all peers)
- [ ] `truncate_prefix(seq)` for old log entries
- [ ] Preserve entries newer than frontier

### 15C: Checkpointing / Finality
- [ ] Periodically finalize state hash (protect against "Deep History Attacks")
- [ ] Signed checkpoint entries in sigchain
- [ ] Nodes reject entries that contradict finalized checkpoints

### 15D: Hash Index Optimization
- [ ] Replace in-memory `HashSet<Hash>` with on-disk index (redb) or Bloom Filter
- [ ] Support 100M+ entries without excessive RAM

---

## Technical Debt

- [ ] **REGRESSION**: Graceful reconnect after sleep/wake (may fix gossip regression)
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
