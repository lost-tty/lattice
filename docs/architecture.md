# Architecture

## Ideas

**Core:**
- SigChains: Ed25519-signed, hash-chained append-only logs per node.
- Offline-First: Iroh for networking. Vector clocks identify missing entries on reconnect.
- Full Replication: All nodes keep all logs until watermark consensus, then prune.

**State:**
- Log-Based State: KV store derived from entries. Watermarks enable pruning + snapshots.
- Merkle-ized State: state.db as Merkle tree. O(1) sync checks, efficient diffing, light clients.
- DAG Conflict Resolution: Entries track ancestry. Forks merge on next write. Tips only in state.db.
- KV Snapshots: Point-in-time snapshots for log pruning, fast bootstrap, time travel.

**Operations:**
- Atomic Batch Writes: Multiple key updates as single entry.
- Conditional Updates (CAS): Update only if current value matches expected hash.

**CRDTs:**
- LWW-Register: Last-writer-wins for single values.
- LWW-Element-Set: Set with add/remove, element present if add > remove timestamp.

## Concepts

- Transitive Pairing: Nodes can introduce new nodes to the mesh.
- Multi-Mesh: A node can participate in multiple meshes (clusters). Each mesh is a group of nodes sharing data.
- Manifest Store: Joining a mesh means joining a special KV store of type "manifest" that defines the mesh membership. The manifest contains node info (`/nodes/{pubkey}/...`).

## Stack

- rust
- iroh
- prost protocol buffers
- redb (embedded KV store)
- rustyline (interactive CLI)

### Bootstrap

- New peers request a full state snapshot from their first connection.
- The snapshot allows them to skip replaying the entire log history.
- After bootstrap, the node receives incremental updates via gossip.

### Networking

- Designed for mobile clients that may only sync a few times per day.
- When peers connect, they exchange vector clocks to identify missing entries.
- Missing entries are fetched via unicast.
- MAX_DRIFT should be generous (e.g., hours) to accommodate sleeping devices.

Networking modes:
- Active (servers/laptops on power): Frequent gossip broadcasts, proactive sync.
- Low-power (mobile/battery): Pull-based sync on wake. Query peers instead of relying on push gossip.

## Parts

### Nodes

- Identified by their Ed25519 public key.
- Private key stored locally in `identity.key` (not replicated).
- Node data stored in KV:
  - `/nodes/{pubkey}/info` = static metadata (name, added_by, added_at)
  - `/nodes/{pubkey}/status` = `active` | `dormant` | `disabled`
  - `/nodes/{pubkey}/role` = `server` | `device` (optional, hints sync priority)
- Inviting a node = writing entries to `/nodes/{pubkey}/...`.
- Liveness: Each node tracks `last_seen` locally (from watermark gossip). UI alerts if a peer hasn't been seen for threshold (e.g., 30 days). User decides to mark dormant/disabled.
- Status effects:
  - `active`: Normal sync participant, blocks watermark until acknowledged.
  - `dormant`: Excluded from watermark consensus, can be reactivated.
  - `disabled`: Permanently removed from mesh.
- Sync priority: Low-power clients prefer peers marked as `server` or recently active.

Future:
- Key rotation: Allow nodes to rotate their keypair. Old key signs a "rotation" entry pointing to new key.
- Secure storage: Support platform keystores (macOS Keychain, Linux Secret Service, TPM) for private key protection.

### Data Model

- Multiple KV stores supported, identified by `store_id` (UUID).
- Keys: Arbitrary byte arrays (`Vec<u8>`), sorted lexicographically.
- Values: Arbitrary byte arrays (`Vec<u8>`).
- Each store defines its own key/value format — applications know their schema.
- Logs are per `(store_id, author_id)` tuple.
- State is maintained by tracking the "frontier" (tips) of the causal graph for each key.
- Entry ordering: by HLC timestamp, then by author ID as tiebreaker.

**Sync vs Causality:**
- Vector Clocks track log coverage ("I have entries from Node A up to seq 50") — syncing files.
- DAG Parents track data causality ("This value replaces that value") — resolving key conflicts.

#### DAG Conflict Resolution

Instead of simple LWW where newest timestamp blindly overwrites, every entry tracks its ancestry:

**Data Model:**
- Each entry includes `parent_hashes` — references to the entries it supersedes
- History forms a DAG (directed acyclic graph), not a linear chain
- state.db stores only "tips" (heads) of the graph per key

**Life Cycle:**

1. **Write (normal):** New entry points to previous entry's hash as parent. History is a straight line.

2. **Write (concurrent/offline):** Two nodes edit same key independently, both pointing to same old parent. History forks into two branches.

3. **Read (forked):** System sees multiple valid values. Uses deterministic rule (highest HLC, then author_id tiebreaker) to return one "winner". No error thrown.

4. **Merge (healing):** Next write to that key cites both existing branches as parents. Fork merges back to single tip.

**Example: Partial Write (Branch Extension)**

```
Initial: Heads = {A, B} where A(ts:100), B(ts:105). Read winner = B.

Offline node C wakes up, only knows A (hasn't seen B).
C writes "v3" with parent = [A].

Result: Heads = {C, B}. Conflict shifted, not resolved.
        C(ts:110) > B(ts:105), so C wins reads.

       ┌──> [A] ──> [C:110]
[Root]─┤
       └──> [B:105]

Later: A synced node writes D with parents = [C, B].
Result: Heads = {D}. Fork merged.
```

This preserves B's work even though C never saw it. Naive LWW would lose B forever.

#### Store Consistency Modes

- **Eventually consistent**: Default. Writes accepted locally, sync happens async. Fast, offline-capable.
- **Strictly consistent**: Writes require quorum acknowledgment before commit. Slower, requires connectivity.

### Timestamps (Hybrid Logical Clocks)

Timestamps use HLC `<wall_time, counter>` with Causal Clamping:

- Each entry includes an HLC and a reference to its parent (prev_hash).
- Standard HLC: `new_hlc = max(local_wall_clock, max_seen_hlc + 1)`.
- On receive: if `entry.hlc > local_wall_clock + MAX_DRIFT`, clamp to `parent.hlc + 1`.
- All nodes compute the same clamped time from the parent (deterministic).
- Genesis entries (no parent) with future timestamps are dropped.

Pre-flight check (before signing):
- Compare local_clock to max_peer_hlc (from recent gossip/entries).
- If `local_clock > max_peer_hlc + MAX_DRIFT`, use `max_peer_hlc + 1` instead.
- This catches future-clock nodes before they poison the log.

Authors apply their own entries through the standard receive path to ensure consistent clamping.

### Storage

Each node stores logs as one file per author:

```
~/.local/share/lattice/
├── identity.key                            # Ed25519 private key
├── stores/
│   └── {store_uuid}/
│       ├── logs/
│       │   └── {author_id_hex}.log         # Append-only SignedEntry stream
│       └── state.db                        # redb: KV snapshot + frontiers
└── meta.db                                 # redb: global metadata (known stores, peers)
```

- Logs: Append-only binary files per `(store, author)`, containing serialized `SignedEntry` messages.
- State DB (redb): Per-store KV state and frontiers. Updated as entries are applied.

#### state.db Tables (per store, redb)

```
Table              Key                     Value                      Purpose
─────────────────────────────────────────────────────────────────────────────
kv                 Vec<u8> (key)           Vec<HeadInfo>              Current tips for each key
applied_frontiers  [u8; 32] (author_id)    (u64 seq, [u8; 32] hash)   What's applied to this store
meta               Vec<u8>                 Vec<u8>                    Store metadata (incl. merkle_root)
```

`HeadInfo: { value: Vec<u8>, hlc: u64, author: [u8;32], hash: [u8;32] }`

Note: KV stores multiple heads per key to support DAG conflict resolution. Reads pick winner deterministically.

#### meta.db Tables (global, redb)

```
Table              Key                     Value                      Purpose
─────────────────────────────────────────────────────────────────────────────
stores             [u8; 16] (UUID)         u64 (created_at_ms)        Known stores
meta               "root_store"            [u8; 16] (UUID)            Root store ID (opened on startup)
```

- **Root Store**: The primary/manifest store for this node, auto-opened on CLI startup
- **Stores Table**: Tracks all stores this node participates in
- Manifest stores define mesh membership via `/nodes/{pubkey}/...` entries
- Data stores hold application data

#### In-Memory Structures

- log_frontiers: `HashMap<AuthorId, (seq, hash)>` — rebuilt from log files on startup

### Operation Flow (put/delete)

```
1. User calls put("/key", value)
         │
         ▼
2. SigChain.create_entry()
   - Build Entry with parent_hashes (current tips for key)
   - Sign it → SignedEntry
         │
         ▼
3. Append to log + Gossip (critical path)
   - Write to author's log file
   - Update log_frontiers (in-memory)
   - Broadcast to peers
         │
         ▼
4. Apply to state.db (background)
   - Update kv heads (merge parent tips into new tip)
   - Update applied_frontiers
   - Update merkle_root hash
```

Fast path (1-3): durable + distributed. Background (4): queryable state.

### Read Flow (get)

`get(key)` reads directly from local state.db. Reads are eventually consistent — if state.db lags behind the log, the read may return slightly stale data.

### Watermarks

- Nodes gossip their watermarks periodically (throttled).
- A watermark is a vector clock: how much of each author's log the node has seen.
- All nodes keep all logs (own + others) for redundancy until watermark consensus.
- Once all peers have acknowledged entries, they can be pruned and replaced by the snapshot.
- If a node is offline too long, it re-bootstraps with a fresh snapshot when it reconnects.
- Note: Consider preserving logs longer than required for redundancy — enables time travel (view state at any point in history).

**Pruning and DAG Parents:**
- If a new entry references a parent that was pruned, accept it only if strictly newer than snapshot timestamp.
- Snapshots act as the base; entries referencing parents older than snapshot are roots relative to that snapshot.

### Rich CRDTs (Future)

Instead of a generic scripting language, use specific data types that merge better than LWW.

Extend value types in redb:

```rust
enum ReplicatedValue {
    LWW(Vec<u8>),          // Standard Last-Write-Wins (current model)
    Counter(i64),          // PN-Counter (Increment/Decrement)
    Set(HashSet<Vec<u8>>), // OR-Set (Observed-Remove Set)
}
```

**Counter** (for "storage used" etc.):
- State is `{node_id: value}` map. Merge = sum all nodes. No conflicts possible.

**OR-Set** (for group membership etc.):
- Merge = union. Element present if add timestamp > remove timestamp.

**Op Code Compromise**: Use commutative operations instead of a VM:

```protobuf
message Entry {
  oneof operation {
    PutOp put = 1;
    DeleteOp delete = 2;
    MergeOp merge = 3;
  }
}

message MergeOp {
  string key = 1;
  oneof payload {
    int64 counter_delta = 2;
    bytes set_add_member = 3;
    bytes set_remove_member = 4;
  }
}
```

Recommendation: Use Put/Delete for 90% of data. Add CRDT primitives only when needed (concurrent counters, lists) rather than a scripting language.

## Open Questions

### Permissions

Write permissions are enforceable cryptographically:
- Every entry is signed by author
- Nodes verify signature before accepting
- Manifest defines allowed writers: `/nodes/{pubkey}/role` = `writer` | `reader`
- Entries from non-writers are rejected

Read permissions are not enforceable:
- Sharing a store = granting read access
- Encryption adds a layer but doesn't solve revocation (once you have the key, you can read past data)
- True revocation is impossible — you can't "unread" data

Practical model:
- Share store = grant read
- Write access defined in manifest
- Read-only nodes replicate and verify but can't contribute entries