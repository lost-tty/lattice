# Lattice Roadmap

## Completed

- **M1**: Single-node append-only log with SigChain, HLC, redb KV, interactive CLI
- **M1.5**: DAG conflict resolution with multi-head tracking and deterministic LWW
- **M1.9**: Async refactor with store actor pattern and tokio runtime
- **M2**: Two-node sync via Iroh (mDNS + DNS), peer management, bidirectional sync
- **Stability**: Gossip join reliability, bidirectional sync fix, diagnostics, orphan timestamps
- **Sync Reliability**: Common HLC in SyncState, auto-sync on discrepancy with deferred check

## Milestone 3: Extract `lattice-store` Crate

**Goal:** Clean crate boundaries as foundation for Multi-Store and CAS.

**Files to extract** (~270KB):
- `actor.rs`, `core.rs`, `handle.rs`, `log.rs`, `mod.rs`
- `sigchain.rs`, `signed_entry.rs`, `sync_state.rs`, `orphan_store.rs`

**Steps:**
- [ ] Create `lattice-store` crate with `Cargo.toml`
- [ ] Move store files, update internal imports
- [ ] Export `StoreHandle`, `Store`, `SyncState`, `SyncNeeded`, etc.
- [ ] `lattice-core` depends on `lattice-store`, re-exports types
- [ ] Update `lattice-net` imports
- [ ] Run tests, fix any breakage

### 3B: Apply Trait (Logic Encapsulation)

Move operation logic from `Store` into `Operation` types. Store becomes a dumb transaction provider.

```rust
pub trait StateContext {
    fn get(&self, key: &[u8]) -> Option<Vec<u8>>;
    fn put(&mut self, key: &[u8], value: &[u8]);
}

pub trait Applyable {
    fn apply(&self, ctx: &mut dyn StateContext, meta: &OpMetadata) -> Result<(), OpError>;
}
```

- [ ] Define `StateContext` and `Applyable` traits in `ops.rs`
- [ ] Implement `Applyable` for `Operation` proto type
- [ ] Refactor `Store` to use trait dispatch instead of match statement
- [ ] Add `MockContext` for unit testing operations in isolation

---

## Milestone 4: Multi-Store

**Goal:** Root store as control plane for declaring/managing additional stores.

### 4A: Store Declarations in Root Store
- [ ] Root store keys: `/stores/{uuid}/name`, `/stores/{uuid}/created_at`
- [ ] CLI: `create-store [name]`, `delete-store <uuid>`, `list-stores`

### 4B: Store Watcher ("Cluster Manager")

Two-phase reconciliation:

**Data Structure** (in `Node`):
```rust
app_stores: tokio::sync::RwLock<HashMap<Uuid, StoreHandle>>
```

**Implementation**:
- [ ] Initial Reconciliation: On startup, process `/stores/` snapshot before returning
- [ ] Live Reconciliation: Spawn background task watching `/stores/` prefix
- [ ] On `Put`: Parse UUID, check if already running, open if new
- [ ] On `Delete`: Optionally close/archive store

**Edge Case**: Make watcher async/non-blocking. `Node::start` returns after Root Store opens locally - don't block on network sync. App Stores materialize as watcher processes local DB state.

### 4C: Multi-Store Gossip
- [ ] `setup_for_store` called for each active store
- [ ] Per-store gossip topics, verify store-id before applying

### 4D: Shared Peer List (Ingest Guard)

All stores use root store peer list for authorization.

**Ingest Guard** (in `lattice-net/src/mesh/server.rs`):
- [ ] On `JoinRequest`/`StatusRequest`: extract `remote_pubkey`
- [ ] Check Root Store: `/nodes/{remote_pubkey}/status` == "active"?
- [ ] If authorized: proceed with sync
- [ ] If not: drop connection (or return 403)

---

## Milestone 5: HTTP API

**Goal:** External access to stores via REST.

### 5A: Access Tokens
- [ ] Token storage: `/tokens/{id}/store_id`, `/tokens/{id}/secret_hash`, `/tokens/{id}/permissions`
- [ ] CLI: `create-token`, `list-tokens`, `revoke-token`

### 5B: HTTP Server (lattice-http crate)
- [ ] REST endpoints: `GET/PUT/DELETE /stores/{uuid}/keys/{key}`
- [ ] Auth via `Authorization: Bearer {token_id}:{secret}`

---

## Milestone 6: Content-Addressable Store (CAS)

**Goal:** Pressure-based blob storage with automatic caching and zombie redundancy.

### 6A: Core (`lattice-cas` crate)
- [ ] `BlobStore` struct with `put(data) -> hash`, `get(hash) -> data`
- [ ] Content-addressed storage: `~/.local/share/lattice/cas/{hash[0:2]}/{hash}.blob`
- [ ] Access time tracking: `get()` touches file for LRU

### 6B: Pin Reconciler
- [ ] Watch `/cas/pins/{my_id}/` prefix in KV
- [ ] On `pending`: fetch blob from peers, write to disk, update to `stored`
- [ ] On delete: demote to cache (don't delete file)

### 6C: Garbage Collector
- [ ] Config: `storage_quota`, `min_free_space`
- [ ] Trigger: periodic or after large writes
- [ ] LRU eviction: filter pinned, sort by atime, delete oldest
- [ ] Update `/blobs/{hash}/nodes/{me}` on eviction

### 6D: Global Discovery
- [ ] `/blobs/{hash}/nodes/{node_id}` = presence marker
- [ ] Fetch: query peers for `/blobs/{hash}/nodes/*`, request from first responder

### 6E: Manifests (for scale)
- [ ] Manifest format: content-addressed list of hashes (like Git Trees)
- [ ] Recursive pinning: pin manifest → implicitly pin all referenced blobs
- [ ] Graph-aware GC: walk pinned manifests to build reachability set before eviction
- [ ] CLI: `cas manifest create <files...>`, `cas manifest list <hash>`

## Milestone 7: WebAssembly Consensus Bus

**Goal:** Transform from passive KV store to replicated state machine.

See [architecture/wasm-consensus-bus.md](architecture/wasm-consensus-bus.md) for detailed design.

### Phase 1: Protocol
- [ ] Add `Instruction` message alongside legacy `Operation` enum
- [ ] Backward compatible: existing put/delete still works

### Phase 2: Dispatcher
- [ ] Refactor `StoreActor` to route by `program_id`
- [ ] Native handler for `sys` (existing redb logic)

### Phase 3: Runtime
- [ ] Embed `wasmtime` runtime
- [ ] Define host functions: `db_get`, `db_put`

### Phase 4: Pilot
- [ ] Deploy first WASM contract (Counter or Tagging bot)

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
- [ ] **Multi-platform traits**: Add `StateBackend` trait to abstract KV storage (redb/sqlite/wasm)
- [ ] Refactor `handle_peer_request` dispatch loop to use `irpc` crate for proper RPC semantics
- [ ] Refactor any `.unwrap` uses
- [ ] Trait boundaries: `KvStore` (user ops) vs `SyncStore` (network ops)
- [ ] Graceful shutdown with `CancellationToken` for spawned tasks (may fix gossip )
- [ ] **REGRESSION**: Graceful reconnect after sleep/wake (may fix gossip regression)

---

## Research & Protocol Evolution

Research areas and papers that may inform future Lattice development.

### Sync Efficiency: Negentropy
Range-based set reconciliation for efficient diff calculation. Replaces O(n) vector clock sync with sub-linear bandwidth. Used by Nostr ecosystem.
- **Apply to:** `mesh/protocol.rs` sync diff logic
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
