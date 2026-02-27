---
title: "Content-Addressable Store (CAS)"
status: design
---

> **Status**: Unimplemented design (roadmap M16). Preserved for future reference.

The Lattice CAS is a high-performance, **node-local** blob storage system designed to support the "Everything is a Store" philosophy. It provides the foundational "dumb pipe" storage layer, while higher-level logic (Stores) handles replication, policy, and access control.

## Core Philosophy

1. **Dumb Pipe (`lattice-cas`)**: The CAS layer stores blobs. It does not know about peers, replication policies, or user intent. It simply answers "Do I have CID X?".
   - Used for **Fast Local Access** (Direct Read/Write).
2. **Smart Manager (`CasReplicator`)**: Handles replication, availability, and policy.
   - Used for **Network Coordination** (Fetch/Push/GC).
   - Stores ask Manager: "Ensure availability of CID X".
3. **Pull-Based Replication**: Data is pulled on demand based on specific interest (Subscriptions) or safety duties (CRUSH), driven by `StateMachine` logic.
4. **Implicit Visibility**: Data safety is inferred from State Heads (sigchain). If a node claims valid Head N, it implicitly guarantees possession of all blobs referenced by Head N.
5. **Strict Privacy**: Nodes never answer "Do you have random CID X?". All queries must be scoped to a shared `StoreId` context.

## Architecture

### Storage Backend (`FsBackend`)
- **Blob Storage**: `blobs/{shard}/{cid}` (sharded filesystem directory).
- **Metadata**: `cas.db` (Redb) stores:
  - `cid` -> `{ size, created_at, last_accessed_recent, last_accessed_frequent }`
  - Enables **ARC (Adaptive Replacement Cache)**: Tracks both recent and frequent items to resist scan pollution.
- **Hashing**: **SHA2-256** (Multihash `0x12`, raw codec).

## Trait Definitions

```rust
/// A content-addressable storage backend.
/// Designed for high concurrency and streaming I/O.
#[async_trait]
pub trait CasBackend: Send + Sync + 'static {
    async fn put(
        &self, 
        data: Pin<Box<dyn AsyncRead + Send + Unpin>>, 
        len: Option<u64>,
        store_id: Uuid
    ) -> Result<Cid, CasError>;

    async fn get(&self, cid: &Cid, store_id: Uuid) -> Result<Pin<Box<dyn AsyncRead + Send + Unpin>>, CasError>;
    async fn get_range(&self, cid: &Cid, range: std::ops::Range<u64>, store_id: Uuid) -> Result<Vec<u8>, CasError>;
    async fn exists(&self, cid: &Cid, store_id: Uuid) -> Result<bool, CasError>;
    async fn delete(&self, cid: &Cid, store_id: Uuid) -> Result<bool, CasError>;
    async fn put_batch(&self, blobs: Vec<Vec<u8>>, store_id: Uuid) -> Result<Vec<Cid>, CasError>;
    async fn list(&self) -> Result<Vec<BlobInfo>, CasError>;
    async fn used_space(&self) -> Result<u64, CasError>;
}

/// State machines expose their blob dependencies here.
pub trait StateMachine: Send + Sync {
    /// Returns a list of CIDs referenced by the current state.
    fn referenced_blobs(&self) -> Box<dyn Iterator<Item = Cid> + '_> {
        Box::new(std::iter::empty())
    }
}

/// Defines how a store or hierarchy segment should be replicated.
pub trait ReplicationPolicy {
    fn replication_factor(&self) -> u8;
    fn crush_map(&self) -> Option<CrushMap>;
}
```

## Replication Strategy

Replication is **Pull-Based**, driven by the Store's `StateMachine`.

1. **The Trigger**: Store receives data, streams to `cas.put()`, submits Op with resulting CID.
2. **The Fetch**: Reconciler checks `cas.exists(cid)`, fetches from peers if missing.
3. **Safety & Retention**: `KeepSet = DutySet ∪ InterestSet`.
   - **Duty (Safety)**: `CRUSH(cid)` selects this node. Must store it.
   - **Interest (Pins)**: `referenced_blobs()` includes `cid`. Wants to store it.
   - **Over-Redundancy (Soft Handoff)**: If no longer a duty holder, degrade to cached status.

## Integration Patterns

### Store Operations
Stores process data, CAS stores it.
- **Store Logic**: Handles Chunking, Hashing, Compression, Encryption.
- **Host Functions**: Node exposes native algorithms (SHA2, Zstd, AES) to Wasm stores.
- **CAS**: Pure storage. Does *not* compress or encrypt.

### Encryption (Privacy)
Encryption is a **Store-Level Responsibility**.
- **Public Data**: Plaintext.
- **Private Data**: Store encrypts *before* `cas.put()`.

### FUSE Integration
Mapping POSIX filesystem to CAS:
- **Read**: `read(inode, offset, len)` -> `cas.get_range(chunk_cid, offset, len)`.
- **Write**: Buffers locally, then on `fsync`: chunks -> `cas.put_batch()` -> `Op::UpdateInode`.

### Wasm Integration (Host Handles)
Avoid passing large files through Wasm memory:
1. **RPC**: Node buffers stream to disk/temp.
2. **Handle**: Node passes integer `handle_id` to Wasm.
3. **Host Functions**: Wasm calls `host.cas_put(handle)` or `host.chunk_file(handle)`.

## Security & Privacy

Lattice enforces strict privacy to prevent "Known-CID" attacks.

### Scoped Queries
Queries must be scoped: "In context of **Store Y**, do you have file X?". Global queries are forbidden.

### Capability-Based Access
Requester must prove:
1. **Membership** (Signed Capability).
2. **Relevance** (Merkle Proof showing file X is in Store Y).
