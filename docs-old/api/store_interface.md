# Store API Reference

The `Store` provides the generic replication interface, while `KvHandle` (and `KvOps`) provides the Key-Value specific API.

## Architecture

```
Store/KvHandle → ReplicationController → { Intention DAG (log), StateMachine (heads) }
```

**Heads-Only:** All read methods return `Vec<Head>`. Use `Merge` trait for resolution.

---

## Merge Strategies

```rust
use lattice_core::Merge;

// Single key
let value = handle.get(key).await?.lww();       // Option<Vec<u8>>
let head = handle.get(key).await?.lww_head();   // Option<&Head>
let all = handle.get(key).await?.all();         // Vec<Vec<u8>>

// Lists
use lattice_core::MergeList;
let intentions = handle.list().await?.lww();       // Vec<(Key, Value)>
```

Strategies: `.lww()` (newest wins), `.fww()` (oldest wins), `.all()` (sorted by HLC)

---

## CRUD

```rust
get(key: &[u8]) -> Vec<Head>    // All heads for key (empty if not found)
put(key: &[u8], value: &[u8])   // Write value
delete(key: &[u8])              // Delete key
```

---

## List

```rust
list() -> Vec<(Key, Vec<Head>)>
list_by_prefix(prefix: &[u8]) -> Vec<(Key, Vec<Head>)>
list_by_regex(pattern: &str) -> Vec<(Key, Vec<Head>)>
```

---

## Watch

```rust
watch(pattern: &str) -> (snapshot, Receiver<WatchEvent>)
watch_by_prefix(prefix: &[u8]) -> (snapshot, Receiver<WatchEvent>)

// Snapshot is Vec<(Key, Vec<Head>)>
// WatchEvent contains key and heads, apply merge as needed
```

---

## Subscriptions

```rust
subscribe_entries() -> Receiver<SignedIntention>
subscribe_sync_needed() -> Receiver<SyncNeeded>
subscribe_gaps() -> Receiver<GapInfo>
```

---

## Sync

```rust
sync_state() -> SyncState
set_peer_sync_state(peer, info) -> SyncDiscrepancy
get_peer_sync_state(peer) -> Option<PeerSyncInfo>
list_peer_sync_states() -> Vec<(PubKey, PeerSyncInfo)>
ingest_intention(intention) -> Result<()>
stream_entries_in_range(author, from, to) -> Receiver<SignedIntention>
```

---

## Chain & Diagnostics

```rust
chain_tip(author) -> Option<ChainTip>
log_seq() -> u64
applied_seq() -> u64
log_stats() -> (file_count, bytes, orphan_count)
orphan_list() -> Vec<OrphanInfo>
orphan_cleanup() -> usize
```
