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

---

## Milestone 14: Slim Down Persistent State

Reduce what state machines store per conflict domain. Currently each key persists a full `HeadList` with duplicated values, HLC, author, and tombstone flags per head. Replace with: a materialized value (the resolved projection) plus a list of intention hashes (pointers into the DAG). Values, timestamps, and authorship live in the DAG — the state machine stores just enough to track conflicts and build `causal_deps`.

> **See:** [Conflict Resolution Architecture](design/meet-vs-crdt/) for the full design analysis.
>
> **Key insight:** The `Head` struct duplicates data that already exists in the intention DAG. Each head's value is a copy of the intention payload; its HLC, author, and hash are copies of intention metadata. By storing only intention hashes per conflict domain, state machines become thinner while retaining the ability to detect conflicts, build `causal_deps`, and surface branch information to clients. Conflict semantics (what constitutes a conflict domain, how to resolve) remain store-specific — the kernel provides DAG primitives, each state machine decides how to use them.

### 14A: DAG Query Primitives ✅
- [x] **`DagQueries` trait.** Defined in `lattice-model/src/dag_queries.rs`. Interface for state machines to query the intention DAG without depending on `IntentionStore` internals. Mockable for testing.
- [x] **`IntentionInfo` struct.** `{ hash, payload, timestamp, author }` — owned intention data without DAG plumbing. Return type for DAG queries.
- [x] **`get_intention(hash) → IntentionInfo`:** Dereferences an intention hash into its data.
- [x] **`find_lca(a, b) → Hash`:** Lowest common ancestor via alternating bidirectional BFS over `Condition::V1` deps + `store_prev` edges.
- [x] **`get_path(from, to) → Vec<IntentionInfo>`:** Intentions between two DAG points in topological order. Reverse BFS to discover subgraph + Kahn's sort.
- [x] **`is_ancestor(ancestor, descendant) → bool`:** DAG reachability via backward BFS from descendant.
- [x] **Implemented on `IntentionStore`.** Synchronous over redb. Shared `dag_parents()` helper eliminates duplication across methods. No async wrapper needed — state machines run synchronously inside the actor which already holds the `IntentionStore`.

### 14B: `KVTable` — Unified State Engine
- [x] **Extract a generic `KVTable` from KvState and SystemTable.** Both implement identical `apply_head()` logic — causal subsumption, idempotency check, deterministic sort, encode/store. Both use the same underlying format (`TableDefinition<&[u8], &[u8]>` with protobuf-encoded `HeadList` values). Pure refactor — no format change, same behavior, shared code.
- [x] **`KVTable::apply()`:** Wraps the existing `apply_head()` logic. Takes key, new head, causal_deps. One implementation replaces the duplicate in KvState and SystemTable.
- [x] **`KVTable::get()`:** Returns decoded `HeadList` for a key (same as current behavior, just shared).
- [x] **`KVTable::heads()`:** Returns head hashes for a key. Used for `causal_deps` on writes and conflict surfacing.
- [x] **KvState uses `KVTable`.** `mutate()` decodes `KvPayload`, calls `KVTable::apply()` per put/delete. `get()` delegates to `KVTable::get()`. `apply_head()` removed.
- [x] **SystemTable uses `KVTable`.** All `set_*`/`add_*`/`remove_*` methods construct the string key and encoded value, then call `KVTable::apply()`. `apply_head()` removed. The typed accessor methods stay — they provide the key schema and value encoding — but the engine underneath is shared.
- [x] **Future store types get `KVTable` for free.** A document store, filesystem metadata store, or any KV-shaped conflict domain uses the same engine out of the box.

### 14C: KVTable API
- [x] **`KVTable::get()` returns materialized value.** `get() -> Option<Vec<u8>>` instead of `Vec<Head>`. `None` for missing keys or tombstone-only. Callers no longer call `.lww()`.
- [x] **Migrate KvState callers.** `handle_get` uses `get()` directly. `handle_put`/`handle_delete`/`handle_batch` use `heads()` for causal deps. Remove `.lww()` calls. `scan()` returns `(key, Option<value>)` instead of `(key, Vec<Head>)`.
- [x] **Migrate SystemTable callers.** `ReadOnlySystemTable` owns a `ReadOnlyKVTable` (not a raw redb table). Point lookups delegate to `get()`/`heads()`. `get_deps()` removed — callers use `head_hashes()` directly. Range sites (`get_peers`, `get_children`, `list_all`) use `ReadOnlyKVTable::range()`/`iter()` via `LwwRange` iterator. No crate outside `lattice-kvtable` calls `decode_heads` or `decode_lww`.
- [x] **`ReadOnlyKVTable::range()` and `iter()`.** `LwwRange` iterator adapter wraps redb `Range`, decodes `HeadList` and LWW-resolves each value internally. Yields `(Vec<u8>, Option<Vec<u8>>)` — owned key bytes and resolved value (`None` for tombstones). Callers never see raw proto encoding.
- [x] **`KvState::mutate()` returns resolved values.** `apply_head()` returns `ApplyResult { value: Option<Vec<u8>> }` (LWW-resolved) instead of `HeadChange { new_heads: Vec<Head> }`. Write-path callers no longer call `lww()` on results.

### 14D: Slim Down Storage Format ✅
- [x] **`StateMachine::apply` receives `&dyn DagQueries`.** Threaded from kernel actor (`IntentionStore` implements `DagQueries`) through `StateMachine::apply` → `StateLogic::mutate` → `KVTable::apply_head`, and `SystemLayer::apply` → `SystemTable`.
- [x] **`KVTable::apply()` resolves LWW at write time.** Compares incoming HLC/author against current winner via DAG lookup. `get()` returns the value directly — no more read-time resolution.
- [x] **New on-disk format.** `proto::Value { oneof kind { value | tombstone }, heads[] }` in `lattice-kvtable/proto/value.proto`. `heads[0]` is the LWW winner. HLC/author not stored — looked up from the DAG on demand. `oneof` distinguishes live entries (including empty values) from tombstones at the type level.
- [x] **Removed `HeadList`/`HeadInfo` from `storage.proto`.** Dead code — `lattice-kvtable` no longer depends on `lattice-proto`. Conversion impls (`Head ↔ HeadInfo`) and re-exports (`lattice-storage::head`, `lattice-kvstore::Head`) removed.
- [x] **Moved `kv_store.proto` to `lattice-kvstore/proto/`.** KV service/payload protos (`PutRequest`, `KvPayload`, `WatchEventProto`, etc.) compiled locally. `lattice-kvstore` and `lattice-kvstore-api` no longer depend on `lattice-proto`. `Operation` helper impls moved to `lattice-kvstore::proto`.

### 14E: Clean Up `StateMachine` Interface ✅
- [x] **`Op` embeds `IntentionInfo`.** `Op { info: IntentionInfo<'a>, causal_deps, prev_hash }`. `IntentionInfo` uses `Cow<'a, [u8]>` for payload — borrowed in `Op` (from in-flight intention), owned in `DagQueries` results (from DB). Same type in both contexts. `prev_hash` stays in `Op` because `verify_and_update_tip` needs it atomically in the same write transaction.
- [x] **Remove `Head` struct from `lattice-model`.** No longer persisted. Intention metadata (HLC, author) is accessed from the DAG when needed (conflict reads, HITL).
- [x] **Remove `Merge` trait from `lattice-model`.** `lww()`, `fww()`, `all()` operate on `[Head]` slices which no longer exist. LWW resolution is inlined in `KVTable::apply()` as an HLC comparison. FWW / multi-value can be added later as apply-time strategies if needed.
- [x] **Update `architecture.md`.** State Machines section updated to reflect `KVTable` engine, write-time LWW resolution, and slim storage format (materialized value + intention hash pointers).
- [x] **`ScopedDag` wrapper.** `SystemLayer` wraps `&dyn DagQueries` before passing to each side: `AppData` scope unwraps inner app bytes, `System` scope unwraps `SystemOp` bytes. Ensures `op.info.payload` and `dag.get_intention().payload` are in the same coordinate system for both app state machines and the system table.

### 14F: Conflict Surfacing
- [ ] **Conflict detection on read.** `get(key)` returns the materialized value. If `heads.len() > 1`, flag the response as conflicted. Cheap — no DAG query needed.
- [ ] **Conflict detail query.** Client can dereference the head hashes into the DAG to get full intention metadata: payloads (the conflicting values), authors, timestamps, causal deps. This is the slow path, only used for HITL or debugging.
- [ ] **Branch inspection.** Given head hashes, the kernel provides LCA (fork point), paths from fork to each head, branch metadata. Uses 14A primitives.

---

## Milestone 15: Review
- [ ] Address all items in Technical Debt
- [ ] Address all items in Discussion
- [ ] Address all items in Future
- [ ] Remove all sleep calls if possible
- [ ] Review code base
- [ ] **Fix `complete_join` store type hardcode** and drop `lattice-kvstore` dev-dependency from `lattice-net-iroh` and (where possible) `lattice-net`
- [ ] **Refactor `SyncSession::run` termination** (`lattice-net/src/network/sync_session.rs`): Bidirectional sync is broken — initiator terminates before handling responder's `FetchIntentions` when `reconcile_load == 0 && active_fetches == 0`. Root cause: `Done` (from `Reconciler`) is sent before `FetchIntentions` on the wire, and the peer uses `reconcile_load == 0` as exit condition. Needs a proper termination protocol — either reorder messages so `Done` follows `FetchIntentions`, add a session-level "finished" signal, or use half-close. Affects `test_channel_bidirectional_sync` and `test_bidirectional_sync`.
- [ ] **Fix `SyncSession` silent ingest error swallowing**: `IntentionResponse` handler (line ~155) silently ignores `ingest_intention` failures via `.is_ok()`. Broken chain errors (`store_prev` pointing to unapplied intention) produce `FATAL: State divergence` log but the error is dropped. Needs: (a) propagate or log the error, (b) investigate out-of-order intention delivery causing chain breaks.
- [ ] **Fix flaky `test_interleaved_modifications`** (`lattice-net/tests/sync_stress.rs`): intermittent failure

---

## Milestone 16: Log Lifecycle & Pruning

Manage log growth on long-running nodes via stability frontier, snapshots, pruning, and finality checkpoints.

> **See:** [Stability Frontier](stability-frontier.md) for the full design.

### 16A: Stability Frontier & Tip Attestations
- [ ] `SystemOp::TipAttestation { tips: Vec<(PubKey, Hash)> }` — publish changed author tips as SystemOps
- [ ] Attestation keys in `TABLE_SYSTEM`: `attestation/{attester}/{author} → Hash` (LWW by HLC)
- [ ] Derive per-author frontier: `frontier[A] = min(tip[A] across all Active peers)`
- [ ] Attestation triggers: post-sync, batch threshold, periodic heartbeat (~15 min), graceful shutdown
- [ ] **Lag metric:** Per-peer divergence from local tips, surfaced via `NodeEvent::PeerLagWarning` and `store status`

### 16B: Snapshotting
- [ ] **Frontier snapshot via replay:** The frontier is behind the current state. Generating a snapshot at the frontier requires replaying intentions from the last snapshot up to the frontier cut, producing the correct state at that point. Options: tempfile-based replay (works today, heavy on I/O) or in-memory `StateBackend` variant (cleaner, requires refactoring `StateLogic`/`PersistentState`).
- [ ] Store snapshots in `snapshot.db` (per-store, includes `TABLE_SYSTEM` attestation state)
- [ ] Bootstrap new peers from snapshot + tail sync instead of full log replay
- [ ] **Future optimization:** Inverse operations stored in `WitnessRecord` could allow rolling back from current state to frontier in O(head − frontier), avoiding replay. Deferred until profiling shows replay is a bottleneck.

### 16C: Pruning
- [ ] `truncate_prefix` for all intentions per author up to `frontier[A]`
- [ ] Discard individual attestation intentions below the frontier (snapshot carries state forward)
- [ ] Preserve intentions newer than frontier

### 16D: Checkpointing / Finality
- [ ] Periodically finalize state hash (protect against "Deep History Attacks")
- [ ] Signed checkpoint intentions in sigchain
- [ ] Nodes reject intentions that contradict finalized checkpoints

### 16E: Recursive Store Bootstrapping (Pruning-Aware)
- [ ] `RecursiveWatcher` identifies child stores from live state, not just intention logs.
- [ ] Bootstrapping a child store requires a **Two-Phase Protocol** because Negentropy cannot sync from an implicit zero-genesis if the store has been pruned.
- [ ] **Phase 1 (Snapshot):** Request the opaque snapshot (`state.db`) from the peer for the discovered child store.
- [ ] **Phase 2 (Tail Sync):** Run Negentropy to sync floating intentions that occurred *after* the snapshot's causal frontier.
- [ ] **Replace Polling with Notify:** `register_store_by_id` and boot sync use `sleep()` polling loops. Replace with `tokio::sync::Notify` or channel-based signaling.

### 16F: Hash Index Optimization ✅
- [x] Replace in-memory `HashSet<Hash>` with on-disk index (`TABLE_WITNESS_INDEX` in redb)
- [x] Support 100M+ intentions without excessive RAM

### 16G: Advanced Sync Optimization (Future)
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

## Milestone 17: Content-Addressable Store (CAS)

Node-local content-addressable blob storage. Replication policy managed separately. Requires M11 and M12.

### 17A: Low-Level Storage (`lattice-cas`)
- [ ] `CasBackend` trait interface (`fs`, `block`, `s3`)
- [ ] Isolation: Mandatory `store_id` for all ops
- [ ] `redb` metadata index: ARC (Adaptive Replacement Cache), RefCounting
- [ ] `FsBackend`: Sharded local disk blob storage

### 17B: Replication & Safety (`CasManager`)
- [ ] `ReplicationPolicy` trait: `crush_map()` and `replication_factor()` from System Table
- [ ] `StateMachine::referenced_blobs()`: Pinning via State declarations
- [ ] Pull-based reconciler: `ensure(cid)`, `calculate_duties()`, `gc()` with Soft Handoff

### 17C: Wasm & FUSE Integration
- [ ] **Wasm**: Host Handles (avoid linear memory copy)
- [ ] **FUSE**: `get_range` (random access) and `put_batch` (buffered write)
- [ ] **Encryption**: Store-side encryption (client responsibility)

### 17D: CLI & Observability
- [ ] `cas put`, `cas get`, `cas pin`, `cas status`

---

## Milestone 18: Lattice File Sync MVP

File sync over Lattice. Requires M11 (Sync) and M15 (CAS).

### 18A: Filesystem Logic
- [ ] Define `DirEntry` schema: `{ name, mode, cid, modified_at }` in KV Store
- [ ] Map file operations (`write`, `mkdir`, `rename`) to KV Ops

### 18B: FUSE Interface
- [ ] Write `lattice-fs` using the `fuser` crate
- [ ] Mount the Store as a folder on Linux/macOS
- [ ] **Demo:** `cp photo.jpg ~/lattice/` → Syncs to second node

---

## Milestone 19: Wasm Runtime

Replace hardcoded state machines with dynamic Wasm modules.

### 19A: Wasm Integration
- [ ] Integrate `wasmtime` into the Kernel
- [ ] Define minimal Host ABI: `kv_get`, `kv_set`, `log_append`, `get_head`
- [ ] Replace hardcoded `KvStore::apply()` with `WasmRuntime::call_apply()`
- [ ] Map Host Functions to `StorageBackend` calls (M9 prerequisite)

### 19B: Data Structures & Verification
- [ ] Finalize `Intention`, `SignedIntention`, `Hash`, `PubKey` structs for Wasm boundary
- [ ] Wasm-side Intention DAG verification (optional, for paranoid clients)
- **Deliverable:** A "Counter" Wasm module that increments a value when it receives an Op

---

## Milestone 20: N-Node Simulator

Scriptable simulation framework for testing Lattice networking at scale. Built on the `lattice-net-sim` crate (M12B).

- [ ] **Simulator library** (`Simulator` API): Scriptable — `add_node`, `take_offline`, `bring_online`, `join_store`, `put`, `sync`, `assert_converged`
- [ ] **Rhai scripting**: Embed Rhai for scenario scripts (loops, conditionals, dynamic topology changes)
- [ ] **Standalone binary**: CLI that loads and runs `.rhai` scenario files
- [ ] **Fix flaky `test_large_dataset_sync`:** Intermittent partial sync failures (misses items). Likely race between auto_sync boot task and explicit `sync_all_by_id`. Currently mitigated by disabling auto_sync in test.
- [ ] **Gate:** 20+ node convergence simulation with metrics: sync calls, items transferred, convergence %, wall-clock time

---

## Milestone 21: Embedded Proof ("Lattice Nano")

Run the kernel on the RP2350.

> Because CLI is already separated from Daemon (M7) and storage is abstracted (M9), only the Daemon needs porting.
> **Note:** Requires substantial refactoring of `lattice-kernel` to support `no_std`.

### 21A: `no_std` Refactoring
- [ ] Split `lattice-kernel` into `core` (logic) and `std` (IO)
- [ ] Replace `wasmtime` (JIT) with `wasmi` (Interpreter) for embedded target
- [ ] Port storage layer to `sequential-storage` (Flash) via `StorageBackend`

### 21B: Hardware Demo
- [ ] Build physical USB stick prototype
- [ ] Implement BLE/Serial transport
- [ ] Sync a file from Laptop → Stick → Phone without Internet

---

## Technical Debt

- [ ] **REGRESSION**: history command list filtering (backend side) capability
- [ ] **REGRESSION**: Graceful reconnect after sleep/wake (may fix gossip regression)
- [ ] **Store Name Lookup Optimization**: `find_store_name()` in `store_service.rs` and `backend_inprocess.rs` does O(meshes × stores) linear search. Store names live in mesh root KV stores (StoreDeclaration). Consider caching in StoreManager or adding index.
- [ ] **Data Directory Lock File**: Investigate lock file mechanism to prevent multiple processes from using the same data directory simultaneously (daemon + embedded app conflict). Options: flock, PID file, or socket-based detection.
- [ ] **Denial of Service (DoS) via Gossip**: Implement rate limiting in GossipManager and drop messages from peers who send invalid data repeatedly.
- [ ] **Payload Validation Strategy**: Decide where semantic validation occurs and what happens on failure. Options: build-time only, versioned rules, intention replacement, or separate chain/payload advancement. See `test_rejected_entry_breaks_chain` in `lattice-kvstore/src/kv.rs`.
- [ ] **Signer Trait**: Introduce a `Signer` trait (sign hash → signature) to avoid passing raw `SigningKey` through the stack. Affects intention creation (`SignedIntention::sign`), witness signing (`WitnessRecord::sign`), and the M11 migration path.
- [ ] **Optimize `derive_table_fingerprint`**: Currently recalculates the table fingerprint from scratch. For large datasets, this should be optimized to use incremental updates or caching to avoid O(N) recalculation.
- [ ] **DAG Reachability Index**: `DagQueries` methods (`find_lca`, `is_ancestor`, `get_path`) use naive BFS. For large DAGs, add generation numbers (prune impossible ancestors by depth) or bloom filters (compact ancestor summaries) for O(log N) reachability. Not needed until BFS becomes a bottleneck.
- [ ] **Sync Trigger & Bootstrap Controller Review**: Review how and when sync is triggered (currently ad-hoc in `active_peer_ids` or `complete_join_handshake`). Consider introducing a dedicated `BootstrapController` to manage initial sync state, retry logic, and transition to steady-state gossip/sync.
- [ ] Unify `IntentionInfo` and `Op` into a single type in StateMachine::apply().
- [ ] **Clean up**: Remove map_err wherever possible.
- [ ] **`complete_join` hardcodes `STORE_TYPE_KVSTORE`**: `Node::complete_join()` calls `store_manager.open(store_id, STORE_TYPE_KVSTORE)` instead of using the actual store type. The store type should be communicated through the join protocol (invite token or `JoinResponse`). Blocks dropping `lattice-kvstore` as a dev-dependency from crates that only need NullState for testing. Affects `lattice-net-iroh` and `lattice-net` tests.

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
- **Shamir Key Splitting**: Split recovery seed into N shares, require K to reconstruct (e.g. 3 shares, any 2 recover). Shares printed as QR codes on labeled cards. For high-value / paranoid use cases.
- **Vault-Node Assisted Recovery**: Physical recovery button or PIN sequence puts vault into pairing mode. Two-factor: physical access + seed/recovery PIN. Optional e-ink challenge-response display.
- **Vault-Node Autonomous Updates**: Authenticated software update mechanism with rollback for vault-nodes when owner is unreachable. Authenticated against root identity.
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
- **USB Gadget Node**: Hardware device (Pi Zero, RISC-V dongle) that enumerates as a USB Ethernet gadget and runs a Lattice node, peering with the host over the virtual interface.
- **Blind Ops / Cryptographic Revealing** (research): Encrypted intention payloads revealed only to nodes possessing specific keys (Convergent Encryption or ZK proofs). Enables selective disclosure within atomic transactions.
- **Epoch Key Encryption**: Shared symmetric key for O(1) gossip payload encryption, enabling efficient peer revocation. See [Epoch Key Encryption](design/epoch-key-encryption/) for the full design.
- **S-Expression Intention View**: Enhance `store history` and `store debug` to optionally display Intentions as S-expressions, providing a structured, verifiable view of the underlying data for debugging.
- **Async/Task-Based Bootstrapping**:
    - Treat bootstrapping as a persistent state/task (`StoreState::Bootstrapping`).
    - Retry indefinitely if peers are unavailable.
    - Recover from "not yet bootstrapped" state on restart.
    - Inviter sends list of potential bootstrap peers in `JoinResponse`.
    - Node can bootstrap from any peer in the list, not just the inviter.
- **Blind Node Relays**: Untrusted VPS relays that sync the raw Intention DAG via Negentropy. No store keys, no Wasm. Can perform graph-based pruning using the `state_independent` flag: prune linear sub-chains below state-independent intentions at the frontier (pure graph operation, no state machine). Full nodes can also push computed snapshots to relays, making them full bootstrap sources. Two-tier pruning: relays do structural pruning (safe by construction), full nodes do semantic pruning (more compact).
