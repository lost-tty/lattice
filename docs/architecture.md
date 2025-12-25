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

### Store/Network Boundary

The store module exposes only `StoreHandle` to the network layer. Internal types (`Store`, `SigChain`, `StoreActor`, `Entry`) are hidden.

```
lattice-net                           lattice-core
┌─────────────────┐                  ┌───────────────────┐
│  LatticeServer  │───────────────── │      Node         │
│  (owns Endpoint)│                  │  (owns stores)    │
└────────┬────────┘                  └─────────┬─────────┘
         │                                     │
         │ uses StoreHandle API                │ spawns
         ▼                                     ▼
  ┌─────────────────────────────────────────────────────────┐
  │                    StoreHandle                          │
  │  • put(key, value), get(key), delete(key)               │
  │  • subscribe_entries() → for gossip broadcast           │
  │  • apply_entry(SignedEntry) → for receiving gossip/sync │
  │  • read_missing_entries(missing_ranges) → for sync send │
  │  • sync_state() → for sync negotiation                  │
  └─────────────────────────────────────────────────────────┘
```

## Parts

### Nodes

- Identified by their Ed25519 public key.
- Private key stored locally in `identity.key` (not replicated).
- Node data stored in KV:
  - `/nodes/{pubkey}/name` = display name
  - `/nodes/{pubkey}/added_at` = timestamp when added
  - `/nodes/{pubkey}/status` = `invited` | `active` | `dormant` (removal deletes keys)
  - `/nodes/{pubkey}/role` = `server` | `device` (optional, hints sync priority)
- Peer invitation flow:
  1. Inviter runs `invite <peer_pubkey>` → writes `/nodes/{peer}/info` + `/status`
  2. Inviter shares their Iroh NodeId out-of-band (QR code, link, text)
  3. Invited peer runs `join <inviter_nodeid>` → syncs with inviter
  4. Sync pulls `/nodes/{self}/info` + `/status` → peer is authorized
  5. `connect` implicitly adds inviter to peer's `/nodes/*` (mutual awareness)
- Accepting = syncing. The invited peer discovers authorization by receiving the entries.
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
AUTHOR_TABLE      [u8; 32] (author_id)    (u64 seq, [u8; 32] hash)   Per-author frontier tracking
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

### Multi-Store Architecture

The root store acts as the **control plane** for all stores in the mesh. Additional stores are declared in root store and automatically created/removed on all nodes.

**Store Lifecycle:**

1. **Declaration**: Any node writes `/stores/{uuid}/...` entries to root store
2. **Propagation**: Changes sync to all peers via normal gossip/sync
3. **Materialization**: Each node watches `/stores/` prefix, creates/deletes local stores
4. **Sync**: Each store syncs independently using its own gossip topic

**Root Store Keys for Stores:**

```
/stores/{uuid}/name      = "My App Data"           # Optional display name
/stores/{uuid}/created_at = 1703548800000          # HLC timestamp
/stores/{uuid}/created_by = {author_pubkey}        # Creator's key
/stores/{uuid}/deleted_at = ...                    # Soft-delete (tombstone)
```

**Shared Peer Model:**

All stores inherit the peer list from root store (`/nodes/` prefix). This simplifies:
- No duplicate peer management per store
- Single trust domain per mesh
- Peer authorization checked against root store on entry ingest

```
Root Store                     Side Stores
┌─────────────────┐           ┌─────────────────┐
│ /nodes/abc/...  │           │ app data        │
│ /nodes/def/...  │──────────▶│ (any keys)      │
│ /stores/xxx/... │  peers    │                 │
└─────────────────┘           └─────────────────┘
                              ┌─────────────────┐
                              │ another store   │
                              │                 │
                              └─────────────────┘
```

**HTTP API Access Tokens:**

Stores can be exposed over HTTP API using token-based authentication. Tokens are declared in root store with a secret hash (clients provide secret, server verifies).

```
/tokens/{token_id}/store_id   = {store_uuid}       # Which store this token accesses
/tokens/{token_id}/secret_hash = {sha256(secret)}  # Hashed secret for verification
/tokens/{token_id}/name        = "Mobile Client"   # Optional description
/tokens/{token_id}/created_at  = ...
/tokens/{token_id}/expires_at  = ...               # Optional expiry (0 = no expiry)
/tokens/{token_id}/permissions = "rw"              # r=read, w=write, rw=both
```

**Token Flow:**

1. Admin generates secret locally: `secret = random_bytes(32)`
2. Admin writes token to root store: `secret_hash = sha256(secret)`
3. Admin shares secret out-of-band (QR code, secure channel)
4. Client calls HTTP API with `Authorization: Bearer {token_id}:{secret}`
5. Server verifies `sha256(secret) == stored_hash`, checks permissions
6. To revoke: delete `/tokens/{token_id}/*` or set `expires_at` in past

**Security Notes:**

- Secrets never stored in replicated state (only hashes)
- Token revocation propagates via normal sync
- Compromised token can be revoked from any node
- Consider: rate limiting per token, audit logging

### Operation Flow (put/delete)

```
1. User calls StoreHandle::put(key, value)
         │
         ▼
2. StoreActor (internal) → SigChain.create_entry()
   - Build Entry with parent_hashes (current tips for key)
   - Use Operation::put(key, value) to add ops
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

Future:
- Capability-based permissions: Explore finer-grained write access (e.g., per-key or per-prefix permissions) via capabilities. Exact mechanism TBD.

### Peer Authorization During Replay

*Key distinction:*
- Signature verification: Is Ed25519 signature valid? (always verifiable)
- Peer authorization: Was this public key allowed to write at this time?

*During replay, need to answer:* "Was author X authorized when they wrote entry Y?"

*Options for cross-store authorization (side-store using root store peer list):*

1. **Self-contained stores** - Each store copies peer list at creation, manages its own
   - Clean separation, but duplicates peer management

2. **HLC-based verification** - Use HLC timestamps:
   - Root store peer changes have HLC, side-store entries have HLC
   - Check: was author in root store peer list at entry's HLC?
   - Requires root store to keep full history

3. **Trust-on-first-sync** - Accept entries from trusted peer during sync
   - "If Alice (trusted) gave me this entry, entry is valid"
   - Pragmatic but less rigorous

4. **Root store audit log** - Never purge peer history:
   - `/peers/{pubkey}/added_at = HLC`, `/peers/{pubkey}/removed_at = HLC`
   - Can reconstruct historical peer state at any point
   - Recommended for rigorous verification

*Current behavior:* Signature verified during ingest, peer status checked on network receive.