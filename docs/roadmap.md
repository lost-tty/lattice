# Lattice Roadmap

## Completed

- **M1**: Single-node append-only log with SigChain, HLC, redb KV, interactive CLI
- **M1.5**: DAG conflict resolution with multi-head tracking and deterministic LWW
- **M1.9**: Async refactor with store actor pattern and tokio runtime
- **M2**: Two-node sync via Iroh (mDNS + DNS), peer management, bidirectional sync
- **Stability**: Gossip join reliability, bidirectional sync fix, diagnostics, orphan timestamps
- **Sync Reliability**: Common HLC in SyncState, auto-sync on discrepancy with deferred check
- **Store Refactor**: Directory reorganization (`sigchain/`, `impls/kv/`), `Patch`/`ReadContext` traits, `KvPatch` with `TableOps`, encapsulated `StoreHandle::open()`
- **Simplified StoreHandle**: Removed generic types and handler traits; KvStore is now the only implementation with direct methods (`get`, `put`, `delete`, `list`, `list_by_prefix`)

---

## Milestone 3: Counter Datatype

**Goal:** Add PN-Counter (Positive-Negative Counter) as a second value type alongside bare values.

### 3A: Value Type Wrapper (Backward Compatible)

Wrap values in protobuf `oneof` using **same field number** for zero-migration:

```protobuf
message HeadInfo {
    uint64 hlc = 1;
    bytes author = 2;
    bytes hash = 3;
    
    oneof data {
        bytes value = 5;          // Same field# as before!
        CounterState counter = 6; // New type
    }
}
```

Old data (field 5) deserializes into `data.value`. New counters use field 6.

**On read:**
```rust
match head_info.data {
    Some(Data::Value(bytes)) => Value::Raw(bytes),
    Some(Data::Counter(state)) => Value::Counter(state),
    None => Value::Raw(vec![]),
}
```

- [ ] Change `HeadInfo.value` to `oneof data { bytes value = 5; CounterState counter = 6; }`
- [ ] Add `CounterState` message to proto
- [ ] Update `KvStore` read/write to handle oneof

### 3B: Counter State

State-based PN-Counter stores each node's value separately:

```rust
struct CounterState {
    values: HashMap<PubKey, i64>,  // {node_a: 2, node_b: 5}
}

impl CounterState {
    fn total(&self) -> i64 { self.values.values().sum() }
    fn merge(&mut self, other: &Self) {
        for (node, val) in &other.values {
            let entry = self.values.entry(*node).or_insert(0);
            *entry = (*entry).max(*val);  // Take max per node
        }
    }
}
```

**Key insight:** Each node only updates its own slot. StoreActor already has `node: NodeIdentity`.

### 3C: Operations

- [ ] Add `Operation::CounterSet { key, value: i64 }` to proto (sets this node's value)
- [ ] `KvStore` needs `NodeIdentity` to know which slot to update
- [ ] Implement `KvStore::apply_counter_op()` - updates `values[self_node_id]`
- [ ] Add `StoreHandle::increment(key, delta)` - reads current, sets current+delta
- [ ] `get()` returns `CounterState::total()` as bytes for counter keys

### 3D: Storage

- [ ] New table or value type marker in `state.db`
- [ ] Counter state serialized as protobuf map
- [ ] Merge on read: combine all heads, next write may resolve (eventual consistency)

### 3E: CLI

- [ ] `incr <key> [delta]` - increment counter (default delta=1)
- [ ] `decr <key> [delta]` - decrement counter (default delta=1)
- [ ] `get <key>` shows total; `get -v <key>` shows per-node breakdown

### 3F: Conflict Resolution & Client API

**Counters:** Always auto-mergeable (CRDT). `get()` returns single merged value.

**Raw values:** Expose all heads to client for explicit resolution:
- `get(key)` → returns winner (deterministic LWW) + conflict flag
- `get_heads(key)` → returns all heads for client-side merge
- Client writes with `parent_hashes` pointing to heads it's resolving

This keeps current DAG behavior for raw values while counters "just work".

---

## Milestone 4: Mesh API & Registry Refactor

**Goal:** Type-safe API for mesh management with clear separation of controller (Mesh) vs data channel (StoreHandle).

See [architecture.md - Mesh API Facade Pattern](architecture.md#mesh-api-facade-pattern-future) for design.

### 4A: Mesh Wrapper Type
- [ ] Create `Mesh` struct wrapping root `StoreHandle` + `PeerProvider`
- [ ] Move `invite_peer`, `revoke_peer`, `list_peers` from `Node` to `Mesh`
- [ ] Add `Mesh::open_channel(uuid)` factory for subordinate stores

### 4B: Node Registry Refactor
- [ ] Change `Node::root_store` → `stores: HashMap<Uuid, StoreHandle>`
- [ ] Add `Node::get_mesh(id)` → returns `Mesh` wrapper
- [ ] Add `Node::get_store(id)` → raw `StoreHandle` access
- [ ] Update `meta_store` to track multiple managed stores

### 4C: CLI Context Switching
- [ ] Add `active_mesh` state to CLI session
- [ ] Implement `mesh init`, `mesh list`, `mesh switch` commands
- [ ] Data commands (`put`, `get`, `peer`) operate on active mesh

---

## Milestone 5: Multi-Store

**Goal:** Root store as control plane for declaring/managing additional stores.

### 5A: Store Declarations in Root Store
- [ ] Root store keys: `/stores/{uuid}/name`, `/stores/{uuid}/created_at`
- [ ] CLI: `create-store [name]`, `delete-store <uuid>`, `list-stores`

### 5B: Store Watcher ("Cluster Manager")

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

### 5C: Multi-Store Gossip
- [ ] `setup_for_store` called for each active store
- [ ] Per-store gossip topics, verify store-id before applying

### 5D: Shared Peer List (Ingest Guard)

All stores use root store peer list for authorization.

**Ingest Guard** (in `lattice-net/src/mesh/server.rs`):
- [ ] On `JoinRequest`/`StatusRequest`: extract `remote_pubkey`
- [ ] Check Root Store: `/nodes/{remote_pubkey}/status` == "active"?
- [ ] If authorized: proceed with sync
- [ ] If not: drop connection (or return 403)

---

## Milestone 6: HTTP API

**Goal:** External access to stores via REST.

### 6A: Access Tokens
- [ ] Token storage: `/tokens/{id}/store_id`, `/tokens/{id}/secret_hash`, `/tokens/{id}/permissions`
- [ ] CLI: `create-token`, `list-tokens`, `revoke-token`

### 6B: HTTP Server (lattice-http crate)
- [ ] REST endpoints: `GET/PUT/DELETE /stores/{uuid}/keys/{key}`
- [ ] Auth via `Authorization: Bearer {token_id}:{secret}`

---

## Milestone 7: Content-Addressable Store (CAS) via Garage

**Goal:** Blob storage using Garage as S3-compatible sidecar, orchestrated by root store.

**Architecture:**
```
Root Store (Lattice)              Garage Sidecar
┌─────────────────────┐          ┌─────────────────────┐
│ /cas/pins/{hash}    │ ───────▶ │ S3: lattice-blobs/  │
│ /blobs/{hash}/meta  │          │ (actual bytes)      │
│   orchestration     │          │ (replication, GC)   │
└─────────────────────┘          └─────────────────────┘
```

### 7A: Garage Integration

- [ ] Garage deployment config (Docker/systemd)
- [ ] S3 client wrapper in `lattice-core` or separate `lattice-cas` crate
- [ ] `put_blob(data) -> hash`, `get_blob(hash) -> data` using S3 API
- [ ] Content-addressed keys: `s3://lattice-blobs/{hash}`

### 7B: Metadata & Pinning

Root store tracks blob metadata and pins:

- [ ] `/cas/pins/{node_id}/{hash}` = pin status (`pending`/`stored`)
- [ ] `/blobs/{hash}/size` = blob size in bytes
- [ ] `/blobs/{hash}/created_at` = upload timestamp
- [ ] Pin reconciler: watch pins, trigger Garage fetch for `pending`

### 7C: CLI

- [ ] `cas put <file>` - upload to Garage, record in root store
- [ ] `cas get <hash>` - fetch from Garage
- [ ] `cas pin <hash>` - mark as pinned
- [ ] `cas ls` - list blobs with pin status

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
- [ ] **Multi-platform traits**: Add `StateBackend` trait to abstract KV storage (redb/sqlite/wasm)
- [ ] Refactor `handle_peer_request` dispatch loop to use `irpc` crate for proper RPC semantics
- [ ] Refactor any `.unwrap` uses
- [ ] Graceful shutdown with `CancellationToken` for spawned tasks (may fix gossip )
- [ ] **REGRESSION**: Graceful reconnect after sleep/wake (may fix gossip regression)
- [ ] **Denial of Service (DoS) via Gossip**: Implement rate limiting in GossipManager and drop messages from peers who send invalid data repeatedly.
- [ ] **Checkpointing / Finality**
  - **Objective**: Protect against "Deep History Attacks" (leaked keys rewriting past) by periodically finalizing the state hash.
  - **Status**: **SECURITY NECESSITY** (Required for robust historical protection).
  - **Dependencies**: SigChain.

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
