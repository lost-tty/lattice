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

## Milestone 3: Multi-Node Mesh (Current)

**Goal:** N nodes form a gossip mesh for real-time sync.

### Completed

**LatticeServer Refactor** ✓
- `LatticeServer` struct wrapping `Arc<Node>` + `Endpoint`
- Encapsulated accept loop via Router + ProtocolHandler

**Gossip Protocol** ✓ (iroh-gossip)
- Router handles `lattice-sync/1` and `/iroh-gossip/1` ALPNs
- Auto-join gossip topic on root store activation
- Broadcast local entries, receive and apply gossip entries
- Topic ID via `blake3::hash("lattice/{store_id}")`
- Bootstrap peers from `/nodes/` via Key Watcher

**Key Watcher** ✓
- `store.watch(regex) -> (initial, Receiver<WatchEvent>)`
- StoreActor tracks watchers, emits on matching put/delete
- Enables reactive patterns: config changes, presence, app-level subscriptions

**Unified Entry Validation** ✓
- Entry written to log ONLY when sigchain AND DAG parent checks pass
- Hash index in SigChainManager for O(1) parent existence checks
- OrphanStore for buffering entries awaiting parents
- Iterative orphan resolution (prevent stack overflow)

**Orphan Gap-Filling** ✓
- Gap detection emits `GapInfo` event via broadcast channel
- `sync_author_all()` for targeted sync across peers
- Gap watcher task triggers sync on detected gaps
- Duplicate entry detection (SigchainValidation::Duplicate) prevents stale orphans

### In Progress

**Orphan Management**
- [ ] TTL expiry for long-lived orphans
- [ ] Retry logic: if gap persists after sync, retry with different peer

**Regressions**
- [ ] Syncing new peer leads to multi-head key (corrupted/missing seq:1 for some authors)
- [ ] On join, node does not reliably join gossip

---

## Phase 3: Multi-Store & Reliability (Next)

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

### 3E: HTTP API Access Tokens
- [ ] Token storage: `/tokens/{id}/store_id`, `/tokens/{id}/secret_hash`, `/tokens/{id}/permissions`
- [ ] CLI: `create-token`, `list-tokens`, `revoke-token`

### 3F: HTTP API Server (lattice-http crate)
- [ ] REST endpoints: `GET/PUT/DELETE /stores/{uuid}/keys/{key}`
- [ ] Auth via `Authorization: Bearer {token_id}:{secret}`

### Gossip Reliability
- [ ] **REGRESSION**: Gossip loses connectivity after sleep/wake
- [ ] Periodic anti-entropy sync to catch missed entries
- [ ] Detect stale gossip → trigger sync

### Sync Resilience
- [ ] Retry failed syncs with backoff
- [ ] Handle partial sync (peer disconnects mid-sync)
- [ ] Offline nodes should not block sync (timeout + skip)

---

## Technical Debt

**Crate Refactoring: lattice-store**
- [ ] Extract `Store`, `SigChain`, `StoreActor` into separate crate
- [ ] Trait boundaries: `KvStore` (user ops) vs `SyncStore` (network ops)

**Proto Schema**
- [ ] Change `HeadInfo.hlc` to proper `HLC` message type
- [ ] Change `HLC.counter` from `uint32` to `uint16`

**Lifecycle Management**
- [ ] Graceful shutdown with `CancellationToken` for spawned tasks

**Error Handling**
- [ ] Replace `Result<..., String>` with proper error types

---

## Future

- Offline nodes should not delay sync
- Transitive sync across all peers
- CRDTs: PN-Counters, OR-Sets for peer list
- Transaction Groups: atomic batched operations
- Watermark tracking & log pruning
- Snapshots: fast bootstrap, point-in-time restore
  - Needs architecture discussion: diffing approach without intermediate log
- Mobile clients (iOS/Android)
- Key rotation
- Secure storage (Keychain, TPM)
- FUSE filesystem mount
- Merkle-ized state (signed root hash, O(1) sync checks)
