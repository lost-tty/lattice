# Lattice Roadmap

## Completed

- **M1**: Single-node append-only log with SigChain, HLC, redb KV, interactive CLI
- **M1.5**: DAG conflict resolution with multi-head tracking and deterministic LWW
- **M1.9**: Async refactor with store actor pattern and tokio runtime
- **M2**: Two-node sync via Iroh (mDNS + DNS), peer management, bidirectional sync
- **Stability**: Gossip join reliability, bidirectional sync fix, diagnostics, orphan timestamps
- **Sync Reliability**: Common HLC in SyncState, auto-sync on discrepancy with deferred check
- **Store Refactor**: Directory reorganization (`sigchain/`, `impls/kv/`), `Patch`/`ReadContext` traits, `KvPatch` with `TableOps`, encapsulated `StoreHandle::open()`
- **Simplified StoreHandle**: Removed generic types and handler traits; KvState is now the only implementation with direct methods
- **Heads-Only API**: `get()`, `list()`, `watch()` return raw `Vec<Head>`. `Merge` trait provides `.lww()`, `.fww()`, `.all()`, `MergeList` for lists. See `docs/store-api.md`.
- **Async Mesh Join**: Non-blocking `mesh join` with `MeshReady` event feedback loop for responsive CLI experience.
- **CLI Mesh Context**: Refactored CLI to use event-driven `Mesh` context, decoupling commands from `Node` and ensuring direct `Mesh` usage.

---

## Milestone 3: Mesh API Refactor

**Goal:** Type-safe API for mesh management, multiple meshes per node, and improved join experience. See [architecture.md](architecture.md#mesh-api-facade-pattern-future).

### 3A: Mesh Wrapper Type
- [x] Create `Mesh` struct wrapping root `StoreHandle` + `PeerProvider`
- [x] Move peer commands from `Node` to `Mesh`

### 3B: Token-Based Join
- [x] Implement `InviteToken` (Base58 + Protobuf) containing `mesh_id`, `secret`, `inviter_pubkey`.
- [x] `mesh invite` generates token.
- [x] `mesh join <token>` extracts info and joins using embedded secret.
- [x] Token consumption via hash lookup in root store.
- [ ] **Known Risk:** Race condition in `consume_invite_secret` (check-then-delete). Low priority since single-node local check. Future: add Mutex or atomic delete.
- See [one-time-join-tokens.md](one-time-join-tokens.md).

### 3C: Node Registry Refactor
- [x] `Node::get_mesh()` → `Mesh` wrapper (Single-mesh MVP)
- [x] `Node::get_store()` → raw `StoreHandle`

### 3D: CLI Context Switching
- [x] `mesh create`, `mesh list`, `mesh use`
- [x] Deterministic default mesh selection (oldest by join time)

---

## Milestone 4: Replicated State Machine Platform

**Goal:** Transform lattice from a specific Key-Value store into a generic Replicated State Machine platform.

### 4A: Reliability & Consistency (Pre-Pivot Cleanup)

- [x] **Transactional Atomicity**: Ensure disk write (Log) and DB update (State) use WAL pattern. Prevents inconsistent states after crash.
- [ ] **Refactor Orphan Resolution**: Move recursive dependency logic from `StoreActor` into `SigChainManager`. Actor receives "Ready Entries", doesn't manage work_queues.
- [ ] **Unify Store Registries**: Remove `StoresRegistry` from lattice-net. Make `Node` single source of truth for active replicas.

### 4B: Architectural Pivot (Decoupling)

**Goal:** Clients use `StateMachine` (e.g., KvState) directly for reads/writes. Replication engine handles only log/sync operations.

```
┌────────────────────────────────────────────────────────────────┐
│                        Client (CLI)                             │
└──────────────────────────┬─────────────────────────────────────┘
                           │ read/write directly
                           ▼
┌────────────────────────────────────────────────────────────────┐
│                   lattice-kvstate                              │
│  KvState implementing StateMachine                             │
│  - get(key) → Vec<Head>       (local read, no network)        │
│  - put(key, value) → submits payload to ReplicationEngine     │
│  - list() → local read                                        │
│  - apply(&Op) ← receives from ReplicationEngine               │
└──────────────────────────┬─────────────────────────────────────┘
                           │ submit(payload) / apply(Op)
                           ▼
┌────────────────────────────────────────────────────────────────┐
│                   lattice-core (ReplicationEngine)             │
│  - submit(payload) → signs entry, commits to log, broadcasts  │
│  - ingest(entry) → validates, commits, calls S::apply()       │
│  - sync_state() → meta-operation for reconciliation           │
│  - NO get/list/watch commands (state machine specific)        │
└────────────────────────────────────────────────────────────────┘
```

- [x] **Extract lattice-model Crate**: `HLC`, `PubKey`, `Hash`, `Op`, `StateMachine` trait.
- [x] **Create lattice-kvstate Crate**: `KvState` implementing `StateMachine`, with KV-specific read/write methods.
- [ ] **Rename Core Components**: `StoreActor` → `ReplicationController`, `StoreHandle` → `Replica`.
- [ ] **Generify ReplicationController**: Works with `S: StateMachine`, handles only log/sync.
- [ ] **Direct Client Access**: Clients call `KvState.get()` directly (no actor channel).
- [ ] **StateWriter Trait Update**: Update `submit(payload)` to `submit(payload, parent_hashes)` to support DAG causality.
- [ ] **Submit Path**: `KvHandle.put()` → `Replica.submit(payload, parents)` → signs, logs, broadcasts.
- [ ] **Apply Path**: `Replica` receives entries → validates → calls `StateMachine::apply()`.


### 4C: Protocol Evolution (Type Agnosticism)

- [ ] **Update Protobuf Schema**: Deprecate `Operation` oneof. Add `topic` field for DAG grouping. Payload becomes opaque bytes.
- [ ] **Refactor Dependency Logic**: Use `Entry.topic` for DAG dependencies, not payload decoding
- [ ] **Refactor KvState as Plugin**: Move Put/Delete decoding inside `KvState::apply()`. Core agnostic to data type.

### 4D: Lifecycle & Optimization

- [ ] **Snapshotting Protocol**: `state.snapshot()` when log grows large. Store in `snapshot.db`.
- [ ] **Waterlevel Pruning**: Calculate stability frontier (min seq seen by all peers). `truncate_prefix(seq)` for old logs.
- [ ] **Typed Replica API**: `node.open_replica::<MyCustomCRDT>(uuid).await?`

---

## Milestone 5: Multi-Replica

**Goal:** Root store as control plane for declaring/managing additional replicas.

### 5A: Replica Declarations in Root Store
- [ ] Root store keys: `/replicas/{uuid}/name`, `/replicas/{uuid}/created_at`
- [ ] CLI: `replica create [name]`, `replica delete <uuid>`, `replica list`

### 5B: Replica Watcher ("Cluster Manager")

- [ ] `app_replicas: RwLock<HashMap<Uuid, Replica>>` in `Node`
- [ ] Initial Reconciliation: On startup, process `/replicas/` snapshot
- [ ] Live Reconciliation: Background task watching `/replicas/` prefix
- [ ] On Put: open new replica; On Delete: close/archive replica

### 5C: Multi-Replica Gossip
- [ ] `setup_for_replica` called for each active replica
- [ ] Per-replica gossip topics, verify replica-id before applying

### 5D: Shared Peer List (Ingest Guard)
- [ ] All replicas use root store peer list for authorization
- [ ] Check `/nodes/{pubkey}/status` on connect

### 5E: Mesh-Based Join Model
- [ ] A **mesh** = root store + subordinated replicas
- [ ] JoinRequest always targets the **mesh** (i.e., root store), not individual replicas
- [ ] After joining mesh, node gains access to all declared replicas via 5B reconciliation

---

## Milestone 6: HTTP API

**Goal:** External access to replicas via REST.

### 6A: Access Tokens
- [ ] Token storage: `/tokens/{id}/replica_id`, `/tokens/{id}/secret_hash`
- [ ] CLI: `token create`, `token list`, `token revoke`

### 6B: HTTP Server (lattice-http crate)
- [ ] REST endpoints: `GET/PUT/DELETE /replicas/{uuid}/keys/{key}`
- [ ] Auth via `Authorization: Bearer {token_id}:{secret}`

---

## Milestone 7: Counter Datatype

**Goal:** Add PN-Counter as a StateMachine implementation. See [pn-counter.md](pn-counter.md).

### 7A: Counter Module
- [ ] Add `CounterState` proto message
- [ ] Create `counter.rs` implementing `StateMachine`
- [ ] Implement merge and `incr(delta)`

### 7B: CLI
- [ ] `incr <key> [delta]`, `decr <key> [delta]`
- [ ] `get -v <key>` shows per-node breakdown

---

## Milestone 8: Content-Addressable Store (CAS) via Garage

**Goal:** Blob storage using Garage as S3-compatible sidecar.

### 8A: Garage Integration
- [ ] S3 client wrapper in `lattice-cas` crate
- [ ] `put_blob(data) -> hash`, `get_blob(hash) -> data`

### 8B: Metadata & Pinning
- [ ] `/cas/pins/{node_id}/{hash}` in root store
- [ ] Pin reconciler: watch pins, trigger Garage fetch

### 8C: CLI
- [ ] `cas put`, `cas get`, `cas pin`, `cas ls`

---

## Technical Debt

- [ ] **[CRITICAL] Refactor DAG Orphan Storage**: `OrphanStore` logic is currently coupled to specific Key types (legacy leakage), breaking the generic kernel architecture. `lattice-kernel` cannot buffer DAG orphans because it lacks key info.
  - **Impact**: Causal Delivery is bypassed; child operations may be applied before parents, leading to incorrect state (concurrent heads instead of merges).
  - **Fix**: Remove `key` dependency from `OrphanStore` (store by `entry_hash` only). Make logic purely hash-based.
- [ ] **Terminology**: Rename `parent_hashes` to `deps` (or `causal_deps`) to distinguish from `prev_hash` (log parent). Match `LogEntry::deps()` trait.
- [x] Proto: Change `HeadInfo.hlc` to proper `HLC` message type
- [x] Proto: Change `HLC.counter` from `uint32` to `uint16`
- [x] rename `history` command to `store history`
- [x] rename `peer sync` to `store sync`, drop single peer sync functionality
- [x] `peer invite` should output the node's id for easy joining
- [x] Async streaming in `do_stream_entries_in_range` (currently re-opens Log in sync thread)
- [x] split `lattice.proto` into network protocol and storage messages
- [x] Remove redundant `AUTHOR_TABLE` from DB - SigChainManager already loads all chains on startup
- [x] Strong Types: separate internal types (`Entry`, `SignedEntry`) from proto types with explicit conversion layer
- [x] **ChainTip ownership separation**: (1) `SigChainManager` owns in-memory ChainTips loaded from logs on startup, updated on append - used for sync state exchange in network protocol via `SigChainManager::sync_state()`. (2) `State::chain_tips_table` validates incoming entries against last applied tips - internal only, not exposed for sync.
- [x] **Entry::is_successor(tip)**: Add method on Entry to check `entry.prev_hash == tip.hash` instead of inline checks in `core.rs`
- [x] **ChainTip::encode()**: Add `encode(&self) -> Vec<u8>` method that hides proto conversion, cleaner than `.encode_to_vec().as_slice()`
- [x] **Rename Store → State**: Renamed `core.rs`→`state.rs` and `Store`→`State` to clarify it's derived materialized view
- [x] **Strong types for byte arrays**: `Hash` and `PubKey` for `[u8; 32]`, `Signature` for `[u8; 64]` - with proper Display/Debug
- [x] **HeadInfo.hlc Option cleanup**: Proto `HeadInfo.hlc` is `Option<Hlc>` but always set in practice - make non-optional or add `HLC::default()` fallback
- [x] Extract `PEER_SYNC_TABLE` from `state.db` for better separation
- [x] **Module reorganization**: Move sigchain-related files into `store/sigchain/` submodule (sigchain.rs, log.rs, orphan_store.rs, sync_state.rs)
- [x] **Zero-Knowledge of Peer Authorization (ACLs)**: `PeerProvider` trait with `can_join`, `can_connect`, `can_accept_entry`, `list_acceptable_authors`. Bootstrap authors from `JoinResponse` trusted during initial sync. Signature verification in `AuthorizedStore::ingest_entry`. Network layer checks peer status on gossip/RPC.
- [x] **Mesh Bootstrapping**: `JoinResponse` includes `authorized_authors` (all acceptable authors from inviter) so new nodes can accept entries during initial sync. Bootstrap authors cleared after sync completes.
- [x] Trait boundaries: `StoreHandle` (user ops) vs `AuthorizedStore` (network ops with peer authorization)
- [x] **Simplified StoreHandle**: Removed generic types (`StoreHandle<S>`), handler traits, and `StoreOps` enum; KvState is now the sole implementation with direct methods on handle
- [x] **Error Propagation Refactor**: Replaced 150+ production `.unwrap()`/`.expect()` with proper error handling
- [ ] Graceful shutdown with `CancellationToken` for spawned tasks
- [ ] Refactor `handle_peer_request` dispatch loop to use `irpc` crate for proper RPC semantics
- [ ] **REGRESSION**: Graceful reconnect after sleep/wake (may fix gossip regression)
- [ ] **Denial of Service (DoS) via Gossip**: Implement rate limiting in GossipManager and drop messages from peers who send invalid data repeatedly.
- [ ] **Checkpointing / Finality**
  - **Objective**: Protect against "Deep History Attacks" (leaked keys rewriting past) by periodically finalizing the state hash.
  - **Status**: **SECURITY NECESSITY** (Required for robust historical protection).
  - **Dependencies**: SigChain.
- [ ] **Streaming list_by_prefix**: Currently collects entire result into Vec before processing. Redb's `range()` returns an iterator, but we can't return it (lifetime tied to txn). Consider callback API or channels for large datasets.
- [ ] **Transactions / Batch Writes**: Group multiple store operations into a single sigchain entry for atomicity. Currently `Peer::save()` writes 4 separate keys which could be seen in inconsistent state by readers. A transaction API would bundle writes into one atomic entry.
- [ ] **Error Handling Review**: Re-evaluate usage of `expect`, `unwrap`, and `unwrap_or_default`. Ensure we are using the right strategy (fail-fast vs fail-safe) in appropriate contexts, particularly in critical paths like lock acquisition and network handlers.
- [ ] **Transactional Atomicity (Dual Commit Problem)**: `StoreActor::commit_entry` writes to two storage mediums (filesystem log via `SigChainManager`, redb via `KvState`) without unified transaction. If log succeeds but state fails, runtime inconsistency until restart. Solutions: (1) Store ChainTips in redb within same transaction as KV updates, (2) Enforce strict WAL pattern where file log is single source of truth, (3) Don't update state.db until file flush confirms success.
- [ ] **Encapsulation of Orphan Resolution Logic**: `StoreActor` contains complex domain logic for resolving recursive dependencies (`work_queue`, `OrphanMeta` tuple management). This leaks orphan storage implementation details into the Actor which should focus on concurrency/dispatch. Solutions: (1) Create `SigChainManager::ingest_and_resolve(entry)` that handles recursion internally, (2) Return `TransactionResult` struct with side effects (entries applied, orphans deleted), (3) Remove `work_queue` and manual orphan deletion from `StoreActor`.
- [ ] **Duplicate Store Registries (Race Conditions)**: `Node` and `MeshNetwork` maintain separate registries of active stores, synced via `NodeEvent::NetworkStore`. Event-driven sync introduces race conditions during startup or rapid mesh creation/deletion. Solutions: (1) Make `Node` the single source of truth, (2) Remove `StoresRegistry` from `MeshNetwork`/`MeshEngine`, (3) Query `Node` directly via `node.mesh_by_id(uuid)` when requests arrive, (4) Create `AuthorizedStore` on-the-fly or cache within `Node`.
- [ ] **Re-implement Watch Feature**: During the Replica/KvHandle refactor, the `watch()` functionality in `PeerManager::start_watching()` was stubbed out. Need to re-add watch support to `KvHandle` or add a watch mechanism to the KV state layer for reactive cache updates.

---

## Research & Protocol Evolution

Research areas and papers that may inform future Lattice development.

### Sync Efficiency: Negentropy
Range-based set reconciliation using hash fingerprints. Replaces O(n) vector clock sync with sub-linear bandwidth. Used by Nostr ecosystem.

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

**Required Infrastructure:**
- [ ] Add hash→entry index (for efficient fetch-by-hash)
- [ ] Implement negentropy fingerprint generation per store
- [ ] Replace `SyncState` protocol with negentropy exchange
- [ ] Decouple `seq` from network sync protocol (keep internal only)

- **Apply to:** `mesh/protocol.rs` sync diff logic, `SyncState`, `FetchRequest`
- **Ref:** [Negentropy Protocol](https://github.com/hoytech/negentropy)

### Data Pruning: Willow Protocol
3D key space (Author, Path, Time) with authenticated deletion. Newer timestamps deterministically overwrite older, enabling partial replication and actual byte deletion without breaking hash chains.
- **Apply to:** `store/state.rs` for pruning, `sigchain.rs` for subspace capabilities
- **Ref:** [Willow Protocol](https://willowprotocol.org/)

### Byzantine Fork Detection
Formal framework for detecting equivocation (same seq# with different content). Generate `FraudProof` for permanent blocklisting.
- **Apply to:** `sync_state.rs` fork detection → punitive action
- **Ref:** [Kleppmann & Howard (2020)](https://arxiv.org/abs/2012.00472)

### Storage: Merkle Search Trees / Prolly Trees
Order-independent Merkle roots for verifiable O(1) state comparison. Used by Bluesky (AT Protocol) and Dolt.
- **Apply to:** Snapshot verification, instant sync-state comparison
- **Ref:** [MST Paper (HAL)](https://hal.inria.fr/hal-02303490/document)

### Probabilistic Filters: IBLTs / Bloom Filters
Invertible Bloom Lookup Tables for probabilistic set difference. Reduces `missing_ranges` bandwidth when peers mostly in sync.
- **Apply to:** `SyncRequest` optimization

### Data Integrity: Verified Reads & Corruption Recovery
Verify all data read from disk (log files, redb store) via hash/signature checks. Gracefully handle corruption by marking damaged ranges and refetching from peers.
- **Apply to:** `log.rs` entry reads, `Store` operations, `SigChain` validation
- **Recovery:** Trigger targeted sync for corrupted author/seq ranges

### Sybil Resistance & Gossip Scaling
Mechanisms to handle millions of nodes and prevent gossip flooding from malicious actors.
- **Problem:** Unbounded gossip from "millions of nodes" (Sybil attack) overwhelms bandwidth/storage.
- **Mitigation:** Resource constraints (PoW), Web-of-Trust gossip limits (only gossip for friends-of-friends), or reputation scores.

### Byzantine Fault Tolerance (BFT)
Ensure system resilience against malicious peers who may lie, omit messages, or attempt to corrupt state (beyond simple forks).
- **Objective:** Validated consistency without a central authority or global consensus.
- **Strategy:** Local verification of all data (SigChains), cryptographic prohibition of history rewriting, and detection/rejection of invalid CRDT merges.


---

## Future

- **Split lattice-core**: Extract `lattice-node` (application/orchestration logic) and `lattice-kernel` (pure replication engine with SigChain, log, sync protocols). Kernel becomes embeddable library with minimal dependencies.
- TTL expiry for long-lived orphans (received_at timestamp now tracked)
- Transitive sync across all peers
- CRDTs: PN-Counters, OR-Sets for peer list
- Transaction Groups: atomic batched operations
- Watermark tracking & log pruning, possibly using HLC instead of sequence numbers
- Snapshots: fast bootstrap, point-in-time restore
- Mobile clients (iOS/Android)
- Key rotation
- Secure storage (Keychain, TPM)
- FUSE filesystem mount
- Merkle-ized state (signed root hash, O(1) sync checks)
- **CLI/Daemon Separation**: Refactor CLI to use gRPC internally (local socket, no network initially) to prepare for daemon/CLI split. Daemon runs as long-lived process, CLI becomes thin gRPC client.
- **Salted Gossip ALPN**: Use `/config/salt` from root store to salt the gossip ALPN per mesh (improves privacy by isolating mesh traffic).
