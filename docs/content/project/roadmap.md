---
title: "Roadmap"
weight: 1
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
| **M17**          | Housekeeping: (A) test infrastructure unification — consolidated `test_node_builder`, `TestHarness`, `TestStore`, `TestCtx`, `MockProvider`, helper functions into `lattice-mockkernel`; replaced 6 mock state machines with `NullState` + `TrackingState`; (B) test scope review — replaced `KvState` with `NullState` in non-KV tests, coverage audit; (C) proto definition audit; (D) crate dependency graph review — removed dead deps and overly broad re-exports |

---

## Milestone 18: Store & Log Lifecycle

Store/mesh lifecycle operations and log growth management. Two themes: (1) how users create, join, leave, and delete stores/meshes; (2) how the system manages unbounded log growth via epochs, pruning, and finality.

> **See:** [Connected DAG & Genesis Commitment](../design/connected-dag) for the epoch/pruning design.
> **See also:** `docs/content/design/revocation-*.dot` for DAG diagrams (render with `dot -Tpdf`).

### Open Questions

These require design decisions before the relevant sub-milestones can be implemented.

**1. Concurrent epoch creation.**
If two nodes simultaneously author epoch-triggering system ops (e.g., A revokes C while B revokes D at the same time), both actors create epoch N with the same seq but different hashes, different frontiers, and different `required_acks`. The current design has no resolution. Options:
- **Reject second epoch** — the first epoch (by hash ordering or HLC) wins. The second node's epoch is dropped. But the second revocation happened — the second node must create epoch N+1 instead.
- **Both valid** — allow multiple epochs at the same seq. Peers ack all of them. Settlement requires all `required_acks` across all epoch N variants. Complex tracking.
- **Epoch seq from DAG** — the epoch's seq is derived from the number of settled epochs reachable from genesis. Two concurrent epochs would naturally diverge into two branches; whichever gets more acks first settles. The other becomes a dead branch.

**2. Child store epochs on parent revocation.**
Child stores with `PeerStrategy::Inherited` share the parent's `PeerManager` (same `Arc`). Gossip filtering (`can_accept_gossip`) therefore already reflects the parent's revocation immediately — no separate peer governance needed in the child.

However, the child still needs its own epoch intention in its own DAG for: pruning boundary, snapshot computation, gossip topic rotation, and negentropy sync scoping. The child's epoch must cite the child's own genesis and author tips, with `required_acks` from the shared `PeerManager`.

**Proposed mechanism:** After the parent's revocation creates epoch N on the parent's actor, `StoreManager` propagates the epoch to inherited children. For each open child store with `PeerStrategy::Inherited`, it submits a kernel-level `EpochOp` directly to the child's actor (not via the SSM — no system batch needed). The child's epoch cites the child's genesis + child's current author tips. `required_acks` is read from the shared `PeerManager`.

Open sub-questions:
- Does propagation happen synchronously in the same `StoreManager` call, or as a follow-up task?
- What if a child store is closed/archived at revocation time? The epoch must be created when the child is re-opened and the node detects its peer set has drifted from the parent's current epoch.

**3. Offline node returns after multiple epochs.**
A node offline during epoch 1 and epoch 2 receives both on reconnect. It must ack both. If it acks epoch 2 (which transitively cites epoch 1 via the DAG), does that satisfy the epoch 1 `required_acks` check? The answer depends on whether the settlement check is "B's tip cites epoch N directly" or "transitively." If transitive, one ack for epoch 2 satisfies both — simpler. Needs to be made explicit.

### 18-Lifecycle: Store & Mesh Lifecycle
Complete the store/mesh lifecycle operations. Currently only create, join, and archive-child exist.

**Join & Leave:**
- [x] Join a mesh via invite token (`store join <token>`)
- [ ] Leave a mesh — stop replicating a root store, remove from `meta.db` rootstores table, close all children, stop watchers. Data kept on disk for possible re-join.
- [ ] Leave confirmation — warn if node is the last peer (data only exists locally)
- [ ] Re-join — re-open a previously left root store from local data, re-register, resume replication

**Add & Remove Stores:**
- [x] Create child store under a parent (`store create --parent <uuid>`)
- [x] Archive child store (`store delete` — sets status to Archived in parent system table)
- [ ] Unarchive child store — restore an archived child to Active status
- [ ] Permanent local delete — remove store data from disk (state.db, intentions/). Only after leaving or archiving. Requires confirmation.
- [ ] Remove root store — leave mesh + optionally delete local data. Two-step: leave first, then delete.

**Peer Management:**
- [x] Invite a peer (`store invite`)
- [x] Revoke a peer (`store revoke-peer`)
- [x] Gossip-level revocation enforcement — revoked peers' gossip broadcasts rejected (`can_accept_gossip`). Negentropy sync and bootstrap unaffected (historical intentions from revoked peers still transfer). Renamed `can_accept_entry` → `can_accept_gossip`, `list_acceptable_authors` → `gossip_authorized_authors`.
- [x] Genesis intention — `GenesisOp` as first intention in every store. Synthetic genesis migration for pre-genesis stores. `genesis::build_synthetic_genesis()` produces deterministic genesis from store UUID.
- [ ] Epoch-level revocation enforcement — epoch frontier defines cutoff for revoked authors' data. SSM signals `EpochRequired { required_acks }`, kernel builds `EpochOp` citing genesis + all author tips. Gossip topic rotates per epoch (`content_hash("lattice/{store_id}/{epoch_hash}")`). Negentropy sync scoped to epoch boundary for revoked peers (they receive the epoch but nothing after). Bootstrap refused for revoked peers. See connected-dag.md Part 3.
- [ ] Self-revoke — a peer removes itself from the peer list (graceful departure)

### 18A: Witness-Only Sync ✅
Negentropy set reconciliation now operates on witnessed intentions only. Floating (unwitnessed) intentions are excluded from fingerprints and range queries.
- [x] Move Negentropy fingerprint accumulation from `insert()` to `witness()` — `witness_fingerprint` tracks witnessed intentions only
- [x] Range queries (`fingerprint_range`, `count_range`, `hashes_in_range`) scan `TABLE_WITNESS_INDEX` instead of `TABLE_INTENTIONS`
- [x] Verify convergence under network partitions, reordering, and bootstrap
- [x] Floating intentions excluded from sync — received-but-unwitnessable intentions no longer pollute fingerprints
- [x] Renamed `table_fingerprint` → `witness_fingerprint` across codebase

### 18B: Meta Table Separation
Prerequisite for epoch indexes and headless replication. Separate store metadata into two tables reflecting the `log.db` / `state.db` split. `log.db` is the durable backbone that always exists; `state.db` is optional (only present when an opener is available and projection is active).

Both databases retain `store_id` and `store_type` for independent identity verification — if files are moved or corrupted, each database rejects mismatches on open. `log.db` is the authoritative source (used by `peek_info()` to decide which opener to use); `state.db` uses them as a consistency check.

**Rename `TABLE_META` → `TABLE_STATE_META`** (stays in `state.db`):
- `store_id` — identity verification on open (stays, consistency check)
- `store_type` — opener match verification (stays, consistency check)
- `schema_version` — state machine schema version
- `tip/{author}` — chain integrity checking during projection writes
- `last_applied_witness` — projection cursor

**New `TABLE_LOG_META`** (in `log.db`, alongside `TABLE_INTENTIONS`, `TABLE_WITNESS`, etc.):
- [ ] `store_id` — store identity, authoritative source
- [ ] `store_type` — store type string, authoritative source
- [ ] `current_epoch` — local epoch cursor (new, for 18C)
- [ ] `epoch_fingerprint/{epoch}` — per-epoch fingerprint (new, for 18C)
- [ ] Update `StateBackend::peek_info()` to read from `log.db` instead of `state.db`
- [ ] Migration: on startup, populate `TABLE_LOG_META` from `TABLE_META` if `log.db` lacks it

This separation enables headless replication: a node can participate in sync and witnessing for a store it doesn't have an opener for. `log.db` + `TABLE_LOG_META` is self-sufficient for the replication layer. `state.db` + `TABLE_STATE_META` is only needed for projection.

### 18B½: Self-Contained State (Pruning Prerequisite)
The state machine's materialized state must be fully self-contained — no DAG lookups during conflict resolution or value reads. This is a hard prerequisite for pruning: once intentions are deleted from `log.db`, any code that reaches back into the DAG to resolve state breaks.

Currently `KvTable::apply_head()` calls `dag.get_intention()` to fetch the current winner's `(timestamp, author)` for LWW tiebreaking. After pruning, that intention may be gone.

**Fix:** Store `(hash, timestamp, author)` per head in the `Value` proto rather than bare hashes. Conflict resolution becomes self-contained.

- [ ] Update `lattice-kvtable/proto/value.proto`: replace `repeated bytes heads` with `repeated HeadEntry head_entries` where `HeadEntry { hash, HLC hlc, author, value, tombstone }`. Define `HLC { wall_time, counter }` locally in `value.proto` (mirrors `storage.proto`). Each entry is a fully self-contained CRDT branch — hash, LWW metadata, and the value the branch wrote. Keep old `heads` field readable for migration.
- [ ] Update `KvTable::apply_head()`: use stored `(wall_time, counter, author)` from `head_entries[0]` for LWW comparison instead of `dag.get_intention()`. Remove `dag: &dyn DagQueries` parameter.
- [ ] Update all callers of `apply_head()` (SystemTable, KvState, rootstore state machine) to drop the `dag` argument.
- [ ] Update `decode_heads()`, `decode_inspect()`, `decode_lww_with_conflict()` to read from `head_entries`.
- [ ] Remove `DagQueries` import from `lattice-kvtable`.

**Note:** This also applies to `SystemTable` — any system key with concurrent writes (e.g., concurrent `SetPeerStatus`) resolves via the same LWW mechanism. Same fix applies.

**Migration:** Existing `state.db` values are in the old bare-hash format. Full replay is required: bump `KEY_SCHEMA_VERSION` in `TABLE_META`, detect the old version on startup, delete `state.db`, let the projection loop rebuild from `log.db` with the new format. All values written during replay will be in the new `head_entries` format. This reuses the existing state.db recovery mechanism (`test_state_db_recovery` already validates this path).

### 18C: Epoch Intentions
Epoch intentions bridge the system and data partitions, enable revocation enforcement at the projection layer, and define pruning boundaries. See connected-dag.md Part 3 for the full design.

**Kernel-level ops (UniversalOp variants):**
- [ ] `EpochOp { seq, required_acks }` — emitted after genesis (epoch 0) and after membership-critical system changes. `causal_deps` cite genesis + all author tips (complete DAG frontier). `required_acks` is the SSM-provided subset of peers that must ack for settlement.
- [ ] `EpochAckOp { epoch_seq }` — submitted by each peer in `required_acks` on receiving a new epoch. `causal_deps` cite the epoch hash + peer's current tip. Epoch creator has implicit ack (no separate op needed).

**Epoch creation flow:**
- [ ] SSM returns `SystemEvent::EpochRequired { required_acks }` when processing membership-critical ops (revocation, key rotation, prune cut)
- [ ] Kernel receives event, reads author tips, submits `EpochOp` with `causal_deps: [genesis, ...all_author_tips]`
- [ ] Epoch 0 emitted automatically: actor detects `EpochRequired` from SSM during projection of the initial system batch (`add_peer(self)` triggers "peers exist, no epoch"). `StoreManager::create()` does not need an explicit epoch 0 step.
- [ ] Auto-submit `EpochAckOp` when a node receives a new epoch it didn't create

**Settlement and pruning:**
- [ ] Kernel tracks settlement: for each peer in `required_acks`, check if their author tip transitively cites the epoch hash (`is_ancestor` query on DAG)
- [ ] Epoch is settled when all `required_acks` peers have acked
- [ ] Once settled, pre-epoch history is safe to prune (no peer will request it)
- [ ] Orphaned branches from revoked peers are garbage collected

**Projection filter:**
- [ ] Kernel queries SSM via `ProjectionFilter` trait: "is this author revoked?"
- [ ] Intentions from revoked authors beyond the epoch frontier are excluded from materialized state
- [ ] Scoped re-projection on epoch arrival: only revoked peer's data re-evaluated

### 18D: Pruning Execution
Physical deletion of intentions below a settled epoch. The coordination protocol (epoch + epoch ack) is in 18C. This milestone covers the actual deletion and sync implications.
- [ ] **Deletion walk:** Once an epoch is settled, walk intentions reachable from the epoch's `causal_deps`. Everything NOT reachable and below the epoch's frontier is deleted from `log.db` (`TABLE_INTENTIONS`, `TABLE_WITNESS`, `TABLE_WITNESS_INDEX`).
- [ ] **Genesis and frontier tips survive:** Genesis is always reachable (epoch cites it). The epoch's immediate deps (author tips at epoch creation) survive as the frontier. The sparse spine is: `genesis ← epoch N ← acks + tail`.
- [ ] **Fingerprint adjustment:** Witness fingerprint must exclude pruned intentions. Either recompute from remaining entries or maintain a cumulative "pruned fingerprint" to subtract.
- [ ] **Sync handshake:** Nodes exchange current epoch seq. Negentropy runs over post-epoch intentions only. A node behind on epochs fast-forwards via the epoch intention (guaranteed receivable — `causal_deps` ensure dependencies are present).
- [ ] **Stale peer return:** Node behind on epochs that connects to a node that has already pruned needs epoch bootstrap — receive the epoch + frontier tips + tail (→ M19D).
- [ ] **Lag metric:** Per-peer epoch lag, surfaced via `NodeEvent::PeerLagWarning` and `store status`.

### 18E: Epoch-Aware Bootstrap Snapshots
Materialized state snapshots for efficient bootstrap of new peers joining a pruned store. Distinct from epoch intentions (which are DAG structural checkpoints) — these carry the projected state.
- [ ] **State at epoch frontier:** The epoch defines the frontier. State at the frontier is deterministic: project all intentions reachable from the epoch's `causal_deps`. For a freshly-settled epoch, this is the current state minus any post-epoch writes.
- [ ] **Snapshot format:** Serialize `TABLE_DATA` + `TABLE_SYSTEM` state at the epoch frontier. Store in `snapshot.db` (per-store) or as a CAS blob (→ M20).
- [ ] **Bootstrap from snapshot:** New peer receives genesis + epoch + snapshot + tail. Applies snapshot as initial state, then projects the tail. No full log replay needed.
- [ ] **Note:** `StateMachine::snapshot()`/`restore()` were removed in M16A Step 3. New mechanism needed, tailored to epoch-aware projection.
- [ ] **Future optimization:** Inverse operations stored in `WitnessRecord` could allow rolling back from current state to frontier in O(head − frontier), avoiding replay. Deferred until profiling shows replay is a bottleneck.

### 18E½: Store-Level Pruning Hints
Store-defined intention metadata that guides consensus pruning. Both hints are advisory — pruning only happens once all peers have attested past the relevant intentions (standard frontier rules).
- [ ] **Supersede hint** (`supersedes_all: true`): Intention declares it supersedes all prior state (e.g., a full snapshot-as-intention, or a state reset). Everything causally before it is safe to prune. The pruner can treat it as a new root, discarding the entire prefix. Useful for stores that periodically compact their state into a single intention.
- [ ] **Ephemeral hint** (`ephemeral: true`): Intention declares it is unlikely to ever be a causal dependency (e.g., a key deletion in a KV store — no future operation will depend on the deleted value). Chains ending at ephemeral intentions may be pruned more aggressively — once all peers have witnessed them past the frontier, the pruner can discard them without waiting for a full snapshot cycle.
- [ ] Wire hints through `Intention` proto field (bitflags or enum), surface in `Op` for state machines, respect in `truncate_prefix` (18D).

### 18F: Checkpointing / Finality
- [ ] Periodically finalize state hash (protect against "Deep History Attacks")
- [ ] Signed checkpoint intentions in sigchain
- [ ] Nodes reject intentions that contradict finalized checkpoints

### 18G: Hash Index Optimization ✅
- [x] Replace in-memory `HashSet<Hash>` with on-disk index (`TABLE_WITNESS_INDEX` in redb)
- [x] Support 100M+ intentions without excessive RAM

### 18H: Advanced Sync Optimization (Future)
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

## App Hosting

Serve web applications at subdomains, backed by Lattice stores. Independent of milestone numbering — can be worked in parallel with M18/M19.

> **See:** [App Hosting Design](../design/app-hosting) for the full design.

### A: Node-Local App Bindings ✅
`AppBinding` struct, `MetaStore` CRUD, `LatticeBackend` trait methods, REST API (`/api/apps`), subdomain routing, embedded app bundles, app shell HTML with SDK bootstrap.
- [x] `AppBinding` model in `lattice-model`
- [x] `app_bindings` table in `meta.db` (redb) with CRUD operations
- [x] `LatticeBackend` trait: `app_set_binding`, `app_remove_binding`, `app_get_binding`, `app_list_bindings`
- [x] `InProcessBackend` implementation (delegates to `MetaStore`)
- [x] REST API: `GET/POST/DELETE /api/apps/{subdomain}`
- [x] Subdomain extraction from `Host` header, app shell serving
- [x] Embedded inventory app bundle via `rust-embed`
- [x] Lattice SDK reads `<meta>` tags and connects to store via WebSocket

### A½: App Hosting Hardening ✅
Security, architecture, and web UI improvements.
- [x] **Bundle manifest storage**: Manifest fields stored as individual rootstore keys (`appbundles/{app_id}/manifest/{section}/{key}`), generic `BundleManifest` proto (repeated sections/entries), `ListBundles` returns manifest metadata
- [x] **Shared manifest parsing**: `lattice-rootstore::manifest::parse_bundle_manifest()` used by both rootstore and web server, eliminated duplicate TOML parsing
- [x] **Separated request/op protos**: `UploadBundleRequest` (client-facing, no manifest) vs `UploadBundleOp` (internal intention, includes manifest)
- [x] **Zip bomb protection**: `Read::take()` caps actual decompression, manifest capped at 64 KB, bundle capped at 64 MB at command handler level
- [x] **Subdomain extraction**: Works with arbitrary hostname depth (not just 2-3 labels)
- [x] **Broadcast resilience**: All `broadcast::Receiver` loops handle `Lagged` explicitly
- [x] **O(1) broadcast clones**: `AppEvent::BundleUpdated` uses `bytes::Bytes`
- [x] **Non-blocking zip parsing**: `spawn_blocking` for bundle extraction
- [x] **TOCTOU fix**: `set_enabled` serialized via `tokio::sync::Mutex`
- [x] **KvRead trait**: Shared read interface for `KVTable` and `ReadOnlyKVTable`
- [x] **Deduplicated app manager**: `collect_store_events()`, `watch_stream()` generic helper
- [x] **`WebServer` takes `Arc<AppManager>`**: Calls `attach()` itself, no temporal coupling
- [x] **Error pages**: `error.html` template with status code, message, back-link to management UI
- [x] **App registration creates child store**: UI creates store matching manifest `store_type` before registering app

### A⅔: Web UI Modernization ✅
ES modules, path-based routing, event-driven state, dashboard.
- [x] **ES modules**: All JS files use `import`/`export`, vendor UMD shims (`preact.mjs`, `sdk.js`), zero globals in application code
- [x] **Path-based routing**: `/`, `/store/{uuid}`, `/store/{uuid}/{tab}`, `/store/{uuid}/debug/{view}`, SPA fallback on server
- [x] **Clean components**: Navigation state passed via props from `App` root, not read from globals
- [x] **Event-driven state**: Node event stream + root store `watch-apps` streams update signals, targeted `refreshStores()`/`refreshApps()`/`refreshNodeStatus()` after mutations, `refresh()` and `refreshCounter` removed
- [x] **Dashboard**: Apps with status/actions, meshes with peer counts, editable node name, copyable node ID
- [x] **JS unit tests**: `boa_engine` runs `.test.js` files in `cargo test` — 33 tests for router and helpers, no Node.js/npm needed
- [x] **CSS cleanup**: Single `--hue` knob, consolidated badge styles, removed Inter font

### B: Store Claiming via SystemOp
Tag stores with an `app-id` so apps can discover which stores belong to them. A store can be claimed by one app type; multiple stores can share the same `app-id`.
- [ ] `SystemOp::SetAppId(String)` — writes `app-id` key to system table (LWW)
- [ ] `SystemOp::ClearAppId` — removes the `app-id` key
- [ ] `SystemState` projection: apply/read `app-id` from system table
- [ ] `get_app_id()` / `set_app_id()` on `SystemStore` trait
- [ ] CLI: `store claim <store-id> <app-id>`, `store unclaim <store-id>`
- [ ] Management UI: claim/unclaim in store detail view

### C: Claim-Aware Discovery
Apps discover their stores by filtering on `app-id` claims.
- [ ] Backend method: `store_list_by_app_id(app_id: &str) -> Vec<StoreRef>` — filters stores with matching `app-id` in system table
- [ ] REST endpoint: `GET /api/stores?app_id=inventory`
- [ ] Management UI: "Register App" modal shows only stores claimed by the selected app type

### D: Auto-Provisioning
One-step app registration: create store, claim it, bind subdomain.
- [ ] `POST /api/apps/{subdomain}` with `{ app_type }` (no `store_id`) — creates a child store (type from manifest), claims it, binds the subdomain
- [ ] If `store_id` is provided, skip creation and just claim + bind
- [ ] Unregister optionally unclaims the store

### E: SDK List Response Convention
The SDK must not auto-unwrap repeated fields from protobuf responses. Implicit unwrapping breaks the moment a response gains pagination, metadata, or a second repeated field.
- [ ] Define `ListResponse` protos with an explicit array field (e.g., `repeated Entry items = 1;`) — no bare repeated-field-as-root
- [ ] SDK returns decoded protobuf objects as-is — no magic field detection or unwrapping
- [ ] Frontend reads the schema-defined field explicitly: `const data = (await store.List(...)).items || [];`
- [ ] Audit existing store protos (`KvStore`, any app-level schemas) for bare repeated responses and migrate to named wrapper fields

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
- [x] **REGRESSION**: `node set-name` created duplicate intentions. Root cause: child stores share parent's PeerManager, but `set_name` iterated all store IDs. Fixed: deduplicate by `Arc::as_ptr` on PeerManager.
- [ ] **Denial of Service (DoS) via Gossip**: Implement rate limiting in GossipManager and drop messages from peers who send invalid data repeatedly.
- [ ] **Optimize `derive_witness_fingerprint`**: Currently recalculates the witness fingerprint by scanning `TABLE_WITNESS_INDEX` from scratch on startup. For large datasets, this should be optimized to use incremental updates or caching to avoid O(N) recalculation.
- [ ] **DAG Reachability Index**: `DagQueries` methods (`find_lca`, `is_ancestor`, `get_path`) use naive BFS. For large DAGs, add generation numbers (prune impossible ancestors by depth) or bloom filters (compact ancestor summaries) for O(log N) reachability. Not needed until BFS becomes a bottleneck.
- [ ] **Swift bindings: propagate `MethodKind`**: `store_inspect()` in `lattice-bindings` builds `MethodInfo` from the proto `ServiceDescriptor` directly, bypassing `store_list_methods()`. Add a `kind` field to the bindings `MethodInfo` struct and populate it.
- [ ] **Expose field format hints via API**: `field_formats()` (Hex, Utf8) lives on `Introspectable` but has no RPC. GUIs render all `bytes` fields as raw hex without it. Add an RPC or bundle hints into the descriptor response. Drive by actual GUI needs.
- [ ] **Move `STORE_TYPE_KVSTORE` / `STORE_TYPE_LOGSTORE`** out of `lattice-model` into their respective store crates. `lattice-model` shouldn't know about specific store types.
- [ ] **Stale author chaintips in state.db**: `verify_and_update_tip` writes `tip/<pubkey>` entries but nothing ever removes them. Revoked authors leave dead records. Low priority (64 bytes per author), but snapshots include stale data.
- [ ] **Decouple Iroh transport key from lattice signing identity**: Iroh requires raw `SigningKey` bytes to derive its QUIC node identity (`lattice-net-iroh/src/transport.rs`), which is incompatible with HSM/TPM backends where the private key never leaves the hardware. Options: (1) derive a transport subkey from the HSM if it supports ECDH/key derivation, (2) give Iroh its own ephemeral key and add a post-connect attestation protocol to bind transport identity to lattice `PubKey`, (3) wait for Iroh to support trait-based signing. Option 2 also decouples transport identity from signing identity, which helps with key rotation and multi-device. Trade-off: nodes can no longer assume Iroh `NodeId` == lattice `PubKey`, so a mapping/attestation step is needed after connection.
- [ ] **Migrate meta.db from redb to SQLite**: `meta.db` is low-volume, read-heavy, and schema-evolving (stores, rootstores, app bindings, node config). redb's typed table definitions fight schema evolution — changing a key type requires manual migration code and can crash on startup. SQLite with versioned migrations (e.g., `rusqlite` + `refinery`) makes schema changes composable and inspectable. Keep redb for `log.db` and `state.db` where raw sequential write performance matters.

---

## Discussion

- [x] ~~Does StateMachine need snapshot/restore once we have IntentionStore pruning/snapshots?~~ **Resolved (M16A Step 3):** Removed from `StateMachine`. Not used in production — replication is witness-log-based. M18E redesigns snapshots for epoch-aware bootstrap.
- [x] ~~Revoking a Peer is untested. How do we ensure removed peers will not receive future Intentions?~~ **Partially resolved:** Gossip-level enforcement implemented (`can_accept_gossip` rejects revoked peers). Full enforcement via epoch intentions designed (connected-dag.md Part 3) — epoch frontier is cutoff for revoked authors' data, SSM signals `EpochRequired`, kernel builds epoch + tracks settlement. Implementation in 18C.

---

## Future

- TTL expiry for long-lived orphans (received_at timestamp now tracked)
- Mobile clients (iOS/Android)
- **Root Identity & Key Hierarchy**: Four-layer key model: Seed (24 words, offline) → Root Identity (Ed25519, signs authorizations only) → Device Keys (per-node, sign intentions) → DAG operations. Peers tracked by root identity in SystemStore; each root identity has an authorized device key list. New SystemOps: `DeviceAuthorize(root, device, sig)`, `DeviceRevoke(root, device)`. Device keys validated against authorization chain. Existing DAG/chain/gossip/sync works unchanged at the device-key level.
- **Seed-Based Disaster Recovery**: BIP-39 mnemonic (24 words) generated during onboarding. Recovery flow: enter seed on fresh device → regenerate identity → connect vault-node → revoke lost device keys → resume. Setup flow includes a recovery drill (verify backup words before setup is complete).
- Secure storage of node key (Keychain, TPM)
- **Salted Gossip ALPN**: Use `/config/salt` from root store to salt the gossip ALPN per mesh (improves privacy by isolating mesh traffic). Note: gossip topics already rotate per epoch for revocation isolation (`content_hash("lattice/{store_id}/{epoch_hash}")`). ALPN salting is an additional layer for mesh-level privacy.
- **HTTP API**: External access to stores via REST/gRPC-Web (design TBD based on store types)
- **Inter-Store Messaging** (research): Stores exchange typed messages using an actor model. Parent stores have a store-level identity (keypair derived from UUID + node key). Children trust the parent's store key — not individual parent authors — preserving encapsulation. Parent state machines decide what to relay; child state machines process messages as ops in their own DAG. Key design questions: message format, delivery guarantees (at-least-once via DAG?), bidirectional messaging (child→parent replies), and whether messages are durable (intentions) or ephemeral.
- **Audit Trail Enhancements** (HLC `wall_time` already in Intention):
  - Human-readable timestamps in CLI (`store history` shows ISO 8601)
  - Time-based query filters (`store history --from 2026-01-01 --to 2026-01-31`)
  - Identity mapping layer (PublicKey → User name/email)
  - Tamper-evident audit export (signed Merkle bundles for external auditors)
  - Optional: External audit sink (stream to S3/SIEM)
- **Blind Ops / Cryptographic Revealing** (research): Encrypted intention payloads revealed only to nodes possessing specific keys (Convergent Encryption or ZK proofs). Enables selective disclosure within atomic transactions.
- **Epoch Key Encryption**: Shared symmetric key for O(1) gossip payload encryption, enabling efficient peer revocation. See [Epoch Key Encryption](../design/epoch-key-encryption) for the full design.
- **Blind Node Relays**: Untrusted VPS relays that sync the raw Intention DAG via Negentropy. No store keys, no Wasm. Can perform graph-based pruning using the `state_independent` flag: prune linear sub-chains below state-independent intentions at the frontier (pure graph operation, no state machine). Full nodes can also push computed snapshots to relays, making them full bootstrap sources. Two-tier pruning: relays do structural pruning (safe by construction), full nodes do semantic pruning (more compact).
