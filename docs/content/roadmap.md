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
| **M16**          | Uniform Store Traits + Witness-First Architecture: (A) extracted `SystemState` from `SystemLayer`, unified transaction ownership, removed `PersistentState<T>`, slimmed `StateMachine` to `apply()` only; (B) `ScopedDb` for domain crates, `StateBackend` owned by `SystemLayer`, `StateFactory` folded into `StateLogic::create()`, `SystemState` implements `StateLogic`; (C) witness-first — witness log is WAL, `project_new_entries()` is single path into state machine, projection cursor tracks progress; (D) `StoreRegistry` absorbed into `StoreManager`, meta.db `STORES_TABLE` populated, recovery from persisted state; (E) bootstrap security tests (substituted intentions, invalid signatures); (F) `ProjectionStatus` stall reporting, non-blocking projection; (G) `apply_witnessed_batch` collapsed into `apply_ingested_batch`, projection cursor uses witness content hash, `scan_witness_log` takes seq; (H) updated `StateMachine` error contract for witness-first semantics; (I) deleted `StoreTypeProvider` (merged into `StateMachine`/`StateLogic`), removed `backfill_store_types` migration, removed `StoreRegistry` |

---

## Milestone 17: Housekeeping

Test cleanup, dependency review.

- [ ] `store status` should show projection cursor
- [ ] Bug on start: ERROR startup projection failed: State error: Backend error: Projection cursor xxx not found in witness content index store_id=yyy (needs testcase)

### 17A: Test Infrastructure Unification
- [ ] **Unify `test_node_builder`**: 6 copies across 5 files, each registering different opener sets. Consolidate into a single configurable builder in `lattice-mockkernel` (e.g., `TestNodeBuilder::new().with_kv().with_log().with_null().in_memory().build(data_dir)`). Current locations: `lattice-node/src/node.rs`, `lattice-node/tests/store_manager_test.rs`, `lattice-node/tests/multi_author_test.rs`, `lattice-net/tests/common/mod.rs`, `lattice-net/tests/gap_fill_integration.rs`, `lattice-net-iroh/tests/iroh_integration_test.rs`.
- [ ] **Unify `TestHarness`**: two identical structs in `lattice-kvstore/src/state.rs` and `lattice-logstore/src/state.rs` that manage `StateBackend` + `StateContext` + manual write transactions. Extract a generic `TestHarness<S: StateLogic>` into `lattice-mockkernel` (or `lattice-storage` behind a `test-support` feature).
- [ ] **Unify `TestStore`/`TestLogStore`**: `lattice-kvstore/tests/common/mod.rs` and `lattice-logstore/tests/common/mod.rs` are near-identical (`SystemLayer<S>` + `MockWriter<S>`). Extract a generic `TestStore<S: StateLogic>` into `lattice-mockkernel`.
- [ ] **Unify `TestCtx`**: two identical copies in `lattice-node/tests/store_manager_test.rs` and `lattice-node/tests/multi_author_test.rs` (`TempDir` + `Node`). Merge into shared `tests/common/`.
- [ ] **Replace mock state machines with `NullState`**: 6 mock `StateMachine`/`StateLogic` impls across 5 files (`MockStateMachine` x2 in kernel, `MockState` in net, `MockLogic` in systemstore, `TestStateMachine` in kernel). Most are no-op or minimal tracking. Replace with `NullState` where possible; for the kernel actor tests that track applied ops, consider adding an optional callback to `NullState` or keeping a single `TrackingState` in `lattice-mockkernel`.
- [ ] **Deduplicate `MockProvider`**: 3 copies of `NodeProviderExt` mocks across `lattice-net/src/network/service_tests.rs`, `lattice-net-iroh/tests/iroh_backend_test.rs`, `lattice-net-iroh/tests/mock_provider_test.rs`. Extract to `lattice-mockkernel` or a shared test module.
- [ ] **Deduplicate helper functions**: `wrap_app_data` (3 copies), `put_kv`/`wait_for_key` (3+ copies), `temp_data_dir` (2 copies), `create_test_op` (2 copies). Move to `lattice-mockkernel` or appropriate `tests/common/`.

### 17B: Test Scope Review
- [ ] Review all integration tests in `lattice-net` and `lattice-kvstore`. Replace `KvState` with `NullState` wherever the test does not exercise KV-specific logic.
- [ ] Move `lattice-kvstore/tests/sync_compliance.rs` to `lattice-systemstore/tests/`. Tests chain rules, snapshot/restore, convergence via `SystemLayer`, not KV-specific logic. Use `NullState` where possible, keep `KvState` only for tests that need real merge behavior.
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

### 18E: Hash Index Optimization ✅
- [x] Replace in-memory `HashSet<Hash>` with on-disk index (`TABLE_WITNESS_INDEX` in redb)
- [x] Support 100M+ intentions without excessive RAM

### 18F: Advanced Sync Optimization (Future)
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

## Milestone 19: Store Bootstrap

Rework the bootstrap protocol for both root and child stores. Witness records are the trust chain — they prove each intention was authorized at the time it was witnessed.

Root store bootstrap requires witness records because the joining node has no peer list yet and can't independently verify intention signatures. It relies on the inviter's witness signatures to vouch for each intention.

Child stores currently use Negentropy (set reconciliation) instead of the bootstrap protocol. This works only because peer revocation is unimplemented. Once peers can be revoked, a syncing node can't distinguish "authored by someone authorized at the time" from "authored by a now-revoked peer." Witness records from an authorized peer prove the intention was valid when witnessed.

### 19A: Split Witness and Intention Transfer
- [ ] **Transfer witness records only during bootstrap.** Witness records are small (~150 bytes each). Node verifies inviter's witness signatures, extracts intention hashes, filters out already-present ones, fetches missing intentions via existing `FetchIntentions` protocol.
- [ ] **Restart-from-zero on failure.** If bootstrap fails mid-way, retry from seq 0 with the same or different inviter. Witness re-transfer is cheap (100k records ≈ 15MB). Only intentions the node doesn't already have are re-fetched. No multi-peer resume — witness log order is node-local, cursors are not portable between peers.

### 19B: Child Store Bootstrap
- [ ] **Extend bootstrap protocol to child stores.** Currently Negentropy-only. Required once peer revocation is implemented — without witness records, a syncing node can't verify historical authorization of intentions from revoked authors.
- [ ] **`RecursiveWatcher` triggers bootstrap, not just sync.** Currently child stores are opened empty and rely on `sync_all_by_id`. Should use the bootstrap protocol to transfer witness records from an already-connected peer.

### 19C: Bootstrap Controller
- [ ] **Persistent bootstrap state.** Treat bootstrapping as a persistent task (`StoreState::Bootstrapping`). Recover from "not yet bootstrapped" state on restart. Retry if peers are unavailable.
- [ ] **Bootstrap peer list.** Inviter sends list of potential bootstrap peers in `JoinResponse`. Node can bootstrap from any peer in the list, not just the inviter.
- [ ] **Transition to steady-state.** Clean handoff from bootstrap to gossip/Negentropy sync once the initial clone is complete.

### 19D: Pruning-Aware Bootstrap
- [ ] **Two-phase protocol for pruned stores.** Negentropy cannot sync from an implicit zero-genesis if the store has been pruned. Phase 1: request snapshot from peer. Phase 2: bootstrap witness log tail after the snapshot's causal frontier.
- [ ] **Snapshot + tail bootstrap for new peers.** Instead of full log replay, send a snapshot at the stability frontier plus the witness log tail.

---

## Milestone 20: Content-Addressable Store (CAS)

Node-local content-addressable blob storage. Replication policy managed separately. Requires M11 and M12.

### 20A: Low-Level Storage (`lattice-cas`)
- [ ] `CasBackend` trait interface (`fs`, `block`, `s3`)
- [ ] Isolation: Mandatory `store_id` for all ops
- [ ] `redb` metadata index: ARC (Adaptive Replacement Cache), RefCounting
- [ ] `FsBackend`: Sharded local disk blob storage

### 20B: Replication & Safety (`CasManager`)
- [ ] `ReplicationPolicy` trait: `crush_map()` and `replication_factor()` from System Table
- [ ] `StateMachine::referenced_blobs()`: Pinning via State declarations
- [ ] Pull-based reconciler: `ensure(cid)`, `calculate_duties()`, `gc()` with Soft Handoff

### 20C: Wasm & FUSE Integration
- [ ] **Wasm**: Host Handles (avoid linear memory copy)
- [ ] **FUSE**: `get_range` (random access) and `put_batch` (buffered write)
- [ ] **Encryption**: Store-side encryption (client responsibility)

### 20D: CLI & Observability
- [ ] `cas put`, `cas get`, `cas pin`, `cas status`

---

## Milestone 21: Lattice File Sync MVP

File sync over Lattice. Requires M11 (Sync) and M20 (CAS).

### 21A: Filesystem Logic
- [ ] Define `DirEntry` schema: `{ name, mode, cid, modified_at }` in KV Store
- [ ] Map file operations (`write`, `mkdir`, `rename`) to KV Ops

### 21B: FUSE Interface
- [ ] Write `lattice-fs` using the `fuser` crate
- [ ] Mount the Store as a folder on Linux/macOS
- [ ] **Demo:** `cp photo.jpg ~/lattice/` → Syncs to second node

---

## Milestone 22: Wasm Runtime

Replace hardcoded state machines with dynamic Wasm modules.

### 22A: Wasm Integration
- [ ] Integrate `wasmtime` into the Kernel
- [ ] Define minimal Host ABI: `kv_get`, `kv_set`, `log_append`, `get_head`
- [ ] Replace hardcoded `KvStore::apply()` with `WasmRuntime::call_apply()`
- [ ] Map Host Functions to `StorageBackend` calls (M9 prerequisite)

### 22B: Data Structures & Verification
- [ ] Finalize `Intention`, `SignedIntention`, `Hash`, `PubKey` structs for Wasm boundary
- [ ] Wasm-side Intention DAG verification (optional, for paranoid clients)
- **Deliverable:** A "Counter" Wasm module that increments a value when it receives an Op

---

## Milestone 23: N-Node Simulator

Scriptable simulation framework for testing Lattice networking at scale. Built on the `lattice-net-sim` crate (M12B).

- [ ] **Simulator library** (`Simulator` API): Scriptable — `add_node`, `take_offline`, `bring_online`, `join_store`, `put`, `sync`, `assert_converged`
- [ ] **Rhai scripting**: Embed Rhai for scenario scripts (loops, conditionals, dynamic topology changes)
- [ ] **Standalone binary**: CLI that loads and runs `.rhai` scenario files
[ ] **Gate:** 20+ node convergence simulation with metrics: sync calls, items transferred, convergence %, wall-clock time

---

## Milestone 24: Embedded Proof ("Lattice Nano")

Run the kernel on the RP2350.

> Because CLI is already separated from Daemon (M7) and storage is abstracted (M9), only the Daemon needs porting.
> **Note:** Requires substantial refactoring of `lattice-kernel` to support `no_std`.

### 24A: `no_std` Refactoring
- [ ] Split `lattice-kernel` into `core` (logic) and `std` (IO)
- [ ] Replace `wasmtime` (JIT) with `wasmi` (Interpreter) for embedded target
- [ ] Port storage layer to `sequential-storage` (Flash) via `StorageBackend`

### 24B: Hardware Demo
- [ ] Build physical USB stick prototype
- [ ] Implement BLE/Serial transport
- [ ] Sync a file from Laptop → Stick → Phone without Internet

---

## Technical Debt

- [ ] **REGRESSION**: Graceful reconnect after sleep/wake (may fix gossip regression)
- [ ] **REGRESSION**: `node set-name` creates two intentions with the same op per store. `Node::set_name` calls `publish_name_to()` for each active store, which each creates a `PeerOp::SetName` intention via `SystemBatch`. Investigate whether duplicate submissions are happening (e.g., store list contains duplicates, or the call is invoked twice).
- [ ] **Denial of Service (DoS) via Gossip**: Implement rate limiting in GossipManager and drop messages from peers who send invalid data repeatedly.
- [ ] **Optimize `derive_table_fingerprint`**: Currently recalculates the table fingerprint from scratch. For large datasets, this should be optimized to use incremental updates or caching to avoid O(N) recalculation.
- [ ] **DAG Reachability Index**: `DagQueries` methods (`find_lca`, `is_ancestor`, `get_path`) use naive BFS. For large DAGs, add generation numbers (prune impossible ancestors by depth) or bloom filters (compact ancestor summaries) for O(log N) reachability. Not needed until BFS becomes a bottleneck.
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
- **Blind Node Relays**: Untrusted VPS relays that sync the raw Intention DAG via Negentropy. No store keys, no Wasm. Can perform graph-based pruning using the `state_independent` flag: prune linear sub-chains below state-independent intentions at the frontier (pure graph operation, no state machine). Full nodes can also push computed snapshots to relays, making them full bootstrap sources. Two-tier pruning: relays do structural pruning (safe by construction), full nodes do semantic pruning (more compact).
