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
- [x] Node info in root store on init: `/nodes/{pubkey}/info` + `/status`
- [ ] Iroh integration (peer discovery, connection)
- [ ] Sync protocol (push missing entries over network)
- [ ] CLI: `peers`, `connect`/`join` commands
- [ ] Background sync task (tokio::spawn)

### Success Criteria

- Node A writes, Node B syncs, both have same state
- Works offline-first (sync when connected)

---

## Milestone 3: Multi-Node Mesh

**Goal:** N nodes form a gossip mesh with watermark consensus.

### Deliverables

- [ ] Gossip protocol
- [ ] Watermark tracking & log pruning
- [ ] Node invitation (sigchain membership)
- [ ] Conflict detection (LWW resolution)

---

## Future

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
