# Lattice Roadmap

## Completed Milestones

### M1: Single-Node Append-Only Log ✓
Node identity, HLC timestamps, entry signing, log file I/O, SigChain validation, redb store with KV tables, interactive CLI. Multi-store support with per-store directories and UUIDs.

### M1.5: DAG Conflict Resolution ✓
Multi-head tracking per key, deterministic LWW reads with HLC+author tiebreaker, parent_hashes for DAG causality, merge semantics via citing multiple heads.

### M1.9: Async Refactor ✓
Store actor pattern (dedicated thread, channel commands), tokio runtime, async CLI via block_in_place.

### M2: Two-Node Sync ✓
Iroh integration (mDNS + DNS discovery), peer management via `/nodes/` keys, join protocol with store UUID exchange, bidirectional sync with SyncState diff, framed protobuf messaging.

---

## Priority 1: Stability

**Goal:** Fix known bugs and add diagnostic tooling.

### Regressions
- [x] On join, node does not reliably join gossip
- [x] `store sync` seems to be uni-directional now

### Diagnostics
- [x] Unicast `peer status` command with sync matrix and RTT
- [x] Track gossip NeighborUp/Down events for real-time peer online status in `peers` command

### Orphan Management
- [x] Track received_at timestamp for orphans (preparation for TTL)

---

## Priority 2: Sync & Gossip Reliability

**Goal:** Production-ready mesh networking with simple, unified sync loop.

- [ ] **REGRESSION**: Graceful reconnect after sleep/wake (may need iroh fix)
- [x] Add highest HLC common to all local logs to SyncState and show it in `store status` for each node
- [ ] Auto-trigger direct sync with peer when sync state discrepancy detected (with throttling)

---

## Priority 3: Multi-Store

**Goal:** Root store as control plane for declaring/managing additional stores.

### 3A: Store Declarations in Root Store
- [ ] Root store keys: `/stores/{uuid}/name`, `/stores/{uuid}/created_at`
- [ ] CLI: `create-store [name]`, `delete-store <uuid>`, `list-stores`

### 3B: Store Watcher (automatic materialization)
- [ ] Node watches `/stores/` prefix, auto-creates local stores
- [ ] Startup reconciliation with root store declarations

### 3C: Multi-Store Gossip
- [ ] `setup_for_store` called for each active store
- [ ] Per-store gossip topics, verify store-id before applying

### 3D: Shared Peer List
- [ ] All stores use root store peer list (`/nodes/`)
- [ ] Peer authorization checked against root store on ingest

---

## Priority 4: HTTP API

**Goal:** External access to stores via REST.

### 4A: Access Tokens
- [ ] Token storage: `/tokens/{id}/store_id`, `/tokens/{id}/secret_hash`, `/tokens/{id}/permissions`
- [ ] CLI: `create-token`, `list-tokens`, `revoke-token`

### 4B: HTTP Server (lattice-http crate)
- [ ] REST endpoints: `GET/PUT/DELETE /stores/{uuid}/keys/{key}`
- [ ] Auth via `Authorization: Bearer {token_id}:{secret}`

---

## Priority 5: WebAssembly Consensus Bus

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

**Crate Refactoring** (larger effort, defer)
- [ ] Extract `Store`, `SigChain`, `StoreActor` into `lattice-store` crate
- [ ] Trait boundaries: `KvStore` (user ops) vs `SyncStore` (network ops)
- [ ] Graceful shutdown with `CancellationToken` for spawned tasks (may fix gossip regression)
- [x] Proto: Change `HeadInfo.hlc` to proper `HLC` message type
- [x] Proto: Change `HLC.counter` from `uint32` to `uint16`
- [x] rename `history` command to `store history`
- [x] rename `peer sync` to `store sync`, drop single peer sync functionality
- [x] `peer invite` should output the node's id for easy joining
- [x] Async streaming in `do_stream_entries_in_range` (currently re-opens Log in sync thread)
- [ ] split `lattice.proto` into network protocol and storage messages
- [ ] Refactor `handle_peer_request` dispatch loop to use `irpc` crate for proper RPC semantics
- [ ] Refactor any `.unwrap` uses
- [ ] Remove redundant `AUTHOR_TABLE` from DB - SigChainManager already loads all chains on startup

---

## Research & Protocol Evolution

Research areas and papers that may inform future Lattice development.

### Sync Efficiency: Negentropy
Range-based set reconciliation for efficient diff calculation. Replaces O(n) vector clock sync with sub-linear bandwidth. Used by Nostr ecosystem.
- **Apply to:** `mesh/protocol.rs` sync diff logic
- **Ref:** [Negentropy Protocol](https://github.com/hoytech/negentropy)

### Data Pruning: Willow Protocol
3D key space (Author, Path, Time) with authenticated deletion. Newer timestamps deterministically overwrite older, enabling partial replication and actual byte deletion without breaking hash chains.
- **Apply to:** `store/core.rs` for pruning, `sigchain.rs` for subspace capabilities
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
- **CAS (Content-Addressable Store)**: Optional per-node blob storage by hash. Not all nodes required to store blobs. A separate "pin map" (CRDT) in a regular store dictates which objects each node should persistently store. On read, nodes cache fetched blobs and pin anything declared in the shared pin map.
