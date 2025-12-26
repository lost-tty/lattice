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

### M3: Multi-Node Mesh (Partial) ✓
LatticeServer refactor, gossip protocol (iroh-gossip), Key Watcher for reactive patterns, unified entry validation with hash index, orphan gap-filling with targeted sync.

---

## Priority 1: Stability

**Goal:** Fix known bugs and add diagnostic tooling.

### Regressions
- [ ] On join, node does not reliably join gossip

### Diagnostics
- [ ] Lightweight peer ping: broadcast (no arg) or unicast (peer id), returns sync_state/watermark

### Orphan Management
- [ ] TTL expiry for long-lived orphans
- [ ] Retry logic: if gap persists after sync, retry with different peer

### Tech Debt
- [ ] Graceful shutdown with `CancellationToken` for spawned tasks (may fix gossip regression)
- [ ] Proto: Change `HeadInfo.hlc` to proper `HLC` message type
- [ ] Proto: Change `HLC.counter` from `uint32` to `uint16`

---

## Priority 2: Sync & Gossip Reliability

**Goal:** Production-ready mesh networking.

### Gossip Reliability
- [ ] **REGRESSION**: Gossip loses connectivity after sleep/wake
- [ ] Periodic anti-entropy sync to catch missed entries
- [ ] Detect stale gossip → trigger sync

### Sync Resilience
- [ ] Retry failed syncs with backoff
- [ ] Handle partial sync (peer disconnects mid-sync)
- [ ] Offline nodes should not block sync (timeout + skip)

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

---

## Future

- Transitive sync across all peers
- CRDTs: PN-Counters, OR-Sets for peer list
- Transaction Groups: atomic batched operations
- Watermark tracking & log pruning
- Snapshots: fast bootstrap, point-in-time restore
- Mobile clients (iOS/Android)
- Key rotation
- Secure storage (Keychain, TPM)
- FUSE filesystem mount
- Merkle-ized state (signed root hash, O(1) sync checks)
