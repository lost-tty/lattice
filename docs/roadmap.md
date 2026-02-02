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
| **M7**           | Client/Daemon split: library extraction, `latticed` daemon with UDS gRPC, `LatticeBackend` trait, RPC event streaming, socket security        |
| **M8**           | Reflection & introspection: `lattice-api` crate, deep type schemas, `ReflectValue` structured results, store watchers & FFI stream bindings   |

## Milestone 9: Decouple Storage from State Machines ✓

**Goal:** Decouple state machine logic from concrete storage. `LogStore` and `KvStore` now operate through abstract traits (`StateBackend`, `StateLogic`) rather than direct `redb` calls. This simplifies testing and prepares for future backend options, but **redb remains the sole runtime implementation**.


- [x] **Define Backend Traits** (`lattice-model`):
  - `StateBackend`: wraps database, provides `verify_and_update_tip`, state hash
  - `StateLogic` trait: `apply()`, `backend()` for state machine implementations
  - `PersistentState<T>` wrapper: composes `StateLogic` with persistence
- [x] **Refactor Consumers**:
  - `lattice-kvstore` uses `StateLogic` / `PersistentState`
  - `lattice-logstore` uses `StateLogic` / `PersistentState`
  
### 9B: Unified Store Access API
> All callers (Node internals, CLI, RPC, FFI) use the same access path. Typed methods become thin wrappers around `dispatch()`.

**Design:**
```
All Callers → dispatch(method, DynamicMessage) → StateLogic::apply(Op)
                    ↑
Typed wrappers:  put(k,v) → dispatch("Put", PutRequest{k,v})
```

**Step 1: Batch via dispatch (prerequisite - Node uses batch heavily)**
- [x] Add `Batch` proto message (list of `BatchOp` with put/delete variants)
- [x] `CommandDispatcher::dispatch("Batch", msg)` delegates to `BatchBuilder`
- [x] Verify CLI and Node can batch through reflection API

**Step 2: Typed methods wrap dispatch**
- [x] `put(k,v)` builds `PutRequest`, calls `dispatch("Put", msg)`
- [x] Single audit path: all operations flow through dispatch
- [x] Refactor GetRequest/Response: use idiomatic Proto3 optional, support 'verbose' metadata

**Step 3: Simplify OpenedStore** 
- [ ] Remove `typed_handle` - only `Arc<dyn StoreHandle>` remains
- [ ] Mesh uses `store_handle.dispatch()` directly (or extension traits)

### 9C: Handle Consolidation
> Simplify store handle layer by unifying patterns across KvStore and LogStore.
- [x] Create `lattice-store-base` crate with shared reflection traits
  - Moved `Introspectable`, `CommandDispatcher`, `StreamReflectable` from `lattice-model`
  - Updated 8 consumer crates to import directly
- [x] Implement `StateProvider` trait with blanket `Introspectable` impl
  - KvHandle now uses blanket impl (~20 lines removed)
- [x] Create `lattice-mockkernel` crate with generic `MockWriter<S>`
  - Shared test infrastructure for stores without real replication
  - KvStore tests now use `MockWriter<KvState>` from shared crate
- [x] Migrate LogState to implement `Introspectable` (already in state.rs)
- [x] **Async Stream Subscription**: Refactor `StreamReflectable::subscribe` to return `BoxFuture`. This ensures setup errors (like invalid regex) are propagated before the stream begins.

### 9D: State Machine Unification
> Reduce duplication between `KvState` and `LogState` implementations.
- [x] **Standardize Open Boilerplate**: `setup_persistent_state<L>()` helper exists in `lattice-storage`
- [x] **Shared Apply Template**: Unified step structure (0→chain, 1→mutate, 2→identity, 3→notify) with matching comments

### 9E: State Machine Interface Documentation
> Clearly document the contract for implementing a new store type.
- [ ] Document `StateLogic` trait requirements and lifecycle
- [ ] Document `Openable` / `StoreOpener` patterns
- [ ] Document `ServiceDescriptor` integration for reflection
- [ ] Provide minimal example skeleton for new store implementations

---

## Milestone 10: Concrete RootStore Implementation

**Goal:** Move root store functionality from Node into a specialized store implementation with proper business logic.

> Currently, peer/store management logic is scattered in `Node` and `Mesh`. This milestone consolidates it into a proper state machine with typed operations.

### 10A: RootStore State Machine
- [ ] Define `RootOp` enum: `AddPeer`, `RevokePeer`, `DeclareStore`, `RemoveStore`
- [ ] Create `RootState` implementing `StateMachine` trait
- [ ] `apply(op, store: &mut dyn StateStorage)` enforces invariants (e.g., "only admins can add peers")

### 10B: Refactor Node/Mesh
- [ ] `PeerManager` submits `RootOp::AddPeer` instead of writing raw keys
- [ ] `StoreManager` submits `RootOp::DeclareStore` instead of direct writes
- [ ] Remove ad-hoc `/nodes/*` and `/stores/*` key manipulation
- [ ] Unified Store Registry: `Node` becomes single source of truth (eliminate race conditions with `MeshNetwork`)

### 10C: Access Control
- [ ] Define admin role in root store (`/acl/admins/{pubkey}`)
- [ ] Policy enforcement in `RootState::apply()`

---

## Milestone 11: Embedded Proof ("Lattice Nano")

**Goal:** Running the Kernel on the RP2350.

> Because CLI is already separated from Daemon (M7) and storage is abstracted (M9), only the Daemon needs porting.

### 11A: `no_std` Refactoring
- [ ] Split `lattice-kernel` into `core` (logic) and `std` (IO)
- [ ] Replace `wasmtime` (JIT) with `wasmi` (Interpreter) for embedded target
- [ ] Port storage layer to `sequential-storage` (Flash) via `StorageBackend`

### 11B: The "Continuity" Demo
- [ ] Build physical USB stick prototype
- [ ] Implement BLE/Serial transport
- **The Reveal:** Sync a file from Laptop → Stick → Phone without Internet

---

## Milestone 12: The Weaver Protocol (Data Model Refactor)

**Goal:** Implement the "Ly" data model by splitting the monolithic `Entry` into `Intention` (Transport-Agnostic Signal) and `Witness` (Chain-Specific Ordering). This decoupling enables "Ghost Parent" resolution and selective data sharing.

> **See:** [Ly Architecture](ly_architecture.md) for full protocol details.

### 12A: The Intention
- [ ] Define `Intention` struct: `{ payload: Vec<u8>, author: PubKey, seq: u64, prev_hash: Hash, sig: Signature }`
- [ ] Implement `Intention` validation (author signature check + seq/hash continuity)
- [ ] Ensure `Intention` is valid independent of any chain (floating message)

### 12B: The Witness
- [ ] Define `Witness` event in `Entry`: `Entry { parent: Hash, event: Witness(Intention), sig: MySig }`
- [ ] Update `SigChain` to append `Witness` events
- [ ] Implement "Ingest/Digest" envelope pattern for Gossip

---

## Milestone 13: Negentropy Sync Protocol

**Goal:** Replace O(n) vector clock sync with sub-linear bandwidth using range-based set reconciliation.

Range-based set reconciliation using hash fingerprints. Used by Nostr ecosystem.

### 13A: Infrastructure
- [ ] Add hash→entry index (for efficient fetch-by-hash)
- [ ] Implement negentropy fingerprint generation per store

### 13B: Protocol Migration
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

## Milestone 14: Wasm Runtime

**Goal:** Replace hardcoded state machine logic with dynamic Wasm modules.

### 14A: Wasm Integration
- [ ] Integrate `wasmtime` into the Kernel
- [ ] Define minimal Host ABI: `kv_get`, `kv_set`, `log_append`, `get_head`
- [ ] Replace hardcoded `KvStore::apply()` with `WasmRuntime::call_apply()`
- [ ] Map Host Functions to `StorageBackend` calls (M9 prerequisite)

### 14B: Data Structures & Verification
- [ ] Finalize `Entry`, `SignedEntry`, `Hash`, `PubKey` structs for Wasm boundary
- [ ] Wasm-side SigChain verification (optional, for paranoid clients)
- **Deliverable:** A "Counter" Wasm module that increments a value when it receives an Op

---

## Milestone 15: Content-Addressable Store (CAS)

**Goal:** Blob storage for large files (prerequisite for Lattice Drive). We use **CIDs (Content Identifiers)** for self-describing formats and IPFS/IPLD compatibility.

### 15A: CAS Integration
- [ ] `lattice-cas` crate with pluggable backend (local FS, Garage S3)
- [ ] `put_blob(data) -> CID`, `get_blob(CID) -> data`

### 15B: Metadata & Pinning
- [ ] `/cas/pins/{node_id}/{hash}` in root store
- [ ] Pin reconciler: watch pins, trigger fetch

### 15C: CLI
- [ ] `cas put`, `cas get`, `cas pin`, `cas ls`

---

## Milestone 16: Lattice Drive MVP

**Goal:** A functioning file sync tool that proves the architecture.

> **Depends on:** M14 (Wasm) + M15 (CAS)

### 16A: Filesystem Logic
- [ ] Write `filesystem.wasm` (in Rust, compiled to wasm32-wasi)
- [ ] Map file operations (`write`, `mkdir`, `rename`) to Log Ops
- [ ] Store file content in CAS, reference hashes in sigchain

### 16B: FUSE Interface
- [ ] Write `lattice-fs` using the `fuser` crate
- [ ] FUSE talks to Daemon via same gRPC interface as CLI
- [ ] Mount the Store as a folder on Linux/macOS
- **Demo:** `cp photo.jpg ~/lattice/` → Syncs to second node

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
- **Audit Trail Enhancements** (HLC `wall_time` already in Entry):
  - Human-readable timestamps in CLI (`store history` shows ISO 8601)
  - Time-based query filters (`store history --from 2026-01-01 --to 2026-01-31`)
  - Identity mapping layer (PublicKey → User name/email)
  - Tamper-evident audit export (signed Merkle bundles for external auditors)
  - Optional: External audit sink (stream to S3/SIEM)
