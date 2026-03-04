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

---

## Milestone 15: Review

- [x] ~~**Refactor `SyncSession::run` termination**~~: Fixed via two-phase `SyncDone` handshake with batched `ReconcilePayload` (fan-out messages sent atomically). Replaced broken `reconcile_load` counter with 1:1 payload-level balance, explicit `SyncDone` message, and strict disconnect handling (`Stream closed before SyncDone handshake completed`). See `sync_termination_test.rs`.
- [x] ~~**Fix flaky `test_interleaved_modifications`**~~: Root cause was premature session termination from fan-out counter desync. Fixed by batched `ReconcilePayload` + two-phase `SyncDone`.
- [ ] **Fix `SyncSession` silent ingest error swallowing**: `IntentionResponse` handler (line ~155) silently ignores `ingest_intention` failures via `.is_ok()`. Broken chain errors (`store_prev` pointing to unapplied intention) produce `FATAL: State divergence` log but the error is dropped. Needs: (a) propagate or log the error, (b) investigate out-of-order intention delivery causing chain breaks.
- [ ] Review docs/

### 15A: Error Handling Cleanup
- [x] ~~**`map_err` mechanical cleanup**~~: Added `SystemReadError`, `SystemWriteError`, `IntoStatus` trait, `KvTableResultExt` trait, typed `RuntimeError::Node`, `Decode` variants on `StateDbError`/`KvTableError`, `Transport` variant on `LatticeNetError`, `StoreWrite` variants on `PeerManagerError`/`StoreManagerError`. Eliminated ~50 `map_err` calls across 17 files. All 244 tests pass.
- [ ] **CRITICAL — Fix discarded `witness()` result**: `actor.rs:554` does `let _ = store.witness(...)`. If witnessing fails (disk full, redb corruption), the intention is applied to state but never witnessed — silent divergence between state machine and witness log. Must propagate the error or stall the actor.
- [ ] **Split `IntentionStoreError::InvalidData(String)`**: This single variant carries 20+ distinct failure modes (directory creation, bad signatures, bad pubkeys, chain corruption, range overflow, missing witnesses). Split into `Io`, `Corruption`, `Decode`, `InvalidSignature` etc. so callers can distinguish recoverable from fatal errors.
- [ ] **Type `LatticeNetError` properly**: 7 of 10 variants are `String`-wrapped. `Sync(String)` alone absorbs `SyncError`, `ReconcileError`, transport failures, and stream failures from 6+ call sites. Replace with typed inner errors: `Sync(#[from] SyncError)`, `Reconcile(#[from] ReconcileError<SyncError>)`, and specific transport/stream variants.
- [ ] **Map `IntoStatus` to proper gRPC codes**: Currently all errors become `Status::internal(e.to_string())`. Add variant matching so `NotFound` → `Status::not_found`, `Lock`/`Unauthorized` → `Status::permission_denied`, validation errors → `Status::invalid_argument`, etc.
- [ ] **Type `SystemWatcher::subscribe_events()`**: Returns `Result<Stream<Result<SystemEvent, String>>, String>` — String errors at two levels. Introduce `SystemWatchError` or reuse `SystemReadError`.
- [ ] **Add `tracing::warn!` to silent error discards**: `peer_manager.rs:321` (`get_peers().unwrap_or_default()` in `list_acceptable_authors`), `store_manager.rs:235` (poisoned lock → empty store list), `state_db.rs:115-122` (DB read errors → default meta). These fail silently in ways that affect authorization and store visibility. At minimum log when falling back to defaults.
- [ ] **Clean up `StoreManagerError` junk-drawer variants**: `Store(String)` and `Registry(String)` each absorb multiple unrelated error types. `Registry(String)` even receives `StoreManagerError` itself via `.to_string()` (self-flattening). Split into typed variants or add context enums.
- [ ] **Reduce `StateDbError` variant count** (12 variants): Consider grouping the 5 redb error types (`Database`, `Table`, `Transaction`, `Storage`, `Commit`) into a single `Redb(RedbError)` wrapper since callers never match on individual redb variants.

### 15B: Remove Sleep-Based Polling
All 5 production sleeps are in `lattice-net/src/network/service.rs`. Zero production sleeps exist in any other crate.

- [ ] **Extract `wait_for_store` helper with `Notify`**: 4 identical copy-pasted polling loops (tagged `TODO(15D)`) each poll `get_store(id)` every 5ms up to 10 retries. Replace with a shared `DashMap<Uuid, Arc<tokio::sync::Notify>>` (or similar). `NetworkStoreRegistry::register_store()` signals the `Notify`; callers `await` it with a timeout. Eliminates sleeps at lines ~400, ~1071, ~1095, ~1122.
- [ ] **Replace boot-sync peer-discovery polling with event-driven wake**: `register_store_by_id` spawns a task that polls `active_peer_ids()` every 500ms for up to 60s waiting for the first peer. `spawn_event_listener` already receives `PeerConnected` events via broadcast channel. Add a `Notify` (or `watch::channel<bool>`) signaled on first `PeerConnected`; boot-sync task awaits it instead of polling. Eliminates sleep at line ~378.
- [ ] **Audit test sleeps (28 sites)**: Most test sleeps (polling for convergence, waiting for background tasks) are acceptable. Review the longer ones: `iroh_integration_test.rs:205` (500ms for store registration — should use `StoreReady` event), `iroh_backend_test.rs:150` (500ms for mDNS — inherent, keep). No action needed for race-condition test sleeps (`watch_integration.rs`) or redb lock retry sleeps (`store_manager_test.rs`).

### 15C: Fix `complete_join` Store Type Hardcode

`Node::complete_join()` hardcodes `STORE_TYPE_KVSTORE` because neither the `Invite` token nor the `JoinResponse` proto carries a `store_type` field. This makes it impossible to join non-KV stores and forces `lattice-net-iroh` and `lattice-net` test crates to pull `lattice-kvstore` as a dev-dependency solely for the join flow.

Three hardcode sites: `complete_join()` (`node.rs:466,487`), `Node::start()` fallback (`node.rs:340`).

- [ ] **Add `store_type` to `JoinResponse`**: Add `string store_type = 3` to `JoinResponse` in `lattice-proto/proto/network.proto`. The inviter knows the store type — it owns the store. The joiner receives it in the response and passes it to `store_manager.open()`.
- [ ] **Update `complete_join(store_id, store_type, bootstrap_peers)`**: Add `store_type: &str` parameter. Replace the hardcoded `STORE_TYPE_KVSTORE` with the value received from `JoinResponse`. Update the two call sites: `process_join_response()` (`node.rs:434`) and tests.
- [ ] **Fix `Node::start()` fallback** (`node.rs:340`): The fallback `open(store_id, STORE_TYPE_KVSTORE)` runs when `open_existing()` fails. `open_existing()` reads the persisted store type from the meta store — if that fails, the correct behavior is to skip the store (log an error), not guess KV. Remove the fallback.
- [ ] **Optionally: add `store_type` to `Invite` token**: Add `string store_type = 4` to `InviteToken` in `network.proto` and the `Invite` struct in `token.rs`. This lets the CLI display the store type before joining and allows the joiner to pre-register only the needed opener. Not strictly required (JoinResponse suffices) but improves UX.
- [ ] **Drop `lattice-kvstore` dev-dependency from `lattice-net-iroh` and `lattice-net`**: After the fix, tests can register only `NullState` (or a minimal test opener) for the join flow. Remove the `lattice-kvstore` dev-deps from both `Cargo.toml` files and the explicit comments documenting the workaround (`iroh_integration_test.rs:33`, `common/mod.rs:29`).

---

## Milestone 16: Witness-First Architecture

Invert the apply/witness order so the witness log becomes a true write-ahead log (WAL). State machines become derived projections of the witness log rather than a prerequisite for witnessing.

> **Current flow:** `insert() → floating → deps ready? → state.apply() → store.witness()`
> **New flow:** `insert() → floating → deps ready? → store.witness() → state.apply() (from witness log)`

### Motivation

The system already treats the witness log as the source of truth — `replay_intentions()` on restart derives state from witness-log tips, `is_witnessed()` gates causal deps, and bootstrap replays the peer's witness order. But the code applies state *before* witnessing, creating problems:

- **Discarded witness result** (`actor.rs:554`): `let _ = store.witness(...)` means apply can succeed while witnessing fails (disk full, redb error), silently diverging state from log.
- **Version-skew cascade**: If one author sends a v2 payload that v1 nodes can't decode, the intention stays floating (never witnessed), which blocks all downstream causal deps — potentially freezing the entire store. With witness-first, the intention is witnessed (structurally valid, signature verified) and only the state projection stalls, while sync and other authors continue unaffected.
- **Complex restart path**: `replay_intentions()` compares two independent tip sets (`IntentionStore` author tips vs `StateMachine::applied_chaintips()`) and walks chains to find the gap. With witness-first, replay is trivial: read witness log from `last_applied_seq + 1`.

### Redefined semantics

**Witness** = "I verified this intention's structural integrity (valid signature, correct chain, matching store_id) and committed it to my local ordering." Does NOT imply payload comprehension.

**State projection** = derived view of the witness log. May lag behind the log if the state machine can't decode a payload (version skew). Catches up on upgrade.

### 16A: Core Refactor
- [ ] **Split `apply_ready_intentions` into two phases.** Phase 1: witness all structurally ready intentions (signature valid, chain valid, causal deps witnessed). Phase 2: feed newly witnessed intentions to state machine in witness-log order.
- [ ] **`witness()` becomes the commit point.** Move `witness()` call before `state.apply()` in `apply_intention_to_state`. The witness write must succeed (not `let _ =`); if it fails, the intention stays floating.
- [ ] **State machine tracks `last_applied_seq: u64`** instead of per-author chaintips. After witnessing, the actor reads the witness log from `last_applied_seq + 1` and applies each intention's payload. If `apply()` fails (version skew), the projection stalls at that seq — the witness log keeps advancing.
- [ ] **Drop `StateMachine::applied_chaintips()`** from the trait. State machines no longer track their own tip set. The single `last_applied_seq` stored in a redb meta table is sufficient.
- [ ] **Simplify `replay_intentions()`**: On restart, read witness log entries from `last_applied_seq + 1`, look up each intention by hash, call `state.apply()`. No tip-comparison dance, no chain-walking. If apply stalls (version skew from before restart), report `stalled_at_seq` and stop.

### 16B: Stall Reporting
- [ ] **`StateMachine` exposes stall state.** New method `fn projection_status(&self) -> ProjectionStatus` returning `{ last_applied_seq, witness_head_seq, stalled_at: Option<(u64, String)> }`. Surfaced via `store status` CLI and `StoreMeta` gRPC.
- [ ] **Non-blocking stall.** A stalled projection does NOT block: witnessing continues, sync continues, other stores are unaffected. Only reads against the stalled store's state return stale data (with a warning flag).

### 16C: Bootstrap Alignment
- [ ] **`apply_witnessed_batch` becomes the normal path.** Currently bootstrap is special-cased to iterate witness records. After this refactor, all ingestion follows the same two-phase pattern: witness first, then project. The separate `apply_witnessed_batch` code path can be unified with regular ingest.

### 16D: Payload Validation Strategy Update
- [ ] **Update "stall on failure" contract** in `StateMachine` trait docs. Stall still applies — but the stall point moves from "intention stays floating, never witnessed" to "intention is witnessed, state projection pauses." The effect on the local user is the same (stale state until upgrade). The effect on the network is better (sync and other authors are unblocked).
- [ ] **Update `Payload Validation Strategy` in Technical Debt** to reflect the new semantics.

### 16E: Simplify StateMachine Interface
Collapse the `StateLogic`/`PersistentState` composition into a single `StateMachine` trait. Best done alongside the witness-first refactor since both touch the apply path, trait surface, and restart logic.

- [ ] **Framework-owned backend.** The framework (kernel) owns the redb `Database` and transaction lifecycle. State machines receive a `Storage` trait (`get`/`put`/`delete`/`scan`) scoped to a write transaction — they no longer call `backend().db().begin_write()` themselves.
- [ ] **Single `apply(&self, op: &Op, storage: &mut dyn Storage, dag: &dyn DagQueries)`** replaces the current dual indirection (`StateMachine::apply` → `StateLogic::apply` → `self.mutate()`). State machines are pure functions over `(Op, Storage) → Result`.
- [ ] **`emit()` for watcher notifications (WriterMonad pattern).** State machines call `storage.emit(event)` during `apply()`. Events are buffered and only flushed to watcher channels after the framework commits the transaction. Eliminates `type Updates`, the `notify()` method, and the risk of notifying before commit.
- [ ] **Framework handles chain-tip tracking.** `verify_and_update_tip()` moves from `StateBackend` into the framework's transaction wrapper. State machines no longer see author tips. Combined with 16A, the framework tracks both `last_applied_seq` and per-author tips internally.
- [ ] **Eliminate `PersistentState<T>`, `StateFactory`, `backend()` method.** `KvState` and `SystemLayer` implement `StateMachine` directly. `PersistentState` wrapper and `StateLogic` trait are deleted. `snapshot()`/`restore()` move to a framework-level operation over the owned database.

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
- [ ] **Frontier snapshot via replay:** The frontier is behind the current state. Generating a snapshot at the frontier requires replaying intentions from the last snapshot up to the frontier cut, producing the correct state at that point. Options: tempfile-based replay (works today, heavy on I/O) or in-memory `StateBackend` variant (cleaner, requires refactoring `StateLogic`/`PersistentState`).
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

- [ ] Does StateMachines need snapshot/restore once we have IntentionStore pruning/snapshots?
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
