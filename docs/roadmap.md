# Lattice Roadmap

## Completed

- **M1**: Single-node append-only log with SigChain, HLC, redb KV, interactive CLI
- **M1.5**: DAG conflict resolution with multi-head tracking and deterministic LWW
- **M1.9**: Async refactor with store actor pattern and tokio runtime
- **M2**: Two-node sync via Iroh (mDNS + DNS), peer management, bidirectional sync
- **Stability**: Gossip join reliability, bidirectional sync fix, diagnostics, orphan timestamps
- **Sync Reliability**: Common HLC in SyncState, auto-sync on discrepancy with deferred check
- **Store Refactor**: Directory reorganization (`sigchain/`, `impls/kv/`), `Patch`/`ReadContext` traits, `KvPatch` with `TableOps`, encapsulated `StoreHandle::open()`
- **Simplified StoreHandle**: Removed generic types and handler traits; KvStore is now the only implementation with direct methods
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
- [ ] Implement `InviteToken` (JWT or similar) containing `mesh_id`, `peer_id`, `endpoints`.
- [ ] `mesh invite` generates token.
- [ ] `mesh join <token>` extracts info and joins using embedded secret.
- [ ] See [one-time-join-tokens.md](one-time-join-tokens.md).

### 3C: Node Registry Refactor
- [x] `Node::get_mesh()` → `Mesh` wrapper (Single-mesh MVP)
- [x] `Node::get_store()` → raw `StoreHandle`

### 3D: CLI Context Switching
- [ ] `mesh init`, `mesh list`
- [ ] `mesh switch`: change active CLI context (target for subsequent commands)
- [ ] `mesh up <id>`, `mesh down <id>` to start/stop specific meshes
- [ ] Support concurrent active meshes (node participates in multiple meshes simultaneously)

---

## Milestone 4: Multi-Store

**Goal:** Root store as control plane for declaring/managing additional stores.

### 4A: Store Declarations in Root Store
- [ ] Root store keys: `/stores/{uuid}/name`, `/stores/{uuid}/created_at`
- [ ] CLI: `store create [name]`, `store delete <uuid>`, `store list`

### 4B: Store Watcher ("Cluster Manager")

- [ ] `app_stores: RwLock<HashMap<Uuid, StoreHandle>>` in `Node`
- [ ] Initial Reconciliation: On startup, process `/stores/` snapshot
- [ ] Live Reconciliation: Background task watching `/stores/` prefix
- [ ] On Put: open new store; On Delete: close/archive store

### 4C: Multi-Store Gossip
- [ ] `setup_for_store` called for each active store
- [ ] Per-store gossip topics, verify store-id before applying

### 4D: Shared Peer List (Ingest Guard)
- [ ] All stores use root store peer list for authorization
- [ ] Check `/nodes/{pubkey}/status` on connect

### 4E: Mesh-Based Join Model
- [ ] A **mesh** = root store + subordinated stores
- [ ] JoinRequest always targets the **mesh** (i.e., root store), not individual stores
- [ ] After joining mesh, node gains access to all declared stores via 4B reconciliation

---

## Milestone 5: HTTP API

**Goal:** External access to stores via REST.

### 5A: Access Tokens
- [ ] Token storage: `/tokens/{id}/store_id`, `/tokens/{id}/secret_hash`
- [ ] CLI: `token create`, `token list`, `token revoke`

### 5B: HTTP Server (lattice-http crate)
- [ ] REST endpoints: `GET/PUT/DELETE /stores/{uuid}/keys/{key}`
- [ ] Auth via `Authorization: Bearer {token_id}:{secret}`

---

## Milestone 6: Counter Datatype

**Goal:** Add PN-Counter as a module on top of raw KV APIs. See [pn-counter.md](pn-counter.md).

### 6A: Counter Module
- [ ] Add `CounterState` proto message
- [ ] Create `counter.rs` with `Counter` struct
- [ ] Implement merge and `incr(delta)`

### 6B: CLI
- [ ] `incr <key> [delta]`, `decr <key> [delta]`
- [ ] `get -v <key>` shows per-node breakdown

---

## Milestone 7: Content-Addressable Store (CAS) via Garage

**Goal:** Blob storage using Garage as S3-compatible sidecar.

### 7A: Garage Integration
- [ ] S3 client wrapper in `lattice-cas` crate
- [ ] `put_blob(data) -> hash`, `get_blob(hash) -> data`

### 7B: Metadata & Pinning
- [ ] `/cas/pins/{node_id}/{hash}` in root store
- [ ] Pin reconciler: watch pins, trigger Garage fetch

### 7C: CLI
- [ ] `cas put`, `cas get`, `cas pin`, `cas ls`

---

## Technical Debt

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
- [x] **Simplified StoreHandle**: Removed generic types (`StoreHandle<S>`), handler traits, and `StoreOps` enum; KvStore is now the sole implementation with direct methods on handle
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
