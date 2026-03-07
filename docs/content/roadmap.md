---
title: "Roadmap"
---

## Completed

| Milestone        | Summary                                                                                                                                       |
|------------------|-----------------------------------------------------------------------------------------------------------------------------------------------|
| **M1–M3**        | Single-node log with Intention DAG/HLC, DAG conflict resolution (LWW), async actor pattern, two-node sync via Iroh, Mesh API with token-based join |
| **M4**           | Generic RSM platform: extracted `lattice-model`, `lattice-kvstore`, WAL-first persistence, gRPC introspection                                 |
| **M5**           | Atomic multi-key transactions: `batch().put(k1,v1).put(k2,v2).commit()`                                                                       |
| **M6**           | Multi-store: root store as control plane, StoreManager with live reconciliation, per-store gossip                                             |
| **Decoupling**   | Node/Net separation via `NodeProvider` traits, `NetEvent` channel owned by net layer, unified StoreManager                                    |
| **Kernel Audit** | Orphan resolution refactor, `MAX_CAUSAL_DEPS` limit, zero `.unwrap()` policy, clean module hierarchy                                          |
| **M7**           | Client/Daemon split: library extraction, `latticed` daemon with UDS gRPC, `LatticeBackend` trait, RPC event streaming, socket security        |
| **M8**           | Reflection & introspection: `lattice-api` crate, deep type schemas, `ReflectValue` structured results, store watchers & FFI stream bindings   |
| **M10**          | Fractal Store Model: replaced `Mesh` struct with recursive store hierarchy, `TABLE_SYSTEM` with HeadList CRDTs, `RecursiveWatcher` for child discovery, invite/revoke peer management, `PeerStrategy` (Independent/Inherited), flattened `StoreManager`, `NetworkService` (renamed from `MeshService`), proper `Node::shutdown()` |
| **M11**          | Weaver Migration & Protocol Sync: Intention DAG (`IntentionStore`), Negentropy set reconciliation, Smart Chain Fetch, stream-based Bootstrap (Clone) Protocol without gossip storms |
| **M12**          | Network Abstraction & Simulation: `Transport`/`GossipLayer` traits, `IrohTransport` extracted to `lattice-net-iroh`, `ChannelTransport`/`BroadcastGossip` in `lattice-net-sim`, gossip lag tracking, event-driven gap handling, `SessionTracker` decoupling, symmetric Negentropy sync |
| **M13**          | Crate Dependency Architecture: removed `lattice-kernel` from `lattice-net`, `lattice-net-types`, `lattice-systemstore`; moved proto types to `lattice-proto`; elevated shared types to `lattice-store-base`; removed phantom deps from `lattice-bindings` and `lattice-api`; moved `WatchEvent`/`WatchEventKind` from `kvstore-api` to `kvstore` (flipped dependency); `kvstore-api` demoted to dev-dep in `lattice-node`; `RuntimeBuilder::with_opener()` plugin mechanism with `core-stores` feature flag; store type constants removed from `lattice-node` re-exports |
| **M14**          | Slim Down Persistent State: `DagQueries` trait with `find_lca`/`get_path`/`is_ancestor` (causal-only BFS), `KVTable` unified state engine (shared by KvState + SystemTable), write-time LWW resolution, slim on-disk format (materialized value + intention hash pointers), `ScopedDag` wrapper, conflict detection on read (`get`/`list` return `conflicted` flag), `Inspect` command for full conflict state, branch inspection (`store debug branch`) via LCA + path traversal |
| **M15**          | Review & cleanup: (A) typed error enums replacing ~50 `map_err` + all `String` error variants, `IntoStatus` gRPC mapping, fixed discarded `witness()` result; (B) eliminated all production sleeps (event-driven auto-sync, `Notify`-based store lookup), standardized 26 test sleeps to `tokio::time::timeout`; (C) `store_type` threaded through `JoinResponse` proto → join flow, removed `STORE_TYPE_KVSTORE` hardcodes, dropped `lattice-kvstore` dev-dep from `lattice-net-iroh`; (D) removed `sync_all` thundering-herd fallback from `handle_missing_dep` — 2-tier recovery (fetch_chain → sync_with_peer) with sequential alternative-peer fallback via acceptable authors; `ChannelNetwork::disconnect()` for partition testing |

---

## Milestone 16: Uniform Store Traits + Witness-First Architecture

Unify how all stores (KvStore, LogStore, SystemStore) interact with storage, then invert apply/witness order so the witness log becomes a true WAL.

> **Concrete type stack today:** `Store< SystemLayer< PersistentState< KvState > > >`
> **After M16:** `Store< SystemLayer< KvState > >` — `PersistentState` removed, `SystemLayer` is a thin Y-gate that owns the transaction and provides `store_meta()`.

### Motivation

1. **Duplicated transaction ceremony.** `SystemLayer::apply_system_transaction()` is a hand-written copy of `StateLogic::apply()`. Both do `begin_write → verify_and_update_tip → open table → mutate → commit` on the same `state.db`. The only difference is which table (`TABLE_SYSTEM` vs `TABLE_DATA`). This means system ops and app-data ops can never be atomic, the chain-tip verification logic is duplicated, and the code is harder to reason about.

2. **System store logic embedded in `SystemLayer`.** `SystemLayer` is 669 lines mixing two concerns: Y-gate routing (decode `UniversalOp`, route system vs app-data) and system store implementation (all the typed peer/hierarchy/invite/strategy match arms in `apply_system_op`). The system store should be a standalone state machine like `KvState` and `LogState`.

3. **Three different patterns for three stores.** `KvState` and `LogState` implement `StateLogic` (mutate/notify/backend), while the system store logic lives inside `SystemLayer` with its own transaction management. All three should implement the same trait.

4. **State machines coupled to redb.** `StateLogic::mutate()` takes `&mut redb::Table` directly. Can't test state machines without a full redb database. With `TableStorage` traits, state machines become pure functions testable with `HashMapStorage`.

5. **Discarded witness result.** `let _ = store.witness(...)` means apply can succeed while witnessing fails. With witness-first, the witness log is the commit point.

6. **Complex restart path.** `replay_intentions()` compares two independent tip sets and walks chains to find gaps. With witness-first, replay is trivial: read witness log from `last_applied_witness` forward.

### 16A: Uniform Store Pattern

Extract system store logic out of `SystemLayer`. Make `KvState`, `LogState`, and a new `SystemState` all implement the same `mutate`-style interface. `SystemLayer` becomes a thin Y-gate that owns the transaction and routes ops to the right store+table. Pure refactor — no new abstractions, still on `redb::Table`.

**Step 1 — Extract `SystemState`** (`lattice-systemstore`): ✅

Moved `apply_system_op()` match arms from `SystemLayer` into `SystemState::mutate()`. Restructured `lattice-systemstore` into `layer/` (orchestration) and `store/` (data logic). Moved `SystemReader` reads into `SystemState<'a> { db: &Database }`. All 250 tests pass.

**Step 2 — Unify transaction ownership in `SystemLayer`**: ✅

`SystemLayer` now owns the single write transaction for both paths via `apply_unified()`. Decodes `UniversalOp` envelope, then: `begin_write → verify_and_update_tip → route to SystemState::mutate(TABLE_SYSTEM) or S::mutate(TABLE_DATA) → commit → notify`. Removed `apply_system_transaction()`. `StateLogic::apply()` default is no longer called in production. All 250 tests pass.

**Step 3 — Remove `PersistentState<T>` and slim `StateMachine`**: ✅

Removed `PersistentState<T>` wrapper entirely — domain crates now implement `StateMachine` directly (just `apply()`). `snapshot`/`restore`/`applied_chaintips` removed from `StateMachine` trait. `applied_chaintips` moved to `StoreIdentity` trait (implemented by `SystemLayer`). `store_meta()` also on `StoreIdentity`. `MockWriter<S>` updated to `Arc<S>`. Type aliases (`PersistentKvState` etc.) removed. All assembly sites updated to `SystemLayer<KvState>` etc. `PersistentState<T>`, `setup_persistent_state()`, `ForwardSubscriber` deleted from `lattice-storage`. All ~250 tests pass.

`StateMachine` slimmed to 1 method:
```
trait StateMachine {
    type Error;
    fn apply(&self, op: &Op, dag: &dyn DagQueries) -> Result<(), Self::Error>;
}
```

`StoreIdentity` trait (in `lattice-model`):
```
trait StoreIdentity: Send + Sync {
    fn store_meta(&self) -> StoreMeta;
    fn applied_chaintips(&self) -> Result<Vec<(PubKey, Hash)>, String>;
}
```

### 16B: Backend Ownership & Trait Consolidation

Move backend ownership from domain crates into `SystemLayer`. Domain crates receive a `ScopedDb` (read-only handle scoped to their table) instead of a full `StateBackend`. Consolidate `StateLogic` and `StateMachine` into a clean two-level split: domain crates implement `StateLogic` (table-level apply), `SystemLayer` implements `StateMachine` (transaction-level apply for the kernel).

> **Before:** Domain crates hold `backend: StateBackend` + `db: Arc<Database>` (full DB access). Two redundant apply traits (`StateMachine::apply(op, dag)` and `StateLogic::apply(op, dag)` default impl).
> **After:** Domain crates hold `ScopedDb` (read-only, one table). `SystemLayer` owns `StateBackend`. One trait per level: `StateLogic::apply(table, op, dag)` for domain crates, `StateMachine::apply(op, dag)` for the kernel.

**Step 1 — `ScopedDb` wrapper** (`lattice-storage`): ✅

`ScopedDb` wraps `Arc<Database>` + `TableDefinition` and exposes `begin_read() -> ScopedReadTxn`. `ScopedReadTxn::open_table()` can only open the scoped table. Domain crates use this for read-path queries (`get`, `scan`, `list`) without access to `TABLE_META` or `TABLE_SYSTEM`.

**Step 2 — Consolidate traits, move backend into `SystemLayer`**:

Rename `StateLogic::mutate()` to `StateLogic::apply()`. Remove `StateLogic::backend()` and the default `StateLogic::apply(op, dag)` impl (dead code — `SystemLayer` bypasses it). Remove `StateMachine` impls from domain crates (`KvState`, `LogState`, `NullState`). Change `StateFactory::create(StateBackend)` to `StateFactory::create(ScopedDb)`. Move `StateBackend` into `SystemLayer` struct. Update `SystemLayer::apply_unified()` to use `self.backend` instead of `self.inner.backend()`.

Trait split after this step:

```
// Domain crates implement this (lattice-storage)
trait StateLogic: Send + Sync {
    type Updates;
    fn apply(&self, table: &mut Table, op: &Op, dag: &dyn DagQueries) -> Result<Self::Updates, StateDbError>;
    fn notify(&self, updates: Self::Updates);
}

// SystemLayer implements this (lattice-model)
trait StateMachine: Send + Sync {
    type Error;
    fn apply(&self, op: &Op, dag: &dyn DagQueries) -> Result<(), Self::Error>;
}
```

Changes:
- [x] Rename `StateLogic::mutate()` to `StateLogic::apply()`, remove `backend()` and default `apply()`
- [x] Remove `StateMachine` impls from `KvState`, `LogState`, `NullState`
- [x] Fold `StateFactory` into `StateLogic::create(ScopedDb)`, delete `StateFactory` trait
- [x] Update `KvState`, `LogState` to hold `ScopedDb` instead of `backend` + `db`
- [x] Update `NullState` to unit struct (no reads, no state)
- [x] Move `StateBackend` into `SystemLayer` struct, update `apply_unified()` and `StoreIdentity` impl
- [x] Update `SystemLayer::open()` to keep backend, pass `ScopedDb` to `S::create()`
- [x] Update `MockWriter<S>` — wraps `Arc<SystemLayer<S>>`, uses `StoreIdentity` for chain tips, wraps payload in `UniversalOp::AppData`
- [x] Update all test helpers (`new_test_store`, `open_test_store`) and domain crate tests
- [x] Run full test suite
- [ ] Make `SystemState` implement `StateLogic` (same trait as `KvState`/`LogState`). Currently `SystemState::mutate()` is a static method taking `&mut WriteTransaction` — it should be an owned struct with `ScopedDb`, `create(ScopedDb)`, and `apply(&self, &mut Table, op, dag)`.
- [ ] `SystemState` should emit `SystemEvent` via `broadcast::Sender` in `notify()`, same as `KvState` emits `WatchEvent`. Delete `subscribe_system_events()` which currently decodes events by re-parsing the witness log stream — the apply path already has the decoded op.
- [ ] Scope domain crate tests closer: internal `#[cfg(test)]` tests should call `StateLogic::apply(table, op, dag)` directly (no `SystemLayer`); integration tests in `tests/` go through `SystemLayer`. Currently some tests still open raw write transactions via `StateBackend` — consider a small test-only helper in `lattice-storage` to reduce ceremony.
- [ ] Move `STORE_TYPE_KVSTORE` and `STORE_TYPE_LOGSTORE` constants out of `lattice-model` into their respective store crates (`lattice-kvstore`, `lattice-logstore`). `lattice-model` shouldn't know about specific store types.

### 16C: Witness-First Core

Convert to witness-first. `TABLE_META` ownership is resolved: `SystemLayer` owns `StateBackend` which owns `TABLE_META`. Domain crates can't see it.

**`TABLE_META` ownership (resolved in 16B):**

`TABLE_META` holds store identity (store_id, store_type, schema_version — written once at creation) and per-author chain tips (tip/<pubkey> → hash — written on every apply). `SystemLayer` owns `StateBackend`, which reads/writes `TABLE_META` via `get_meta()` and `verify_and_update_tip()`. Domain crates only see `ScopedDb` scoped to `TABLE_DATA`.

Remaining decisions for witness-first:
- [ ] **`last_applied_witness: Hash`**: framework-owned. Written by `project_new_entries()` after each successful state projection. Read on restart to resume.
- [ ] **`stalled_at: Option<(Hash, String)>`**: framework-owned. Records where projection stopped (version skew).
- [ ] Decide if chain tips and witness cursor should remain in `TABLE_META` or split into a separate table.

**Step 1 — Witness-first conversion:**

The witness log becomes the sole interface between the DAG layer and the state machine. The two halves are fully decoupled:

```
Intention DAG ──► witness_ready() ──► Witness Log ──► project_new_entries() ──► State Machine
(floating pool)   (witness-only,       (append-only,   (reads log forward,      (pure apply)
                   no state touch)      sequential)     calls state.apply())
```

`project_new_entries()` is the single path into the state machine — called after witnessing, on restart (replay), and after bootstrap (no separate `apply_witnessed_batch`).

**Redefined semantics:**
- **Witness** = "I verified this intention's structural integrity (valid signature, correct chain, matching store_id) and committed it to my local ordering." Does NOT imply payload comprehension.
- **State projection** = derived view of the witness log. May lag behind the log if the state machine can't decode a payload (version skew). Catches up on upgrade.

- [ ] **`witness_ready()`**: Loop through floating intentions whose deps are witnessed. Witness each one. Do not touch state. Returns count of newly witnessed entries.
- [ ] **`project_new_entries()`**: Read witness log from `last_applied_witness` forward. For each entry, build `Op`, call `state.apply()` via the unified transaction path. Update `last_applied_witness` after each successful projection. If projection fails (version skew), record `stalled_at` and stop — the witness log keeps advancing independently.
- [ ] **`witness()` becomes the commit point.** Must succeed (not `let _ =`); if it fails, the intention stays floating.
- [ ] **Drop `StateMachine::applied_chaintips()`**. Per-author chain tips remain framework-internal (needed for `verify_and_update_tip`), but not exposed to state machines.
- [ ] **`replay_intentions()` becomes `project_new_entries()`** — same code path for restart, post-witness, and post-bootstrap. No tip-comparison, no chain-walking.

### 16D: Stall Reporting
- [ ] **Framework exposes projection status** via `StoreMeta`: `{ last_applied_witness: Hash, witness_head: Hash, stalled_at: Option<(Hash, String)> }`. Surfaced via `store status` CLI.
- [ ] **Non-blocking stall.** A stalled projection does NOT block: witnessing continues, sync continues, other stores are unaffected. Only reads against the stalled store's state return stale data (with a warning flag).

### 16E: Bootstrap Alignment
- [ ] **Delete `apply_witnessed_batch`.** Bootstrap now uses the same two-step path: insert + witness the peer's entries into the log, then `project_new_entries()`. No special code path.

### 16F: Payload Validation Strategy Update
- [ ] **Update "stall on failure" contract** in `StateMachine` trait docs. Stall still applies — but the stall point moves from "intention stays floating, never witnessed" to "intention is witnessed, state projection pauses." The effect on the local user is the same (stale state until upgrade). The effect on the network is better (sync and other authors are unblocked).

### Future: System Store Separation
Deferred to a later milestone: make the system table a sibling store with its own DAG instead of a `SystemLayer` wrapper. This eliminates the `UniversalOp` envelope and dual-table pattern entirely, but changes the replication model (two DAGs per logical store) and touches join protocol, sync, gossip, and `RecursiveWatcher`.

---

## Milestone 17: Log Lifecycle & Pruning

Manage log growth on long-running nodes via stability frontier, snapshots, pruning, and finality checkpoints.

> **See:** [Stability Frontier](stability-frontier.md) for the full design.

### 17A: Stability Frontier & Tip Attestations
- [ ] `SystemOp::TipAttestation { tips: Vec<(PubKey, Hash)> }` — publish changed author tips as SystemOps
- [ ] Attestation keys in `TABLE_SYSTEM`: `attestation/{attester}/{author} → Hash` (LWW by HLC)
- [ ] Derive per-author frontier: `frontier[A] = min(tip[A] across all Active peers)`
- [ ] Attestation triggers: post-sync, batch threshold, periodic heartbeat (~15 min), graceful shutdown
- [ ] **Lag metric:** Per-peer divergence from local tips, surfaced via `NodeEvent::PeerLagWarning` and `store status`

### 17B: Snapshotting
- [ ] **Frontier snapshot via replay:** The frontier is behind the current state. Generating a snapshot at the frontier requires replaying intentions from the last snapshot up to the frontier cut, producing the correct state at that point. Options: tempfile-based replay (works today, heavy on I/O) or in-memory `StateBackend` variant (cleaner). **Note:** `StateMachine::snapshot()`/`restore()` were removed in M16A Step 3 — the old `StateBackend::snapshot_internal()` serialization format is deleted. M17 will design a new snapshot mechanism tailored to pruning requirements (frontier-aware, incremental, possibly stored in a separate `snapshot.db`).
- [ ] Store snapshots in `snapshot.db` (per-store, includes `TABLE_SYSTEM` attestation state)
- [ ] Bootstrap new peers from snapshot + tail sync instead of full log replay
- [ ] **Future optimization:** Inverse operations stored in `WitnessRecord` could allow rolling back from current state to frontier in O(head − frontier), avoiding replay. Deferred until profiling shows replay is a bottleneck.

### 17C: Pruning
- [ ] `truncate_prefix` for all intentions per author up to `frontier[A]`
- [ ] Discard individual attestation intentions below the frontier (snapshot carries state forward)
- [ ] Preserve intentions newer than frontier

### 17D: Checkpointing / Finality
- [ ] Periodically finalize state hash (protect against "Deep History Attacks")
- [ ] Signed checkpoint intentions in sigchain
- [ ] Nodes reject intentions that contradict finalized checkpoints

### 17E: Recursive Store Bootstrapping (Pruning-Aware)
- [ ] `RecursiveWatcher` identifies child stores from live state, not just intention logs.
- [ ] Bootstrapping a child store requires a **Two-Phase Protocol** because Negentropy cannot sync from an implicit zero-genesis if the store has been pruned.
- [ ] **Phase 1 (Snapshot):** Request the opaque snapshot (`state.db`) from the peer for the discovered child store.
- [ ] **Phase 2 (Tail Sync):** Run Negentropy to sync floating intentions that occurred *after* the snapshot's causal frontier.

### 17F: Hash Index Optimization ✅
- [x] Replace in-memory `HashSet<Hash>` with on-disk index (`TABLE_WITNESS_INDEX` in redb)
- [x] Support 100M+ intentions without excessive RAM

### 17G: Advanced Sync Optimization (Future)
- [ ] **Modular-Add Fingerprints:** Replace XOR-based fingerprints with modular addition (mod 2^256). XOR is linear and cancels duplicates (`a ⊕ a = 0`); mod-add is strictly more robust at identical cost. Affected sites:
  - `IntentionStore::xor_fingerprint()` / `derive_table_fingerprint()` / `fingerprint_range()` in `lattice-kernel`
  - `SyncProvider` trait docs in `lattice-kernel/src/sync_provider.rs`
  - Reconciler mock in `lattice-sync` tests
- [ ] **Persistent Merkle Index / Range Accumulator:**
  - Avoid O(N) scans for range fingerprints (currently linear)
  - Pre-compute internal node hashes in a B-Tree or Merkle Tree structure
- [ ] **High-Radix Splitting:**
  - Increase branching factor (e.g., 16-32 children) to reduce sync rounds (log32 vs log2)
  - Parallelize range queries

---

## Milestone 18: Content-Addressable Store (CAS)

Node-local content-addressable blob storage. Replication policy managed separately. Requires M11 and M12.

### 18A: Low-Level Storage (`lattice-cas`)
- [ ] `CasBackend` trait interface (`fs`, `block`, `s3`)
- [ ] Isolation: Mandatory `store_id` for all ops
- [ ] `redb` metadata index: ARC (Adaptive Replacement Cache), RefCounting
- [ ] `FsBackend`: Sharded local disk blob storage

### 18B: Replication & Safety (`CasManager`)
- [ ] `ReplicationPolicy` trait: `crush_map()` and `replication_factor()` from System Table
- [ ] `StateMachine::referenced_blobs()`: Pinning via State declarations
- [ ] Pull-based reconciler: `ensure(cid)`, `calculate_duties()`, `gc()` with Soft Handoff

### 18C: Wasm & FUSE Integration
- [ ] **Wasm**: Host Handles (avoid linear memory copy)
- [ ] **FUSE**: `get_range` (random access) and `put_batch` (buffered write)
- [ ] **Encryption**: Store-side encryption (client responsibility)

### 18D: CLI & Observability
- [ ] `cas put`, `cas get`, `cas pin`, `cas status`

---

## Milestone 19: Lattice File Sync MVP

File sync over Lattice. Requires M11 (Sync) and M18 (CAS).

### 19A: Filesystem Logic
- [ ] Define `DirEntry` schema: `{ name, mode, cid, modified_at }` in KV Store
- [ ] Map file operations (`write`, `mkdir`, `rename`) to KV Ops

### 19B: FUSE Interface
- [ ] Write `lattice-fs` using the `fuser` crate
- [ ] Mount the Store as a folder on Linux/macOS
- [ ] **Demo:** `cp photo.jpg ~/lattice/` → Syncs to second node

---

## Milestone 20: Wasm Runtime

Replace hardcoded state machines with dynamic Wasm modules.

### 20A: Wasm Integration
- [ ] Integrate `wasmtime` into the Kernel
- [ ] Define minimal Host ABI: `kv_get`, `kv_set`, `log_append`, `get_head`
- [ ] Replace hardcoded `KvStore::apply()` with `WasmRuntime::call_apply()`
- [ ] Map Host Functions to `StorageBackend` calls (M9 prerequisite)

### 20B: Data Structures & Verification
- [ ] Finalize `Intention`, `SignedIntention`, `Hash`, `PubKey` structs for Wasm boundary
- [ ] Wasm-side Intention DAG verification (optional, for paranoid clients)
- **Deliverable:** A "Counter" Wasm module that increments a value when it receives an Op

---

## Milestone 21: N-Node Simulator

Scriptable simulation framework for testing Lattice networking at scale. Built on the `lattice-net-sim` crate (M12B).

- [ ] **Simulator library** (`Simulator` API): Scriptable — `add_node`, `take_offline`, `bring_online`, `join_store`, `put`, `sync`, `assert_converged`
- [ ] **Rhai scripting**: Embed Rhai for scenario scripts (loops, conditionals, dynamic topology changes)
- [ ] **Standalone binary**: CLI that loads and runs `.rhai` scenario files
[ ] **Gate:** 20+ node convergence simulation with metrics: sync calls, items transferred, convergence %, wall-clock time

---

## Milestone 22: Embedded Proof ("Lattice Nano")

Run the kernel on the RP2350.

> Because CLI is already separated from Daemon (M7) and storage is abstracted (M9), only the Daemon needs porting.
> **Note:** Requires substantial refactoring of `lattice-kernel` to support `no_std`.

### 22A: `no_std` Refactoring
- [ ] Split `lattice-kernel` into `core` (logic) and `std` (IO)
- [ ] Replace `wasmtime` (JIT) with `wasmi` (Interpreter) for embedded target
- [ ] Port storage layer to `sequential-storage` (Flash) via `StorageBackend`

### 22B: Hardware Demo
- [ ] Build physical USB stick prototype
- [ ] Implement BLE/Serial transport
- [ ] Sync a file from Laptop → Stick → Phone without Internet

---

## Technical Debt

- [ ] **REGRESSION**: Graceful reconnect after sleep/wake (may fix gossip regression)
- [ ] **Denial of Service (DoS) via Gossip**: Implement rate limiting in GossipManager and drop messages from peers who send invalid data repeatedly.
- [ ] **Optimize `derive_table_fingerprint`**: Currently recalculates the table fingerprint from scratch. For large datasets, this should be optimized to use incremental updates or caching to avoid O(N) recalculation.
- [ ] **DAG Reachability Index**: `DagQueries` methods (`find_lca`, `is_ancestor`, `get_path`) use naive BFS. For large DAGs, add generation numbers (prune impossible ancestors by depth) or bloom filters (compact ancestor summaries) for O(log N) reachability. Not needed until BFS becomes a bottleneck.
- [ ] **Sync Trigger & Bootstrap Controller Review**: Review how and when sync is triggered (currently ad-hoc in `active_peer_ids` or `complete_join_handshake`). Consider introducing a dedicated `BootstrapController` to manage initial sync state, retry logic, and transition to steady-state gossip/sync.
- [ ] **Decouple Iroh transport key from lattice signing identity**: Iroh requires raw `SigningKey` bytes to derive its QUIC node identity (`lattice-net-iroh/src/transport.rs`), which is incompatible with HSM/TPM backends where the private key never leaves the hardware. Options: (1) derive a transport subkey from the HSM if it supports ECDH/key derivation, (2) give Iroh its own ephemeral key and add a post-connect attestation protocol to bind transport identity to lattice `PubKey`, (3) wait for Iroh to support trait-based signing. Option 2 also decouples transport identity from signing identity, which helps with key rotation and multi-device. Trade-off: nodes can no longer assume Iroh `NodeId` == lattice `PubKey`, so a mapping/attestation step is needed after connection.

---

## Discussion

- [x] ~~Does StateMachine need snapshot/restore once we have IntentionStore pruning/snapshots?~~ **Resolved (M16A Step 3):** Removed from `StateMachine`. Not used in production — replication is witness-log-based. M17 will redesign snapshot mechanism for pruning when requirements are clear.
- [ ] Revoking a Peer is untested. How do we ensure removed peers will not receive future Intentions?

---

## Future

- TTL expiry for long-lived orphans (received_at timestamp now tracked)
- Mobile clients (iOS/Android)
- **Root Identity & Key Hierarchy**: Four-layer key model: Seed (24 words, offline) → Root Identity (Ed25519, signs authorizations only) → Device Keys (per-node, sign intentions) → DAG operations. Peers tracked by root identity in SystemStore; each root identity has an authorized device key list. New SystemOps: `DeviceAuthorize(root, device, sig)`, `DeviceRevoke(root, device)`. Device keys validated against authorization chain. Existing DAG/chain/gossip/sync works unchanged at the device-key level.
- **Seed-Based Disaster Recovery**: BIP-39 mnemonic (24 words) generated during onboarding. Recovery flow: enter seed on fresh device → regenerate identity → connect vault-node → revoke lost device keys → resume. Setup flow includes a recovery drill (verify backup words before setup is complete).
- Secure storage of node key (Keychain, TPM)
- **Salted Gossip ALPN**: Use `/config/salt` from root store to salt the gossip ALPN per mesh (improves privacy by isolating mesh traffic).
- **HTTP API**: External access to stores via REST/gRPC-Web (design TBD based on store types)
- **Inter-Store Messaging** (research): Stores exchange typed messages using an actor model. Parent stores have a store-level identity (keypair derived from UUID + node key). Children trust the parent's store key — not individual parent authors — preserving encapsulation. Parent state machines decide what to relay; child state machines process messages as ops in their own DAG. Key design questions: message format, delivery guarantees (at-least-once via DAG?), bidirectional messaging (child→parent replies), and whether messages are durable (intentions) or ephemeral.
- **Audit Trail Enhancements** (HLC `wall_time` already in Intention):
  - Human-readable timestamps in CLI (`store history` shows ISO 8601)
  - Time-based query filters (`store history --from 2026-01-01 --to 2026-01-31`)
  - Identity mapping layer (PublicKey → User name/email)
  - Tamper-evident audit export (signed Merkle bundles for external auditors)
  - Optional: External audit sink (stream to S3/SIEM)
- **Blind Ops / Cryptographic Revealing** (research): Encrypted intention payloads revealed only to nodes possessing specific keys (Convergent Encryption or ZK proofs). Enables selective disclosure within atomic transactions.
- **Epoch Key Encryption**: Shared symmetric key for O(1) gossip payload encryption, enabling efficient peer revocation. See [Epoch Key Encryption](design/epoch-key-encryption/) for the full design.
- **Async/Task-Based Bootstrapping**:
    - Treat bootstrapping as a persistent state/task (`StoreState::Bootstrapping`).
    - Retry indefinitely if peers are unavailable.
    - Recover from "not yet bootstrapped" state on restart.
    - Inviter sends list of potential bootstrap peers in `JoinResponse`.
    - Node can bootstrap from any peer in the list, not just the inviter.
- **Blind Node Relays**: Untrusted VPS relays that sync the raw Intention DAG via Negentropy. No store keys, no Wasm. Can perform graph-based pruning using the `state_independent` flag: prune linear sub-chains below state-independent intentions at the frontier (pure graph operation, no state machine). Full nodes can also push computed snapshots to relays, making them full bootstrap sources. Two-tier pruning: relays do structural pruning (safe by construction), full nodes do semantic pruning (more compact).
