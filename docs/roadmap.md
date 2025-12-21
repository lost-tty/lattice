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
- [ ] Interactive CLI: `init`, `put`, `get`, `delete`, `status`, `quit`

### Success Criteria

- Can create a new identity
- Can append entries to local log
- Can replay log to reconstruct KV state
- All operations survive restart

### Multi-KV Refactoring (before M2)

Current code assumes single store. Changes needed:
- [ ] DataDir → support `stores/{uuid}/` subdirectories
- [ ] SigChain → scoped to (store_id, author_id)
- [ ] Store → per-store state.db, not global
- [ ] Log paths → `stores/{uuid}/logs/{author}.log`
- [ ] Add global meta.db for stores table
- [ ] Proto: SignedEntry/messages need store_id (UUID)
- [ ] Proto: Entry needs `parent_hashes` for DAG (not just `prev_hash`)
- [ ] Store: keys/values → `Vec<u8>` (binary, not String)
- [ ] Store: KV table value → `Vec<HeadInfo>` for DAG heads
- [ ] CLI → `create-store`, `list-stores`, `use <store>`

---

## Milestone 2: Two-Node Sync

**Goal:** Two nodes can sync their logs over the network.

### Deliverables

- [ ] Store: add `applied_frontiers` table (sync state per author)
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
