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
- **M3 (Mesh API)**: Fully refactored Mesh API with `Mesh` wrapper, Token-Based Join (`mesh invite/join`), Node Registry refactor, and CLI Context switching.
- **Watch Feature**: Verified `KvHandle` watch support (resolved in M4).
- **Streaming Scan**: Implemented `KvHandle::scan()` with visitor callback for large datasets.

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
- [x] **Rename Core Components**: `StoreActor` → `ReplicationController` (conceptually), `StoreHandle` → `Store`.
- [x] **Generify ReplicationController**: Works with `S: StateMachine` (`Store<S>`), handles only log/sync.
- [x] **Direct Client Access**: Clients call `KvState.get()` directly (no actor channel).
- [x] **StateWriter Trait Update**: Update `submit(payload)` to `submit(payload, parent_hashes)` to support DAG causality.
- [x] **Submit Path**: `KvHandle.put()` → `StateWriter.submit(payload)` → signs, logs, broadcasts.
- [x] **Apply Path**: `Store` receives entries → validates → calls `StateMachine::apply()`.

### 4C: Protocol Evolution (Type Agnosticism)

- [x] **Refactor KvState as Plugin**: Move Put/Delete decoding inside `KvState::apply()`. Core agnostic to data type.

### 4D: Lifecycle & Optimization

- [x] **Typed Store API**: `node.open_store::<MyCustomCRDT>(uuid)` (Enabled via Generic `StoreRegistry`)

### 4E: Generic CLI & Introspection (Next Up)

- [x] **Store Command Introspection (gRPC)**: `StateMachine` exposes a `ServiceDescriptor`.
- [x] **Dynamic CLI**: `lattice-cli` uses `prost-reflect` to dynamically build commands and decode log payloads.

### 4F: Lattice-Kernel Audit & Stability

- [x] Move `PeerSyncStore` out of `lattice-kernel` and into `lattice-net`.
- [ ] **Lattice-Kernel Audit**: Thorough review of `lattice-kernel` to ensure architectural cleanliness, proper visibility, and minimal dependencies before declaring it stable.
  - [ ] **Enforce strict limit on causal_deps**: Prevent DoS by capping `entry.causal_deps` len (e.g. 1024).
  - [x] **Rename ReplicatedState to ReplicationController**: Align code name with architectural concept.
  - [x] **Environmental Agnosticism**: Removed `hostname` dependency.
  - [x] **Runtime Decoupling**: "Ownership Inversion" for threading (Node spawns threads).
- [x] **Transactional Atomicity (Dual Commit Problem)**: `StoreActor::commit_entry` writes to two storage mediums (filesystem log via `SigChainManager`, redb via `KvState`) without unified transaction. If log succeeds but state fails, runtime inconsistency until restart.
  - [x] **Fix WAL Inversion**: Current order (State then Log) is unsafe. Must be **Log (WAL) then State**. Log is source of truth.
  - [x] Solutions: (1) Store ChainTips in redb within same transaction as KV updates, (2) Enforce strict WAL pattern where file log is single source of truth, (3) Don't update state.db until file flush confirms success.
- [x] **Encapsulation of Orphan Resolution Logic**: `OrphanStore` currently requires a `key` argument, coupling it to KV-semantics. Refactor `OrphanStore` and `SigChainManager` to strictly use `causal_deps` (hashes) for DAG orphan detection. This enables generic buffering for any StateMachine (e.g. Chat/Counter) without decoding payloads. Solutions: (1) Remove `key` from `DagOrphanKey` in `redb`, (2) Create `SigChainManager::ingest_and_resolve` that handles recursion internally.

---

## Milestone 5: KvStore Transactions

**Goal:** Atomic multi-key operations for consistent writes.

### 5A: Local Atomic Batch
- [x] `KvHandle::batch()` builder API: `batch().put(k1, v1).put(k2, v2).delete(k3).commit()`
- [x] Single entry in sigchain containing all operations
- [x] All-or-nothing semantics: either all ops apply or none

### 5B: Batch Payload Format
- [x] Extend `KvPayload` to support multiple operations per entry
- [x] Maintain backward compatibility with single-op entries

---

## Milestone 6: Multi-Store

**Goal:** Root store as control plane for declaring/managing additional stores.

### 6A: Store Declarations in Root Store
- [x] Root store keys: `/stores/{uuid}/name`, `/stores/{uuid}/created_at`, `/stores/{uuid}/type`
- [x] CLI: `store create [name] --type <kvstore|chat|...>`, `store delete <uuid>`, `store list`

### 6B: Store Watcher ("Cluster Manager")

- [x] `app_stores: RwLock<HashMap<Uuid, Store>>` in `StoreManager`
- [x] Initial Reconciliation: On startup, process `/stores/` snapshot
- [x] Live Reconciliation: Background task watching `/stores/` prefix
- [x] On Put: open new store; On Delete: close/archive store

### 6C: Multi-Store Gossip
- [x] `setup_for_store` called for each active store
- [x] Per-store gossip topics, verify store-id before applying

### 6D: Shared Peer List (Ingest Guard)
- [x] All stores use root store peer list for authorization (`AuthorizedStore`)
- [x] Check `/nodes/{pubkey}/status` on connect

### 6E: Mesh-Based Join Model
- [x] A **mesh** = root store + subordinated stores
- [x] JoinRequest always targets the **mesh** (i.e., root store), not individual stores
- [x] After joining mesh, node gains access to all declared stores via 6B reconciliation

---

## Milestone 7: ChatRoom

**Goal:** Create a specialized `ChatRoom` to demonstrate `lattice` as a messaging platform.

### 7A: Chat Module
- [ ] `ChatState`: Append-only list of messages (no KV overhead).
- [ ] `post(text)`, `reply(ref, text)`, `react(ref, emoji)`.
- [ ] Causal sorting: Ensure replies render after parents.

### 7B: CLI Chat Client
- [ ] `chat post <msg>`, `chat ls` (render thread).
- [ ] Real-time updates via `watch()`.

---

## Milestone 8: Negentropy Sync Protocol

**Goal:** Replace O(n) vector clock sync with sub-linear bandwidth using range-based set reconciliation.

Range-based set reconciliation using hash fingerprints. Used by Nostr ecosystem.

### 8A: Infrastructure
- [ ] Add hash→entry index (for efficient fetch-by-hash)
- [ ] Implement negentropy fingerprint generation per store

### 8B: Protocol Migration
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

## Milestone 9: Client/Daemon Split (CLI Separation)

**Goal:** Decouple the CLI from the Node application logic, establishing a true Daemon/Client architecture.

**Reference:** Kubernetes (CRI uses gRPC/UDS), Docker (dockerd/docker-cli).

### 9A: Lattice Daemon (`latticed`)
- [ ] New binary `latticed`: Long-running background process.
- [ ] Hosts `Node`, P2P Networking, and Storage.
- [ ] Exposes **gRPC API** over **Unix Domain Sockets** (macOS/Linux) or Named Pipes (Windows).
- [ ] **Security:** File system permissions (`0600`) restrict access to the user.

### 9B: Lattice CLI (`lattice`)
- [ ] Refactor CLI to be a thin gRPC client.
- [ ] Connects to default socket `~/.lattice/control.sock`.
- [ ] Supports multiple concurrent clients (e.g., CLI + Menu Bar App + Web GUI).

### 9C: Multi-Head Support
- [ ] **CLI**: Interactive text-based control.
- [ ] **GUI**: Native Swift/Rust UI connecting to the same daemon UDS.
- [ ] **Web**: Optional HTTP gateway for browser-based access (like Syncthing).

---

## Milestone 10: HTTP API (lattice-http)

**Goal:** External access to stores via REST.

### 10A: Access Tokens
- [ ] Token storage: `/tokens/{id}/store_id`, `/tokens/{id}/secret_hash`
- [ ] CLI: `token create`, `token list`, `token revoke`

### 10B: HTTP Server
- [ ] REST endpoints: `GET/PUT/DELETE /stores/{uuid}/keys/{key}`
- [ ] Auth via `Authorization: Bearer {token_id}:{secret}`

---

## Milestone 11: Content-Addressable Store (CAS) via Garage

**Goal:** Blob storage using Garage as S3-compatible sidecar.

### 11A: Garage Integration
- [ ] S3 client wrapper in `lattice-cas` crate
- [ ] `put_blob(data) -> hash`, `get_blob(hash) -> data`

### 11B: Metadata & Pinning
- [ ] `/cas/pins/{node_id}/{hash}` in root store
- [ ] Pin reconciler: watch pins, trigger Garage fetch

### 11C: CLI
- [ ] `cas put`, `cas get`, `cas pin`, `cas ls`

---

## Technical Debt

- [ ] Graceful shutdown with `CancellationToken` for spawned tasks
- [ ] Refactor `handle_peer_request` dispatch loop to use `irpc` crate for proper RPC semantics
- [ ] **REGRESSION**: Graceful reconnect after sleep/wake (may fix gossip regression)
- [ ] **Denial of Service (DoS) via Gossip**: Implement rate limiting in GossipManager and drop messages from peers who send invalid data repeatedly.
- [ ] **Checkpointing / Finality**
  - **Objective**: Protect against "Deep History Attacks" (leaked keys rewriting past) by periodically finalizing the state hash.
  - **Status**: **SECURITY NECESSITY** (Required for robust historical protection).
  - **Dependencies**: SigChain.
- [ ] **Transactions / Batch Writes**: Group multiple store operations into a single sigchain entry for atomicity. Currently `Peer::save()` writes 4 separate keys which could be seen in inconsistent state by readers. A transaction API would bundle writes into one atomic entry.
- [ ] **Unified Store Registry**: Eliminate race conditions between `Node` and `MeshNetwork`. `Node` becomes the single source of truth. Offline/Online status should be a state flag in `Node`, not determined by presence/absence in a duplicate `MeshEngine` map.
- [ ] **Snapshotting Protocol**: `state.snapshot()` when log grows large. Store in `snapshot.db`.
- [ ] **Waterlevel Pruning**: Calculate stability frontier (min seq seen by all peers). `truncate_prefix(seq)` for old logs.
- [ ] **Hash Index Optimization**: `SigChainManager` currently keeps all history hashes in RAM (`HashSet<Hash>`). For large logs (100M+ entries), replace with Bloom Filter or on-disk index (redb) to reduce memory footprint.

---

## Research & Protocol Evolution

Research areas and papers that may inform future Lattice development.

### Sync Efficiency: Negentropy

> **Note:** Negentropy is now **Milestone 7**. See above for implementation details.

### Byzantine Fork Detection
Formal framework for detecting equivocation (same seq# with different content). Generate `FraudProof` for permanent blocklisting.
- **Apply to:** `sync_state.rs` fork detection → punitive action
- **Ref:** [Kleppmann & Howard (2020)](https://arxiv.org/abs/2012.00472)

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

### Payload Validation Strategy (To Decide)

**Problem:** Where should semantic payload validation occur, and what happens when validation fails?

**Current state:**
- Kernel layer validates: signature, chain structure (prev_hash), entry size
- State machine layer (KvState) validates: empty keys (currently rejects, breaks chain)

**The tension:**
| Approach | Strictness | Availability |
|----------|-----------|--------------|
| Reject at apply_op | High | One bad entry bricks author forever |
| Skip bad ops, advance chain | Low | Bad data ignored, chain continues |
| Entry replacement/reorg | High | Complex, attack surface |

**The deterministic replay problem:**
If validation rules in `apply_op` can change between software versions, state diverges on replay:
- Node A (v1 rules): skips empty key
- Node B (v2 rules): applies empty key
- → State divergence = broken system

**Options to evaluate:**
1. **Build-time only validation** - Validate at `BatchBuilder::commit()`, never at `apply_op()`. Once signed, always apply. Maintains deterministic replay.
2. **Versioned validation rules** - Entry includes schema version. Validation tied to version. Complex.
3. **Entry replacement protocol** - Allow authors to "supersede" bad entries with corrective entries. Fork resolution required.
4. **Separate chain advancement from payload application** - Kernel advances chaintip (chain is valid), state machine skips bad ops (no effect). Entry exists in history but is a no-op.

**Current test case:** `test_rejected_entry_breaks_chain` in `lattice-kvstate/src/kv.rs` demonstrates the chain break problem.

**Decision needed:** Which validation model best fits lattice's goals of reliability, CRDT convergence, and security?

---

## Future

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
- **Salted Gossip ALPN**: Use `/config/salt` from root store to salt the gossip ALPN per mesh (improves privacy by isolating mesh traffic).
