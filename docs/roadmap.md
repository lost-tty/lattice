## Lattice Roadmap

**Status:** Milestone 10 (Fractal Stores) complete. Current focus: M11 (CAS).

This document outlines the development plan for Lattice.
- **Completed M1-M10:** Core RSM platform, Handle-Less Architecture, Typed API, Fractal Store Model.
- **Current Focus (M11):** Content-Addressable Store (CAS).
- **Upcoming:** Lattice Drive (M12), Weaver Protocol (M13).

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
| **M10**          | Fractal Store Model: replaced `Mesh` struct with recursive store hierarchy, `TABLE_SYSTEM` with HeadList CRDTs, `RecursiveWatcher` for child discovery, invite/revoke peer management, `PeerStrategy` (Independent/Inherited), flattened `StoreManager`, `NetworkService` (renamed from `MeshService`), proper `Node::shutdown()` |

---

## Milestone 11: The Weaver Protocol

**Goal:** Replace the legacy linear-log sync with a **Negentropy-based set reconciliation protocol** and a **Transaction-based Data Model**. This establishes the correct metadata structure before storing heavy blobs (CAS).

> **See:** `weaver-hs/lattice-weaver.md` for the transition guide.

### 11A: The Transaction Model (Intention & Entry)
- [ ] Define `Intention` struct: `{ author, timestamp, entries: [Entry], signature }`
- [ ] Define `Entry` struct: `{ store_id, store_prev, causal_deps, payload }`
- [ ] Implement validation logic:
  - `store_prev` hash chain continuity per (store, author)
  - `causal_deps` DAG validation
  - Atomic signature verification over the full Intention

### 11B: Intention Storage (Metadata)
- [ ] `intentions.db` (redb): Local storage for validated Intentions and Entries
- [ ] Index: `Hash -> Entry` (for random access/sync)
- [ ] Decouple storage from sequential log files
- [ ] Implement `WitnessLog` for local audit trail

### 11C: Negentropy Sync
- [ ] Port `negentropy` implementation to Rust (no-std compatible)
- [ ] Implement `sync_v2` protocol:
  - Phase 1: Exchange author timestamps (fast-path check) + Acks
  - Phase 2: Negentropy reconciliation over `Entry` hashes
  - Phase 3: Bulk fetch of missing Intentions
- [ ] Remove `OrphanStore` (reconciliation handles gaps automatically)

### 11D: Trans-Store Atomicity
- [ ] APIs for creating multi-entry Intentions (e.g., "move file from Store A to B")
- [ ] Ensure validation enforces atomicity (all-or-nothing accept)

---

## Milestone 12: Content-Addressable Store (CAS)

**Goal:** Provide a high-performance, node-local "Dumb Pipe" for content storage. Replication and policy are handled separately by a "Smart Manager".

### 12A: Low-Level Storage (`lattice-cas`)
- [ ] `CasBackend` trait interface (`fs`, `block`, `s3`)
- [ ] Isolation: Mandatory `store_id` for all ops
- [ ] `redb` metadata index: ARC (Adaptive Replacement Cache), RefCounting
- [ ] `FsBackend`: Sharded local disk blob storage

### 12B: Replication & Safety (`CasManager`)
- [ ] `ReplicationPolicy` trait: `crush_map()` and `replication_factor()` from System Table
- [ ] `StateMachine::referenced_blobs()`: Pinning via State declarations
- [ ] Pull-based reconciler: `ensure(cid)`, `calculate_duties()`, `gc()` with Soft Handoff

### 12C: Wasm & FUSE Integration
- [ ] **Wasm**: Host Handles (avoid linear memory copy)
- [ ] **FUSE**: `get_range` (random access) and `put_batch` (buffered write)
- [ ] **Encryption**: Store-side encryption (client responsibility)

### 12D: CLI & Observability
- [ ] `cas put`, `cas get`, `cas pin`, `cas status`

---

## Milestone 13: Lattice Drive MVP (File Sync)

**Goal:** A functioning file sync tool that requires M11 (Sync) and M12 (CAS).

### 13A: Filesystem Logic
- [ ] Define `DirEntry` schema: `{ name, mode, cid, modified_at }` in KV Store
- [ ] Map file operations (`write`, `mkdir`, `rename`) to KV Ops

### 13B: FUSE Interface
- [ ] Write `lattice-fs` using the `fuser` crate
- [ ] Mount the Store as a folder on Linux/macOS
- [ ] **Demo:** `cp photo.jpg ~/lattice/` → Syncs to second node

---

## Milestone 14: Log Lifecycle & Pruning

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

- [x] **Legacy Peer Data Migration** (`migrate_legacy_peer_data` in `mesh.rs`)
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
- **Inter-Store Messaging** (research): Stores exchange typed messages using an actor model. Parent stores have a store-level identity (keypair derived from UUID + node key). Children trust the parent's store key — not individual parent authors — preserving encapsulation. Parent state machines decide what to relay; child state machines process messages as ops in their own DAG. Key design questions: message format, delivery guarantees (at-least-once via DAG?), bidirectional messaging (child→parent replies), and whether messages are durable (entries) or ephemeral.
- **Audit Trail Enhancements** (HLC `wall_time` already in Entry):
  - Human-readable timestamps in CLI (`store history` shows ISO 8601)
  - Time-based query filters (`store history --from 2026-01-01 --to 2026-01-31`)
  - Identity mapping layer (PublicKey → User name/email)
  - Tamper-evident audit export (signed Merkle bundles for external auditors)
  - Optional: External audit sink (stream to S3/SIEM)
- **USB Gadget Node**: A dedicated hardware device (e.g., Pi Zero, RISC-V dongle) that plugs into a host and enumerates as a USB Ethernet gadget. It runs a standalone Lattice Node and peers with the host over the virtual network interface. This provides a "smart external drive" experience—physically pluggable storage that syncs via standard Lattice protocols, decoupled from the host's filesystem limitations.
