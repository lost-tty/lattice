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

## Milestone 1.5: DAG Conflict Resolution ← NEXT

**Goal:** Upgrade store from simple LWW to DAG-based conflict resolution per architecture.md.

### Deliverables

- [ ] Proto: Add `repeated bytes parent_hashes` to Entry (for DAG causality, separate from sigchain `prev_hash`)
- [ ] Store: keys → `Vec<u8>` (binary, not String)
- [ ] Store: KV table schema → `Vec<u8> → Vec<HeadInfo>` where `HeadInfo = { value, hlc, author, hash }`
- [ ] Store: `applied_frontiers` table → `author_id → (seq, hash)` per author
- [ ] Store: `apply_entry` → track multiple heads, merge parent tips into new tip
- [ ] Store: `get` → deterministic winner from heads (highest HLC, author_id tiebreaker)
- [ ] EntryBuilder: `.parent_hashes(...)` method for DAG ancestry

### Success Criteria

- Concurrent writes to same key create multiple heads
- Reads return deterministic winner
- Next write citing both heads merges fork to single tip
- All existing tests still pass

---

## Milestone 2: Two-Node Sync

**Goal:** Two nodes can sync their logs over the network.

### Deliverables

- [ ] VectorClock module (diff, merge, missing entries)
- [ ] Sync protocol (push missing entries)
- [ ] Iroh integration (peer discovery, connection)
- [ ] Multi-author log merging
- [ ] CLI: `peers`, `connect`/`join` commands

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
