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

**Phase 1: LatticeServer Refactor**
- [x] `LatticeServer` struct in `lattice-net` wrapping `Arc<Node>` + `Endpoint`
- [x] Move `join_mesh`, `sync_with_peer`, `sync_all` to `LatticeServer` methods
- [x] Encapsulate `spawn_accept_loop` inside `LatticeServer`
- [x] CLI uses `LatticeServer` instead of raw `Node` + `Endpoint`
- [ ] Route sync command through `LatticeServer` (not raw functions)
- [ ] Integration test: invite → join → sync end-to-end
- [ ] Periodic background sync with known peers
- [ ] Track last sync time per peer

**Phase 2: Gossip Protocol**
- [ ] Proto: `GossipAnnounce` message with author + latest seq + HLC
- [ ] `LatticeServer::spawn_gossip_loop` for periodic announcements
- [ ] On receiving announce: detect missing entries, trigger sync
- [ ] Track last-seen per peer for staleness detection

---

## Future

- Gossip:
  - gossip new entries to peers
  - backfill missing entries from peers (how do peers notice missing entries?)
  - snapshots for kv store
  - prune using consensus watermark
- remove_peer should be a transactional operation on store
- Watermark tracking & log pruning
  - Track minimum confirmed seq per author across all peers
  - Log pruning: remove entries below watermark
- Multi-KV-Store sync
- Optimized sync on join. Only transfer current watermark state, then sync missing entries. This would allow pruning. Might need snapshot support in KV store.
- Mobile (iOS/Android) clients
- Key rotation
- Secure storage (Keychain, TPM)
- Snapshots for fast bootstrap
- FUSE filesystem mount
  - Note: FUSE requires u64 inode numbers → maintain `BiMap<u64, Hash>` in redb
- Merkle-ized State
  - state.db as Merkle tree with signed root hash
  - O(1) sync checks (compare root), efficient binary-search diffing
  - Light clients: fetch value + Merkle proof, verify without full state
  - Trade-off: write amplification, requires deterministic tree (Patricia Trie / Merkle Search Tree)
