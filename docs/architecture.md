# Architecture

## Ideas

- SigChains Ed25519-signed, hash-chained append-only logs per node. Trust via local signature verification.
- Log-Based State: KV store derived by replaying entries. Watermarks enable safe log pruning + snapshots.
- Offline-First: Iroh for networking. Vector clocks identify missing entries on reconnect—converges mathematically.
- Full Replication: All nodes keep all logs until watermark consensus, then prune and snapshot.

## Concepts

- Transitive Pairing. Nodes can introduce new nodes to the mesh.

## Stack

- rust
- iroh
- prost protocol buffers

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

- Keys are flat strings using path conventions (e.g., `/nodes/{pubkey}`, `/config/sync/interval`).
- Prefix queries via string matching (sorted map enables efficient range scans).
- State is computed by replaying `Put`/`Delete` operations from all authors.
- Entry ordering: by HLC timestamp, then by author ID as tiebreaker.
- Conflicts resolved by last-write-wins (using the ordering above).

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
data/
├── identity.key               # Local node's Ed25519 private key
├── logs/
│   └── {author_id_hex}.log    # Append-only SignedEntry stream per author
└── state.db                   # redb: KV snapshot + vector clocks + indexes
```

- Logs: Append-only binary files per author, containing serialized `SignedEntry` messages.
- State DB (redb): Combined KV state, vector clocks, and indexes. Updated as entries are applied.

#### state.db Tables (redb)

```
Table           Key                     Value                      Purpose
─────────────────────────────────────────────────────────────────────────────
kv              String (path)           Vec<u8>                    Replicated key-value data
vector_clocks   [u8; 32] (author_id)    (u64 seq, [u8; 32] hash)   Track sync state + chain verification
entry_index     (author_id, seq)        u64 (offset)               Fast entry lookup by position
meta            String                  Vec<u8>                    System metadata (own_seq, watermark, etc.)
```

### Watermarks

- Nodes gossip their watermarks periodically (throttled).
- A watermark is a vector clock: how much of each author's log the node has seen.
- All nodes keep all logs (own + others) for redundancy until watermark consensus.
- Once all peers have acknowledged entries, they can be pruned and replaced by the snapshot.
- If a node is offline too long, it re-bootstraps with a fresh snapshot when it reconnects.

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
