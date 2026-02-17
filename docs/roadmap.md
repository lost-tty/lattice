## Lattice Roadmap

**Status:** Milestone 10 (Fractal Stores) complete. Current focus: M11 (Weaver Protocol).

This document outlines the development plan for Lattice.
- **Completed M1-M10:** Core RSM platform, Handle-Less Architecture, Typed API, Fractal Store Model.
- **Completed M11A-B:** Intention data model, IntentionStore, floating intentions, witness records.
- **Current Focus (M11C):** Negentropy sync.
- **Upcoming:** Network Abstraction & Simulation (M12), CAS (M13).

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

**Goal:** Replace the legacy linear-log sync with a **Negentropy-based set reconciliation protocol** and an **Intention-based Data Model**.

> **See:** `weaver-hs/lattice-weaver.md` for the transition guide.

### 11A: The Intention Data Model ✅
- [x] Define `Intention` struct: `{ author, timestamp, store_id, store_prev, condition, ops }`
- [x] Define `Condition` enum: `V1(Vec<Hash>)` — causal dependencies
- [x] Borsh serialization, content-addressed `Intention::hash()`
- [x] `SignedIntention` envelope with Ed25519 signing + verification
- [x] `WitnessRecord` type with signed witness attestation
- [x] `HLC` (Hybrid Logical Clock) for causal timestamps

### 11B: Intention Storage ✅
- [x] `IntentionStore` (redb): `TABLE_INTENTIONS` (hash → borsh) + `TABLE_WITNESS` (seq → borsh)
- [x] In-memory `author_tips` tracking, rebuilt from DB on open
- [x] Idempotent insert, out-of-order chain arrival
- [x] Floating intention support (store-and-forward for unresolved causal deps)
- [x] Witness records written on state application
- [x] `HistoryEntry` proto with embedded `SignedIntention`, `HLC`, `Condition`
- [x] Composable `From` impls (kernel → proto) for `HLC`, `Condition`, `SignedIntention`
- [x] Remove legacy `Entry`/`SignedEntry`/`LogRecord` from storage proto
- [x] Delete sigchain module + entry.rs + log_traits.rs
- [x] **Witness Log Hash Chaining:** Add `prev_hash` to `WitnessContent`.
    - Enables verifying the integrity of the witness log sequence itself.
    - Requires caching `last_witness_hash` in `IntentionStore`.
- [ ] **Dependency Tracking & Execution Order:** 
    - **Problem:** Efficiently scheduling intentions when dependencies (parents or conditions) are missing or arrive out-of-order.
    - **Linear Progress:** `pending_children` (Map<ParentHash, Children>) for linear history (waiting for `store_prev`).
    - **Causal Progress:** `deferred_conditions` (Map<ConditionHash, Waiters>) for cross-author dependencies.
    - **Ordering Strategy:** Evaluate if a Min-Heap (HLC-ordered) is strictly necessary for fairness/determinism or if a simpler FIFO/Round-Robin approach suffices for concurrent authors.
    - **Goal:** Strict causal consistency without O(N) scanning.

### 11B½: Validation Hardening & Test Coverage
- [x] Reject ingested intentions with invalid signature (actor-level enforcement + test)
- [x] Reject ingested intentions with mismatched `store_id` (actor-level enforcement + test)
- [x] Verify duplicate ingest doesn't create duplicate witness record
- [x] Multi-author convergence test (3+ authors on same store)
- [x] HLC monotonicity across sequential submits at actor level

### 11C: Negentropy Sync
- [ ] Port `negentropy` implementation to Rust (no-std compatible)
- [ ] Implement `sync_v2` protocol:
  - Phase 1: Exchange author timestamps (fast-path check) + Acks
  - Phase 2: Negentropy reconciliation over `Intention` hashes
  - Phase 3: Bulk fetch of missing Intentions
- [ ] Remove `OrphanStore` (reconciliation handles gaps automatically)
- [ ] **Gate:** 50-node simulation (M12) runs flawlessly before moving to M13

---

## Milestone 12: Network Abstraction & Simulation

**Goal:** Decouple `lattice-net` from Iroh-specific types to enable deterministic in-memory simulation of multi-node networks. Prerequisite for validating Negentropy sync at scale.

### 12A: Transport Abstraction
- [ ] Extract `Transport` trait from `LatticeEndpoint` (connect/accept → MessageSink/MessageStream)
- [ ] Extract `GossipLayer` trait from `iroh_gossip::Gossip` (join/broadcast/subscribe)
- [ ] `IrohTransport` + `IrohGossip`: Production implementations wrapping Iroh QUIC
- [ ] `NetworkService` generic over `Transport` + `GossipLayer`

### 12B: In-Memory Simulation Harness
- [ ] `ChannelTransport` + `BroadcastGossip`: In-memory implementations using mpsc channels
- [ ] N-node simulator: configurable topology, round-based sync, convergence metrics
- [ ] **Gate:** 20+ node simulation shows reliable convergence

---

## Milestone 13: Log Lifecycle & Pruning

**Goal:** Enable long-running nodes to manage log growth through snapshots, pruning, and finality checkpoints.

> **See:** [DAG-Based Pruning Architecture](pruning.md) for details on Ancestry-based metrics and Log Rewriting.

### 13A: Snapshotting
- [ ] `state.snapshot()` when log grows large
- [ ] Store snapshots in `snapshot.db`
- [ ] Bootstrap new peers from snapshot instead of full log replay

### 13B: Waterlevel Pruning
- [ ] Calculate stability frontier (min seq acknowledged by all peers)
- [ ] `truncate_prefix(seq)` for old log entries
- [ ] Preserve entries newer than frontier

### 13C: Checkpointing / Finality
- [ ] Periodically finalize state hash (protect against "Deep History Attacks")
- [ ] Signed checkpoint entries in sigchain
- [ ] Nodes reject entries that contradict finalized checkpoints

### 13D: Hash Index Optimization
- [ ] Replace in-memory `HashSet<Hash>` with on-disk index (redb) or Bloom Filter
- [ ] Support 100M+ entries without excessive RAM

---

## Milestone 14: Content-Addressable Store (CAS)

**Goal:** Provide a high-performance, node-local "Dumb Pipe" for content storage. Replication and policy are handled separately by a "Smart Manager". Requires M11 (Weaver sync) and M12 (simulation harness for validation).

### 14A: Low-Level Storage (`lattice-cas`)
- [ ] `CasBackend` trait interface (`fs`, `block`, `s3`)
- [ ] Isolation: Mandatory `store_id` for all ops
- [ ] `redb` metadata index: ARC (Adaptive Replacement Cache), RefCounting
- [ ] `FsBackend`: Sharded local disk blob storage

### 14B: Replication & Safety (`CasManager`)
- [ ] `ReplicationPolicy` trait: `crush_map()` and `replication_factor()` from System Table
- [ ] `StateMachine::referenced_blobs()`: Pinning via State declarations
- [ ] Pull-based reconciler: `ensure(cid)`, `calculate_duties()`, `gc()` with Soft Handoff

### 14C: Wasm & FUSE Integration
- [ ] **Wasm**: Host Handles (avoid linear memory copy)
- [ ] **FUSE**: `get_range` (random access) and `put_batch` (buffered write)
- [ ] **Encryption**: Store-side encryption (client responsibility)

### 14D: CLI & Observability
- [ ] `cas put`, `cas get`, `cas pin`, `cas status`

---

## Milestone 15: Lattice File Sync MVP

**Goal:** A functioning file sync tool that requires M11 (Sync) and M14 (CAS).

### 15A: Filesystem Logic
- [ ] Define `DirEntry` schema: `{ name, mode, cid, modified_at }` in KV Store
- [ ] Map file operations (`write`, `mkdir`, `rename`) to KV Ops

### 15B: FUSE Interface
- [ ] Write `lattice-fs` using the `fuser` crate
- [ ] Mount the Store as a folder on Linux/macOS
- [ ] **Demo:** `cp photo.jpg ~/lattice/` → Syncs to second node

---

## Milestone 16: Wasm Runtime

**Goal:** Replace hardcoded state machine logic with dynamic Wasm modules.

### 16A: Wasm Integration
- [ ] Integrate `wasmtime` into the Kernel
- [ ] Define minimal Host ABI: `kv_get`, `kv_set`, `log_append`, `get_head`
- [ ] Replace hardcoded `KvStore::apply()` with `WasmRuntime::call_apply()`
- [ ] Map Host Functions to `StorageBackend` calls (M9 prerequisite)

### 16B: Data Structures & Verification
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
- [ ] **Signer Trait**: Introduce a `Signer` trait (sign hash → signature) to avoid passing raw `SigningKey` through the stack. Affects intention creation (`SignedIntention::sign`), witness signing (`WitnessRecord::sign`), and the M11 migration path.

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
- **USB Gadget Node**: A dedicated hardware device (e.g., Pi Zero, RISC-V dongle) that plugs into a host and enumerates as a USB Ethernet gadget. It runs a standalone Lattice Node and peers with the host over the virtual network interface. This provides a "smart pluggable storage" experience—physically pluggable storage that syncs via standard Lattice protocols, decoupled from the host's filesystem limitations.
- **Blind Ops / Cryptographic Revealing** (research): Encrypted intention payloads revealed only to nodes possessing specific keys (Convergent Encryption or ZK proofs). Enables selective disclosure within atomic transactions.
- **S-Expression Intention View**: Enhance `store history` and `store debug` to optionally display Intentions as S-expressions, providing a structured, verifiable view of the underlying data for debugging.
