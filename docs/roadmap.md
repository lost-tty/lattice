# Lattice Roadmap

## Milestone 1: Single-Node Append-Only Log

**Goal:** A single node can create, sign, and persist entries to its own log. No networking yet.

### Deliverables

- [x] HLC timestamps
- [x] Node identity (Ed25519 keypair, save/load)
- [x] Entry signing & verification
- [x] Log file I/O (append, read, hash verification)
- [x] SigChain (validate entries before appending)
- [x] Store (redb) — `kv` + `meta` tables, log replay
- [x] Interactive CLI: `init`, `put`, `get`, `delete`, `status`, `quit`

### Success Criteria

- Can create a new identity
- Can append entries to local log
- Can replay log to reconstruct KV state
- All operations survive restart

### Multi-KV Refactoring (before M2) ✓

- [x] DataDir → `stores/{uuid}/` subdirectories
- [x] Store → per-store state.db
- [x] Log paths → `stores/{uuid}/logs/{author}.log`
- [x] Proto: Entry has store_id (UUID)
- [x] CLI → `init`, `create-store`, `list-stores`, `use`
- [x] meta.db stores table (MetaStore)
- [x] SigChain → validate entry.store_id

---

## Milestone 1.5: DAG Conflict Resolution

**Goal:** Upgrade store from simple LWW to DAG-based conflict resolution per architecture.md.

### Deliverables

- [x] Proto: Add `repeated bytes parent_hashes` to Entry (for DAG causality)
- [x] Proto: Add `HeadInfo` message for multi-head storage
- [x] Store: KV table schema → `Vec<u8> → Vec<HeadInfo>`
- [x] Store: `apply_entry` → track multiple heads, merge parent tips
- [x] Store: `get` → deterministic winner (highest HLC, author tiebreaker)
- [x] Store: `get_heads` → inspect all heads for a key
- [x] EntryBuilder: `.parent_hashes(...)` method for DAG ancestry
- [x] CLI: Show conflict indicator when multiple heads

### Success Criteria

- [x] Concurrent writes to same key create multiple heads
- [x] Reads return deterministic winner
- [x] Next write citing both heads merges fork to single tip
- [x] All existing tests still pass (71 tests)

---

## Milestone 1.9: Async Refactor

**Goal:** Prepare codebase for concurrent CLI + network operation.

### Deliverables

**Phase 1: Store Actor (sync)** ✓
- [x] Store actor pattern: dedicated thread owns Store, receives commands via `std::sync::mpsc`
- [x] StoreHandle wraps channel sender, keeps current API
- [x] Validate: CLI works as before with actor

**Phase 2: Async Runtime** ✓
- [x] Add tokio runtime (`#[tokio::main]`)
- [x] Migrate `std::sync::mpsc` → `tokio::sync::mpsc`
- [x] Async CLI using `block_in_place` for sync handlers

### Success Criteria

- [x] CLI still works as before
- [x] Store operations serialized (no data races)
- [x] Ready for concurrent network tasks

---

## Milestone 2: Two-Node Sync

**Goal:** Two nodes can sync their logs over the network.

### Deliverables

**Phase 1: Sync Logic (no network)** ✓
- [x] SyncState with AuthorInfo (seq + hash) for hash-based log resumption
- [x] `Store::sync_state()` → author-to-seq+hash map from AUTHOR_TABLE
- [x] `SyncState::diff()` → `Vec<MissingRange>` with from_hash for `read_entries_after`
- [x] Multi-store sync test: compute diff, fetch entries, apply, verify same state

**Phase 2: Iroh Integration**

*Completed:*
- [x] Node info in root store on init: `/nodes/{pubkey}/info` + `/status`
- [x] CLI: `invite <pubkey>` to authorize peers
- [x] CLI: `peers` to list known nodes (with name/added_at info, sorted)
- [x] CLI: `remove <pubkey>` to remove a peer
- [x] Iroh endpoint on startup (same Ed25519 key, mDNS + DNS discovery)
- [x] CLI: `join <nodeid>` - connects to peer, verifies invited
- [x] Peer verification via `/nodes/{pubkey}/status` check

*Join Protocol (new→existing):* ✓
- [x] Proto: `JoinRequest` / `JoinResponse` with store UUID
- [x] Accept handler sends root store UUID in response
- [x] Join command creates empty store with received UUID (no writes until sync)

*Sync Protocol (bidirectional):* ✓
- [x] Proto: `PeerMessage` wrapper with `oneof` for message type discrimination
- [x] `framing.rs` with `MessageSink`/`MessageStream` using `LengthDelimitedCodec`
- [x] Proto: `SyncRequest`/`SyncResponse` using `SyncState`
- [x] `Store::read_entries_after(hash)` to fetch log chunks
- [x] Accept handler: receive SyncState, compute diff, send missing entries
- [x] Sync command: receive entries, apply to store via `apply_entry`
- [x] CLI: `sync [nodeid]` command (syncs with all active peers if no nodeid)
- [x] After sync: node updates own `/nodes/{pubkey}/info` with hostname

*Cleanup*:
- [x] Move core logic from cmd_join and cmd_sync out of commands.rs (now in `sync.rs`)
- [x] Add 'invited' state: invite sets 'invited', peer sets 'active' after sync

*Regressions:*
- [x] Entry ordering: Per-author streaming is correct (hash chain per author, HLC for cross-author).
- [x] Multi-head sync fixed: SyncState now tracks HashSet of head hashes per author.
- [x] Sync entry ordering: Entries sent in HLC order (merge-sort across authors) to ensure causal order.
- [x] `join_mesh` doesn't populate `node.root_store`: Fixed with `complete_join` method.

### Success Criteria

- Node A writes, Node B syncs, both have same state
- Works offline-first (sync when connected)

**Post-M2 Refactoring:**
- [x] Unify `node.rs` from `lattice-cli` and `lattice-core`
- [x] Move network code to `lattice-net`

---

## Milestone 3: Multi-Node Mesh

**Goal:** N nodes form a gossip mesh for real-time sync.

### Deliverables

**Phase 1: LatticeServer Refactor** ✓
- [x] `LatticeServer` struct in `lattice-net` wrapping `Arc<Node>` + `Endpoint`
- [x] Move `join_mesh`, `sync_with_peer`, `sync_all` to `LatticeServer` methods
- [x] Encapsulate accept loop inside `LatticeServer` (via Router + ProtocolHandler)
- [x] CLI uses `LatticeServer` instead of raw `Node` + `Endpoint`

**Phase 2: Gossip Protocol** ✓ (iroh-gossip)
- [x] Router handles both `lattice-sync/1` and `/iroh-gossip/1` ALPNs
- [x] `NodeEvent::StoreReady` emitted when root store opens
- [x] Auto-join gossip topic on root store activation
- [x] Broadcast local entries to gossip topic on commit
- [x] Receive gossip entries and apply to store
- [x] Topic ID via `blake3::hash("lattice/{store_id}")`
- [x] Gossip bootstrap peers from `/nodes/` via Key Watcher

**Key Watcher (reactive store updates)** ✓
- [x] `store.watch(regex) -> (initial, Receiver<WatchEvent>)` with regex pattern matching
- [x] `WatchEvent::Put { key, value }` / `WatchEvent::Delete { key }` types
- [x] StoreActor tracks watchers, emits on matching put/delete
- [x] LatticeServer uses `/nodes/([a-f0-9]+)/status` watch to monitor peers
- [x] Enables reactive patterns: config changes, presence, app-level subscriptions

**Pre-M3: Orphan Entry Handling & Entry Ingestion**

Entries can arrive out-of-order via gossip when:
- Node wakes from offline, receives gossip before sync completes
- Gossip messages lost, creating gaps in chain
- DAG parent hashes reference entries not yet received


*Tasks:*
- [x] Fix in-memory state bug: ingest() uses SigChain.append() which updates state
- [x] Add ingest() method (validates via SigChain, returns ready entries)
- [x] StoreActor uses ingest() return value to apply entries
- [x] Add orphan buffer (orphans.db with redb)
- [x] On parent arrival: check buffer, apply any now-valid orphans
- [x] Metrics: track orphan count in store status CLI
- [ ] TTL expiry for orphans

**Phase 3: Multi-Store & Reliability** (Next)

**Goal:** Root store as control plane for declaring/managing additional stores. All stores sync independently using shared peer list.

*Phase 3A: Store Declarations in Root Store*
- [ ] Root store keys: `/stores/{uuid}/name`, `/stores/{uuid}/created_at`, `/stores/{uuid}/created_by`
- [ ] CLI: `create-store [name]` writes to root store (not local meta.db)
- [ ] CLI: `delete-store <uuid>` writes tombstone (`/stores/{uuid}/deleted_at`)
- [ ] CLI: `list-stores` reads from root store prefix scan

*Phase 3B: Store Watcher (automatic materialization)*
- [ ] Node watches `/stores/` prefix on root store
- [ ] On new store detected: create local `stores/{uuid}/` directory + state.db
- [ ] On store deleted: mark local store as archived (don't delete data)
- [ ] Startup: reconcile local stores with root store declarations

*Phase 3C: Multi-Store Gossip*
- [ ] `setup_for_store` called for each active store (not just root)
- [ ] Per-store gossip topics: `blake3("lattice/{store_id}")`
- [ ] Track which stores are actively gossiping
- [ ] Verify gossip entries have correct store-id before applying

*Phase 3D: Shared Peer List*
- [ ] All stores use root store peer list (`/nodes/` prefix)
- [ ] Peer authorization checked against root store on ingest
- [ ] Remove Phase 3C "per-store peer sets" — single trust domain

*Phase 3E: HTTP API Access Tokens*
- [ ] Root store keys: `/tokens/{id}/store_id`, `/tokens/{id}/secret_hash`, `/tokens/{id}/permissions`
- [ ] Token verification: `sha256(client_secret) == stored_hash`
- [ ] Permissions: `r` (read), `w` (write), `rw` (both)
- [ ] CLI: `create-token <store> [--name] [--expires]` → generates secret, writes hash
- [ ] CLI: `list-tokens`, `revoke-token <id>`

*Phase 3F: HTTP API Server (lattice-http crate)*
- [ ] `GET /stores/{uuid}/keys/{key}` → read value
- [ ] `PUT /stores/{uuid}/keys/{key}` → write value  
- [ ] `DELETE /stores/{uuid}/keys/{key}` → delete
- [ ] `GET /stores/{uuid}/keys?prefix=...` → list by prefix
- [ ] Auth via `Authorization: Bearer {token_id}:{secret}` header
- [ ] Rate limiting per token

*Gossip Reliability*
- [ ] Handle gossip gaps (missed updates while offline)
- [ ] Periodic anti-entropy sync to catch missed entries
- [ ] Detect stale gossip (peer hasn't sent in N seconds → trigger sync)
- [ ] Gossip ack/nack for delivery confirmation?

*Orphan Gap-Filling (backpressure)* ✓
- [x] Store orphan's seq in orphans.db value (8-byte LE + entry bytes)
- [x] Gap detection: emit `GapInfo` event via broadcast channel on orphan insert
- [x] `SigChainManager::subscribe_gaps()` for network layer to listen
- [x] `sync_author_all()`: targeted sync for specific author across all peers
- [x] Gap watcher task in LatticeServer: listens for gaps, triggers sync
- [x] Deduplication: skip duplicate gap events while sync in progress
- [x] Handle `RecvError::Lagged` gracefully (don't kill task)
- [x] Iterative orphan resolution (prevent stack overflow)
- [ ] Retry logic: if gap persists after sync attempt, retry with different peer

*Sync Resilience*
- [ ] Retry failed syncs with backoff
- [ ] Handle partial sync (peer disconnects mid-sync)
- [ ] Track sync failures per peer
- [ ] Offline nodes should not block sync (timeout + skip)

*Failure & Partition Scenarios (testing)*
- [ ] Node A writes while B offline → B rejoins → verify sync
- [ ] A-B-C chain: A↔B synced, B↔C synced, A↔C never direct → verify eventual consistency
- [ ] Network partition: A-B | C-D → partition heals → verify merge
- [ ] Node crashes mid-sync → restarts → verify no data corruption
- [ ] Gossip message loss → verify anti-entropy sync recovers
- [ ] Split-brain: A writes x=1, C writes x=2 concurrently → verify LWW resolution
- [ ] Simultaneous writes to same key across N nodes → verify convergence
- [ ] Node rejoins after long offline period → verify catch-up
- [ ] Delete during partition → verify tombstone propagates correctly
- [ ] Clock skew between nodes → verify HLC handles it

---

## Technical Debt

**Crate Refactoring: lattice-store**
- [x] Consolidate store logic into `src/store/` module with private submodules
- [x] Make store API opaque: hide internal `Entry`/`SignedEntry` from consumers
- [x] Store should not know about network; network should not know about entry internals
- [x] Added `Operation::put()`/`delete()` constructors for clean operation creation
- [ ] Extract `Store`, `SigChain`, `StoreActor` into separate `lattice-store` crate
- [ ] Define trait boundary between store and network layers:
  - `trait KvStore`: user-facing ops (`get`, `put`, `delete`, `list`, `watch`)
  - `trait SyncStore`: network ops (`ingest_entry`, `sync_state`, `read_entries_after`, `subscribe_gaps`)
  - Network layer takes `impl SyncStore` (can't call `put`)
  - CLI/app takes `impl KvStore` (can't call `ingest_entry`)
  - Zero runtime cost, compile-time enforcement
- [ ] Consider: Entry validation/signing as pluggable strategy
- [ ] Goal: `lattice-core` becomes thin orchestration layer

**Logging**
- [x] Replace `println!`/`eprintln!` with `tracing` crate (`tracing::info!`, `tracing::error!`)
- Standard in Rust async ecosystem, used by Iroh internally. CLI uses `RUST_LOG` env var.

**Lifecycle Management (Zombie Tasks)**
- [ ] Spawned infinite loops keep running if `LatticeServer` is dropped
- [ ] Use `tokio_util::sync::CancellationToken` or keep `JoinHandle`s for graceful shutdown

**Error Handling**
- [ ] Replace `Result<..., String>` with `anyhow::Result` or define `LatticeNetError` enum
- String errors make it hard to handle specific failure cases

---

## Future

- Offline nodes should not delay sync
- Sync command should transitive sync all peers
- CRDTs: Richer data types (PN-Counters, OR-Sets)
  - OR-Sets for peer list management
- Transaction Groups: Atomic batched operations
  - `EntryBuilder.operation()` already supports chaining multiple ops per entry
  - Expose via `StoreHandle::transact()` or similar API
  - Requires validating all operations succeed or rejecting entire entry atomically
  - Consider renaming `Entry` → `Transaction` to reflect semantics
- Watermark tracking & log pruning
  - Track min confirmed seq per author across all peers
  - Prune entries below watermark
- Snapshots: Fast bootstrap, optimized join, KV state transfer
- Multi-KV-Store sync
- Mobile (iOS/Android) clients
- Key rotation
- Secure storage (Keychain, TPM)
- FUSE filesystem mount
  - Note: requires u64 inodes → BiMap<u64, Hash> in redb
- Merkle-ized State
  - state.db as Merkle tree with signed root hash
  - O(1) sync checks, efficient diffing, light clients
