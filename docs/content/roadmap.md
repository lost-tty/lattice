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
    fn last_applied_witness(&self) -> Result<Hash, String>;
    fn set_last_applied_witness(&self, hash: Hash) -> Result<(), String>;
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
- [x] Make `SystemState` implement `StateLogic` (same trait as `KvState`/`LogState`). `SystemState` is now an owned struct with `ScopedDb` (scoped to `TABLE_SYSTEM`), `create(ScopedDb)`, and `apply(&self, &mut Table, op, dag) -> Result<Vec<SystemEvent>>`. `SystemLayer` holds both `inner: S` and `system: SystemState`.
- [x] `SystemState` emits `SystemEvent` via `broadcast::Sender` in `notify()`, same as `KvState` emits `WatchEvent`. Deleted `subscribe_system_events()` and `decode_system_event()` — events are emitted inline from the apply match arms where the data is already decoded. `SystemWatcher` blanket now uses `SystemReader::subscribe_system_events()` (backed by the broadcast channel) instead of re-parsing the witness log.

### 16C: Witness-First Core

Remaining decisions for witness-first:
- [x] **`last_applied_witness: (u64, Hash)`**: framework-owned. Written by `project_new_entries()` after each successful state projection. Read on restart to resume. Stored as `ProjectionCursor` protobuf in `TABLE_META`.
- [x] **`stalled_at` removed**: stall is implicit — gap between `ProjectionCursor` and witness log head. No separate record needed.
- [x] Decide if chain tips and witness cursor should remain in `TABLE_META` or split into a separate table.

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

- [x] **`witness_ready()`**: Loop through floating intentions whose deps are witnessed. Witness each one. Do not touch state. Returns count of newly witnessed entries.
- [x] **`project_new_entries()`**: Free function. Read witness log from `last_applied_witness` forward. For each entry, build `Op`, call `state.apply()`. Update `ProjectionCursor` after each successful projection. If projection fails (version skew), stop — the witness log keeps advancing independently.
- [x] **`witness()` becomes the commit point.** Must succeed (not `let _ =`); if it fails, the intention stays floating.
- [x] **Drop `StoreIdentity::applied_chaintips()`**. Removed from trait, SystemLayer impl, StateBackend getter, and all mock impls. Per-author chain tips remain in state.db (needed by `verify_and_update_tip`) but are no longer exposed. `MockWriter` tracks `prev_hash` internally instead of reading from `StoreIdentity`.
- [x] **`replay_intentions()` becomes `project_new_entries()`** — same code path for restart, post-witness, and post-bootstrap. No tip-comparison, no chain-walking.

### 16D: Merge StoreRegistry into StoreManager & State Recovery

Eliminated dual-cache architecture (`StoreRegistry` + `StoreManager`). Single `StoreManager` owns store lifecycle. Openers are pure factories. `STORES_TABLE` in meta.db is now populated (was always empty — dead code path).

- [x] **Step 1 — Opener becomes pure factory.** `StoreOpener::open(store_id, configs, identity) -> OpenedStoreBundle`. No reference to StoreManager.
- [x] **Step 2 — StoreRegistry absorbed into StoreManager.** Single struct, single cache. `store_registry.rs` deleted.
- [x] **Step 3 — Meta.db registration.** `register(store_id, parent_id, ...)` writes to STORES_TABLE with correct parent_id. `store_list()` reads store_type from STORES_TABLE.
- [x] **Step 4 — Recovery.** `open_existing()` resolves type from STORES_TABLE. `Node::start()` logs actual errors.
- [x] **Step 5 — CLI.** `node meta` command dumps meta.db tables as s-expressions.
- [x] **Step 6 — Tests:**
  - [x] `test_child_store_has_correct_parent_in_meta`
  - [x] `test_create_store_populates_stores_table`
  - [x] `test_joined_store_populates_stores_table`
  - [x] `test_state_db_recovery`
  - [x] `test_open_existing_resolves_type_from_meta`

### 16E: Bootstrap Security Tests
- [x] **Test: `apply_witnessed_batch` rejects substituted intentions.** Hand over witness records for intention A but substitute intention B in the intentions list. Verify B is not inserted/witnessed.
- [x] **Test: `apply_witnessed_batch` rejects invalid witness signatures.** Send witness records with a bad signature. Verify nothing is inserted.

### 16F: Stall Reporting
- [x] **Framework exposes projection status** via `ProjectionStatus`: `{ last_applied_seq, last_applied_hash, witness_head_seq, witness_head_hash }`. Gap between cursor and head indicates stall. Surfaced via `store status` CLI (`Projection: N/M`, `(STALLED)` when behind).
- [x] **Non-blocking stall.** Witnessing continues, sync continues, other stores are unaffected. `project_new_entries` stops at the first failing entry and returns `Ok(count)` — no error propagation. Stall is surfaced via `ProjectionStatus` / `store status (STALLED)`.

### 16G: Bootstrap Alignment
- [x] **`apply_witnessed_batch` collapsed into `apply_ingested_batch`.** Verification layer extracts authorized intentions from peer witness records, then delegates to the standard ingestion path (insert → `witness_ready` → `project_new_entries`). No separate code path.
- [x] **Switch projection cursor to witness content hash.** `last_applied_witness` stores `blake3(WitnessRecord.content)`. `TABLE_WITNESS_CONTENT_INDEX` maps content hash → seq. `scan_witness_log` takes `start_seq: u64` directly — callers resolve externally. `project_new_entries` stores content hash, resolves via `get_content_seq_for()`. Bootstrap protocol uses `start_seq`/`last_seq` (no hashes). `SyncProvider` streaming paginates by seq.
- [x] **`scan_witness_log` takes seq, not hash.** Removed hash→seq resolution from inside `scan_witness_log`. All callers (projection, bootstrap handler, bootstrap client, `SyncProvider` stream) pass a `u64` seq directly. Bootstrap proto changed from `start_hash bytes` to `start_seq uint64`, response includes `last_seq`.

### 16H: Payload Validation Strategy Update
- [ ] **Update "stall on failure" contract** in `StateMachine` trait docs. Stall still applies — but the stall point moves from "intention stays floating, never witnessed" to "intention is witnessed, state projection pauses." The effect on the local user is the same (stale state until upgrade). The effect on the network is better (sync and other authors are unblocked).

### 16I: Cleanup

- [ ] Move `lattice-kvstore/tests/sync_compliance.rs` to `lattice-systemstore/tests/` — tests chain rules, snapshot/restore, convergence via `SystemLayer`, not KV-specific logic. Use `NullState` where possible, keep `KvState` only for tests that need real merge behavior.
- [x] Remove `MetaStore::backfill_store_types()` migration (all nodes migrated).
- [x] Remove `store_registry.rs` and `StoreRegistry` re-exports (verified: file deleted, type gone, no external consumers).
- [x] Merge `StoreTypeProvider` into `StateMachine` / `StateLogic` — `store_type()` now lives on both traits. `StoreTypeProvider` trait deleted. One fewer bound in `StoreHandle`/`direct_opener` where clauses.

---

## Milestone 17: Housekeeping

Review and clean up tests, reduce unnecessary dependencies, fix multi-peer bootstrap.

### 17A: Multi-Peer Bootstrap
- [ ] **Multi-peer bootstrap is broken.** Witness log order is node-local — each node witnesses in its own arrival order. Resuming from peer A's cursor on peer B skips entries regardless of hash type. Multi-peer failover needs set reconciliation, not linear log scanning.

### 17B: Test Review & NullStore Migration
- [ ] Review all integration tests — replace `KvStore` with `NullStore` wherever the test does not exercise KV-specific logic (chain rules, sync compliance, actor lifecycle, etc.).
- [ ] Unify `test_node_builder` and `file_node_builder` helpers into a single shared builder (see Technical Debt).
- [ ] Scope domain crate tests closer — add generic `TestHarness<S: StateLogic>` to `lattice-storage` (behind `test-support` feature), replace duplicated harnesses in `lattice-kvstore` and `lattice-logstore` unit tests. Internal `#[cfg(test)]` tests already call `StateLogic::apply` directly (correct); integration tests in `tests/` go through `SystemLayer` (correct). Only the boilerplate needs dedup.
- [ ] Audit test coverage gaps exposed by the migration.

### 17C: Proto Definition Audit
- [ ] Audit `lattice-proto`: identify proto definitions that are only consumed by a single crate. Move those definitions into the consuming crate's own proto files. If all definitions can be relocated, remove `lattice-proto` entirely.

### 17D: Crate Dependency Graph Review
- [ ] Review the dependency graph between workspace crates. Identify unnecessary or circular dependencies, overly broad re-exports, and crates that pull in more than they need. Clean up after M13 and M16 reshuffling.

---

## Milestone 18: Log Lifecycle & Pruning

Manage log growth on long-running nodes via stability frontier, snapshots, pruning, and finality checkpoints.

> **See:** [Stability Frontier](stability-frontier.md) for the full design.

### 18A: Stability Frontier & Tip Attestations
- [ ] `SystemOp::TipAttestation { tips: Vec<(PubKey, Hash)> }` — publish changed author tips as SystemOps
- [ ] Attestation keys in `TABLE_SYSTEM`: `attestation/{attester}/{author} → Hash` (LWW by HLC)
- [ ] Derive per-author frontier: `frontier[A] = min(tip[A] across all Active peers)`
- [ ] Attestation triggers: post-sync, batch threshold, periodic heartbeat (~15 min), graceful shutdown
- [ ] **Lag metric:** Per-peer divergence from local tips, surfaced via `NodeEvent::PeerLagWarning` and `store status`

### 18B: Snapshotting
- [ ] **Frontier snapshot via replay:** The frontier is behind the current state. Generating a snapshot at the frontier requires replaying intentions from the last snapshot up to the frontier cut, producing the correct state at that point. Options: tempfile-based replay (works today, heavy on I/O) or in-memory `StateBackend` variant (cleaner). **Note:** `StateMachine::snapshot()`/`restore()` were removed in M16A Step 3 — the old `StateBackend::snapshot_internal()` serialization format is deleted. M18 will design a new snapshot mechanism tailored to pruning requirements (frontier-aware, incremental, possibly stored in a separate `snapshot.db`).
- [ ] Store snapshots in `snapshot.db` (per-store, includes `TABLE_SYSTEM` attestation state)
- [ ] Bootstrap new peers from snapshot + tail sync instead of full log replay
- [ ] **Future optimization:** Inverse operations stored in `WitnessRecord` could allow rolling back from current state to frontier in O(head − frontier), avoiding replay. Deferred until profiling shows replay is a bottleneck.

### 18C: Pruning
- [ ] `truncate_prefix` for all intentions per author up to `frontier[A]`
- [ ] Discard individual attestation intentions below the frontier (snapshot carries state forward)
- [ ] Preserve intentions newer than frontier

### 18C½: Store-Level Pruning Hints
Store-defined intention metadata that guides consensus pruning. Both hints are advisory — pruning only happens once all peers have attested past the relevant intentions (standard frontier rules).
- [ ] **Supersede hint** (`supersedes_all: true`): Intention declares it supersedes all prior state (e.g., a full snapshot-as-intention, or a state reset). Everything causally before it is safe to prune. The pruner can treat it as a new root, discarding the entire prefix. Useful for stores that periodically compact their state into a single intention.
- [ ] **Ephemeral hint** (`ephemeral: true`): Intention declares it is unlikely to ever be a causal dependency (e.g., a key deletion in a KV store — no future operation will depend on the deleted value). Chains ending at ephemeral intentions may be pruned more aggressively — once all peers have witnessed them past the frontier, the pruner can discard them without waiting for a full snapshot cycle.
- [ ] Wire hints through `Intention` proto field (bitflags or enum), surface in `Op` for state machines, respect in `truncate_prefix` (18C).

### 18D: Checkpointing / Finality
- [ ] Periodically finalize state hash (protect against "Deep History Attacks")
- [ ] Signed checkpoint intentions in sigchain
- [ ] Nodes reject intentions that contradict finalized checkpoints

### 18E: Recursive Store Bootstrapping (Pruning-Aware)
- [ ] `RecursiveWatcher` identifies child stores from live state, not just intention logs.
- [ ] Bootstrapping a child store requires a **Two-Phase Protocol** because Negentropy cannot sync from an implicit zero-genesis if the store has been pruned.
- [ ] **Phase 1 (Snapshot):** Request the opaque snapshot (`state.db`) from the peer for the discovered child store.
- [ ] **Phase 2 (Tail Sync):** Run Negentropy to sync floating intentions that occurred *after* the snapshot's causal frontier.

### 18F: Hash Index Optimization ✅
- [x] Replace in-memory `HashSet<Hash>` with on-disk index (`TABLE_WITNESS_INDEX` in redb)
- [x] Support 100M+ intentions without excessive RAM

### 18G: Advanced Sync Optimization (Future)
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

## Milestone 19: Content-Addressable Store (CAS)

Node-local content-addressable blob storage. Replication policy managed separately. Requires M11 and M12.

### 19A: Low-Level Storage (`lattice-cas`)
- [ ] `CasBackend` trait interface (`fs`, `block`, `s3`)
- [ ] Isolation: Mandatory `store_id` for all ops
- [ ] `redb` metadata index: ARC (Adaptive Replacement Cache), RefCounting
- [ ] `FsBackend`: Sharded local disk blob storage

### 19B: Replication & Safety (`CasManager`)
- [ ] `ReplicationPolicy` trait: `crush_map()` and `replication_factor()` from System Table
- [ ] `StateMachine::referenced_blobs()`: Pinning via State declarations
- [ ] Pull-based reconciler: `ensure(cid)`, `calculate_duties()`, `gc()` with Soft Handoff

### 19C: Wasm & FUSE Integration
- [ ] **Wasm**: Host Handles (avoid linear memory copy)
- [ ] **FUSE**: `get_range` (random access) and `put_batch` (buffered write)
- [ ] **Encryption**: Store-side encryption (client responsibility)

### 19D: CLI & Observability
- [ ] `cas put`, `cas get`, `cas pin`, `cas status`

---

## Milestone 20: Lattice File Sync MVP

File sync over Lattice. Requires M11 (Sync) and M19 (CAS).

### 20A: Filesystem Logic
- [ ] Define `DirEntry` schema: `{ name, mode, cid, modified_at }` in KV Store
- [ ] Map file operations (`write`, `mkdir`, `rename`) to KV Ops

### 20B: FUSE Interface
- [ ] Write `lattice-fs` using the `fuser` crate
- [ ] Mount the Store as a folder on Linux/macOS
- [ ] **Demo:** `cp photo.jpg ~/lattice/` → Syncs to second node

---

## Milestone 21: Wasm Runtime

Replace hardcoded state machines with dynamic Wasm modules.

### 21A: Wasm Integration
- [ ] Integrate `wasmtime` into the Kernel
- [ ] Define minimal Host ABI: `kv_get`, `kv_set`, `log_append`, `get_head`
- [ ] Replace hardcoded `KvStore::apply()` with `WasmRuntime::call_apply()`
- [ ] Map Host Functions to `StorageBackend` calls (M9 prerequisite)

### 21B: Data Structures & Verification
- [ ] Finalize `Intention`, `SignedIntention`, `Hash`, `PubKey` structs for Wasm boundary
- [ ] Wasm-side Intention DAG verification (optional, for paranoid clients)
- **Deliverable:** A "Counter" Wasm module that increments a value when it receives an Op

---

## Milestone 22: N-Node Simulator

Scriptable simulation framework for testing Lattice networking at scale. Built on the `lattice-net-sim` crate (M12B).

- [ ] **Simulator library** (`Simulator` API): Scriptable — `add_node`, `take_offline`, `bring_online`, `join_store`, `put`, `sync`, `assert_converged`
- [ ] **Rhai scripting**: Embed Rhai for scenario scripts (loops, conditionals, dynamic topology changes)
- [ ] **Standalone binary**: CLI that loads and runs `.rhai` scenario files
[ ] **Gate:** 20+ node convergence simulation with metrics: sync calls, items transferred, convergence %, wall-clock time

---

## Milestone 23: Embedded Proof ("Lattice Nano")

Run the kernel on the RP2350.

> Because CLI is already separated from Daemon (M7) and storage is abstracted (M9), only the Daemon needs porting.
> **Note:** Requires substantial refactoring of `lattice-kernel` to support `no_std`.

### 23A: `no_std` Refactoring
- [ ] Split `lattice-kernel` into `core` (logic) and `std` (IO)
- [ ] Replace `wasmtime` (JIT) with `wasmi` (Interpreter) for embedded target
- [ ] Port storage layer to `sequential-storage` (Flash) via `StorageBackend`

### 23B: Hardware Demo
- [ ] Build physical USB stick prototype
- [ ] Implement BLE/Serial transport
- [ ] Sync a file from Laptop → Stick → Phone without Internet

---

## Technical Debt

- [ ] **Unify test node builders**: Find all `test_node_builder` and `file_node_builder` helpers across tests and consolidate into a single shared builder.
- [ ] **REGRESSION**: Graceful reconnect after sleep/wake (may fix gossip regression)
- [ ] **Denial of Service (DoS) via Gossip**: Implement rate limiting in GossipManager and drop messages from peers who send invalid data repeatedly.
- [ ] **Optimize `derive_table_fingerprint`**: Currently recalculates the table fingerprint from scratch. For large datasets, this should be optimized to use incremental updates or caching to avoid O(N) recalculation.
- [ ] **DAG Reachability Index**: `DagQueries` methods (`find_lca`, `is_ancestor`, `get_path`) use naive BFS. For large DAGs, add generation numbers (prune impossible ancestors by depth) or bloom filters (compact ancestor summaries) for O(log N) reachability. Not needed until BFS becomes a bottleneck.
- [ ] **Sync Trigger & Bootstrap Controller Review**: Review how and when sync is triggered (currently ad-hoc in `active_peer_ids` or `complete_join_handshake`). Consider introducing a dedicated `BootstrapController` to manage initial sync state, retry logic, and transition to steady-state gossip/sync.
- [ ] **Move `STORE_TYPE_KVSTORE` / `STORE_TYPE_LOGSTORE`** out of `lattice-model` into their respective store crates. `lattice-model` shouldn't know about specific store types.
- [ ] **Stale author chaintips in state.db**: `verify_and_update_tip` writes `tip/<pubkey>` entries but nothing ever removes them. Revoked authors leave dead records. Low priority (64 bytes per author), but snapshots include stale data.
- [ ] **Decouple Iroh transport key from lattice signing identity**: Iroh requires raw `SigningKey` bytes to derive its QUIC node identity (`lattice-net-iroh/src/transport.rs`), which is incompatible with HSM/TPM backends where the private key never leaves the hardware. Options: (1) derive a transport subkey from the HSM if it supports ECDH/key derivation, (2) give Iroh its own ephemeral key and add a post-connect attestation protocol to bind transport identity to lattice `PubKey`, (3) wait for Iroh to support trait-based signing. Option 2 also decouples transport identity from signing identity, which helps with key rotation and multi-device. Trade-off: nodes can no longer assume Iroh `NodeId` == lattice `PubKey`, so a mapping/attestation step is needed after connection.

---

## Discussion

- [x] ~~Does StateMachine need snapshot/restore once we have IntentionStore pruning/snapshots?~~ **Resolved (M16A Step 3):** Removed from `StateMachine`. Not used in production — replication is witness-log-based. M18 will redesign snapshot mechanism for pruning when requirements are clear.
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
