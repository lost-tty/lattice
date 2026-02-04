## Lattice Roadmap

**Status:** Milestone 10 (Fractal Stores) in progress.

This document outlines the development plan for Lattice.
- **Completed M1-M9:** Core RSM platform, Handle-Less Architecture, Typed API.
- **Current Focus (M10):** Unifying Mesh/Store concepts into a recursive graph (Fractal Store Model).
- **Upcoming:** CAS (M11), Weaver Protocol (M12).

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
| **M9**           | Storage abstraction: `StateBackend`/`StateLogic` traits, unified dispatch API, `PersistentState<T>` wrapper, store identity/type registry, generic open |

---

## Milestone 10: Fractal Store Implementation

**Goal:** Implement the **Fractal Store Model** (ADR 001), replacing the legacy `Mesh` struct with a recursive store hierarchy.

> **Why:** The distinction between `Mesh` and `Store` is artificial. By treating everything as a store, we simplify the codebase, enable infinite nesting, and unify peer management.

### 10A: System Store (TABLE_SYSTEM)
- [x] Created `lattice-systemstore` crate with `TABLE_SYSTEM` using HeadList CRDTs
- [x] Moved `PeerStrategy` to `lattice-systemstore` (clean dependency)
- [x] Implemented `SystemStore` trait with `get_peer`/`update_peer`/`get_peers` API
- [x] System table interception layer in `PersistentState::apply()`
- [x] Removed dead code: `HandleBase`, `PeerManager`/`SubstoreManager` traits, `as_writer`
- [x] Added `list_all()` API and `store system show` CLI command for debugging
- [x] Add child store hierarchy (`child/{uuid}/status`, `child/{uuid}/name`)
- [x] Migrate CLI: `mesh peers` -> `store peers` (peers are part of the store system data)
- [ ] Add `strategy` key for persisted `PeerStrategy` (Independent/Inherited)
- [ ] Add invite handling (`invite/{token_hash}/status`, `invited_by`, `claimed_by`)

### 10B: Root Store & Identity (The "New Mesh")
- [ ] Define "Root Store" concept: `PeerStrategy::Independent` + `StoreMeta`
- [ ] **Migrate Peer Names**: Move names from Data Table (`/nodes/*/name`) to System Table (`peer/*/name`)
- [ ] `Node::set_name()` should propagate name to all locally managed Root Stores (System Table)
- [ ] Update `lattice-node` bootstrapping:
    - Deprecate `Node::new(mesh_id)`
    - Add `Node::load(root_store_id)`
    - `StoreManager` should load all locally available Root Stores at startup

### 10C: Migration (Peer Strategy)
- [ ] **Strategy Backfill**: Since stores are already migrated to System Table, we just need to populate `strategy`:
    - Root Stores -> `Independent` (default)
    - Child Stores -> `Inherited` (future default)
- [ ] Test migration with existing data directories

### 10D: Legacy Cleanup
- [ ] Convert existing `Mesh` structs to Root Stores
- [ ] Remove `Mesh` and `ControlPlane` code
- [ ] CLI: Remove `mesh` subcommand entirely (replace with `store create`, `store join`, `store peers`)
- [ ] Refactor `StoreManager`: Remove "mesh" concept, just manage a flat list of loaded stores (some are roots)

---

## Milestone 11: Content-Addressable Store (CAS)

**Goal:** Blob storage for large files (prerequisite for Lattice Drive). We use **CIDs (Content Identifiers)** for self-describing formats and IPFS/IPLD compatibility.

### 11A: CAS Integration
- [ ] `lattice-cas` crate with pluggable backend (local FS, Garage S3)
- [ ] `put_blob(data) -> CID`, `get_blob(CID) -> data`

### 11B: Metadata & Pinning
- [ ] `/cas/pins/{node_id}/{hash}` in root store
- [ ] Pin reconciler: watch pins, trigger fetch

### 11C: CLI
- [ ] `cas put`, `cas get`, `cas pin`, `cas ls`

---

## Milestone 12: Lattice Drive MVP (File Sync)

**Goal:** A functioning file sync tool that proves the architecture. Requires Fractal Stores (M10) and CAS (M11).

> **Note:** We are prioritizing this over Weaver Protocol to get a user-facing product sooner.

### 12A: Filesystem Logic
- [ ] Define `DirEntry` schema: `{ name, mode, cid, modified_at }` in KV Store
- [ ] Map file operations (`write`, `mkdir`, `rename`) to KV Ops

### 12B: FUSE Interface
- [ ] Write `lattice-fs` using the `fuser` crate
- [ ] Mount the Store as a folder on Linux/macOS
- [ ] **Demo:** `cp photo.jpg ~/lattice/` → Syncs to second node

---

## Milestone 13: The Weaver Protocol (Data Model Refactor)

**Goal:** Implement the "Ly" data model by splitting the monolithic `Entry` into `Intention` (Transport-Agnostic Signal) and `Witness` (Chain-Specific Ordering). This decoupling enables "Ghost Parent" resolution and selective data sharing.

> **See:** [Ly Architecture](ly_architecture.md) for full protocol details.

### 13A: The Intention
- [ ] Define `Intention` struct: `{ payload: Vec<u8>, author: PubKey, seq: u64, prev_hash: Hash, sig: Signature }`
- [ ] Implement `Intention` validation (author signature check + seq/hash continuity)
- [ ] Ensure `Intention` is valid independent of any chain (floating message)

### 13B: The Witness
- [ ] Define `Witness` event in `Entry`: `Entry { parent: Hash, event: Witness(Intention), sig: MySig }`
- [ ] Update `SigChain` to append `Witness` events
- [ ] Implement "Ingest/Digest" envelope pattern for Gossip

---

## Milestone 14: Negentropy Sync Protocol

**Goal:** Replace O(n) vector clock sync with sub-linear bandwidth using range-based set reconciliation.

Range-based set reconciliation using hash fingerprints. Used by Nostr ecosystem.

### 14A: Infrastructure
- [ ] Add hash→entry index (for efficient fetch-by-hash)
- [ ] Implement negentropy fingerprint generation per store

### 14B: Protocol Migration
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

## Milestone 15: Wasm Runtime

**Goal:** Replace hardcoded state machine logic with dynamic Wasm modules.

### 15A: Wasm Integration
- [ ] Integrate `wasmtime` into the Kernel
- [ ] Define minimal Host ABI: `kv_get`, `kv_set`, `log_append`, `get_head`
- [ ] Replace hardcoded `KvStore::apply()` with `WasmRuntime::call_apply()`
- [ ] Map Host Functions to `StorageBackend` calls (M9 prerequisite)

### 15B: Data Structures & Verification
- [ ] Finalize `Entry`, `SignedEntry`, `Hash`, `PubKey` structs for Wasm boundary
- [ ] Wasm-side SigChain verification (optional, for paranoid clients)
- **Deliverable:** A "Counter" Wasm module that increments a value when it receives an Op

---

## Milestone 16: Log Lifecycle & Pruning

**Goal:** Enable long-running nodes to manage log growth through snapshots, pruning, and finality checkpoints.

> **See:** [DAG-Based Pruning Architecture](pruning.md) for details on Ancestry-based metrics and Log Rewriting.

### 16A: Snapshotting
- [ ] `state.snapshot()` when log grows large
- [ ] Store snapshots in `snapshot.db`
- [ ] Bootstrap new peers from snapshot instead of full log replay

### 16B: Waterlevel Pruning
- [ ] Calculate stability frontier (min seq acknowledged by all peers)
- [ ] `truncate_prefix(seq)` for old log entries
- [ ] Preserve entries newer than frontier

### 16C: Checkpointing / Finality
- [ ] Periodically finalize state hash (protect against "Deep History Attacks")
- [ ] Signed checkpoint entries in sigchain
- [ ] Nodes reject entries that contradict finalized checkpoints

### 16D: Hash Index Optimization
- [ ] Replace in-memory `HashSet<Hash>` with on-disk index (redb) or Bloom Filter
- [ ] Support 100M+ entries without excessive RAM

---

## Milestone 17: Embedded Proof ("Lattice Nano")

**Goal:** Running the Kernel on the RP2350.

> Because CLI is already separated from Daemon (M7) and storage is abstracted (M9), only the Daemon needs porting.
> **Note:** Requires substantial refactoring of `lattice-kernel` to support `no_std`.

### 17A: `no_std` Refactoring
- [ ] Split `lattice-kernel` into `core` (logic) and `std` (IO)
- [ ] Replace `wasmtime` (JIT) with `wasmi` (Interpreter) for embedded target
- [ ] Port storage layer to `sequential-storage` (Flash) via `StorageBackend`

### 17B: The "Continuity" Demo
- [ ] Build physical USB stick prototype
- [ ] Implement BLE/Serial transport
- **The Reveal:** Sync a file from Laptop → Stick → Phone without Internet

---

## Technical Debt

- [ ] **REGRESSION**: history command list filtering (backend side) capability
- [ ] **REGRESSION**: Graceful reconnect after sleep/wake (may fix gossip regression)
- [ ] **Store Name Lookup Optimization**: `find_store_name()` in `store_service.rs` and `backend_inprocess.rs` does O(meshes × stores) linear search. Store names live in mesh root KV stores (StoreDeclaration). Consider caching in StoreManager or adding index.
- [ ] **Data Directory Lock File**: Investigate lock file mechanism to prevent multiple processes from using the same data directory simultaneously (daemon + embedded app conflict). Options: flock, PID file, or socket-based detection.
- [ ] **Denial of Service (DoS) via Gossip**: Implement rate limiting in GossipManager and drop messages from peers who send invalid data repeatedly.
- [ ] **Payload Validation Strategy**: Decide where semantic validation occurs and what happens on failure. Options: build-time only, versioned rules, entry replacement, or separate chain/payload advancement. See `test_rejected_entry_breaks_chain` in `lattice-kvstore/src/kv.rs`.

---

## Active Migrations & Backfills

- [x] **System Table Type Backfill** (`backfill_child_types` in `mesh.rs`)
  - **Purpose**: Populates `store_type` in System Table for legacy stores that have `type="unknown"`.
  - **Mechanism**: On startup, checks local disk headers for unknown stores and issues `ChildAdd` op.
  - **Completion Condition**: Can be removed once all nodes have cycled and updated the System Table.

- [ ] **Legacy Root Store Data Migration** (`migrate_legacy_data` in `mesh.rs`)
  - **Purpose**: Moves root store declarations from `/stores/` keys to System Table.
  - **Mechanism**: One-time migration on startup.
  - **Destructive**: Deletes legacy keys after successful migration.

- [ ] **Legacy Peer Data Migration** (`migrate_legacy_peer_data` in `mesh.rs`)
  - **Purpose**: Moves peer status/metadata from raw keys to System Table.
  - **Mechanism**: One-time migration on startup.

- [ ] **StateDB Table Renaming** (`state_db.rs`)
  - **Purpose**: Renames legacy "kv" or "log" redb tables to standard "data" table.
  - **Mechanism**: Checked on every `StateBackend` open.

- [ ] **Legacy Store Type Aliases** (`lattice-model`, `lattice-runtime`)
  - **Purpose**: Supports opening stores with legacy type strings ("kvstore", "logstore").
  - **Mechanism**: `StoreRegistry` maps these to modern equivalents.

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
