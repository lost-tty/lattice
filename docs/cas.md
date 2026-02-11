# Lattice Content-Addressable Store (CAS)

The Lattice CAS is a high-performance, **node-local** blob storage system designed to support the "Everything is a Store" philosophy. It provides the foundational "dumb pipe" storage layer, while higher-level logic (Stores) handles replication, policy, and access control.

## Core Philosophy

1.  **Dumb Pipe (`lattice-cas`)**: The CAS layer stores blobs. It does not know about peers, replication policies, or user intent. It simply answers "Do I have CID X?".
    -   Used for **Fast Local Access** (Direct Read/Write).
2.  **Smart Manager (`CasReplicator`)**: Handles replication, availability, and policy.
    -   Used for **Network Coordination** (Fetch/Push/GC).
    -   Stores ask Manager: "Ensure availability of CID X".
3.  **Pull-Based Replication**: Data is pulled on demand based on specific interest (Subscriptions) or safety duties (CRUSH), driven by `StateMachine` logic.
4.  **Implicit Visibility**: Data safety is inferred from State Heads (sigchain). If a node claims valid Head N, it implicitly guarantees possession of all blobs referenced by Head N.
4.  **Strict Privacy**: Nodes never answer "Do you have random CID X?". All queries must be scoped to a shared `StoreId` context.

## Architecture (M11A)

### Storage Backend (`FsBackend`)
-   **Blob Storage**: `blobs/{shard}/{cid}` (sharded filesystem directory).
-   **Metadata**: `cas.db` (Redb) stores:
    -   `cid` -> `{ size, created_at, last_accessed_recent, last_accessed_frequent }`
    -   Enables **ARC (Adaptive Replacement Cache)**: Tracks both recent and frequent items to resist scan pollution.
-   **Hashing**: **SHA2-256** (Multihash `0x12`, raw codec).

## Trait Definitions

```rust
/// A content-addressable storage backend.
/// Designed for high concurrency and streaming I/O.
#[async_trait]
pub trait CasBackend: Send + Sync + 'static {
    /// Store a blob. Returns the computed CID.
    /// - `data`: Async reader for streaming large files.
    /// - `len`: Optional size hint for pre-allocation.
    /// - `store_id`: The store context (enforces isolation).
    async fn put(
        &self, 
        data: Pin<Box<dyn AsyncRead + Send + Unpin>>, 
        len: Option<u64>,
        store_id: Uuid
    ) -> Result<Cid, CasError>;

    /// Retrieve a blob.
    /// - `store_id`: Must be authorized to access this CID.
    async fn get(&self, cid: &Cid, store_id: Uuid) -> Result<Pin<Box<dyn AsyncRead + Send + Unpin>>, CasError>;

    /// Retrieve a byte range (for FUSE/Streaming).
    async fn get_range(&self, cid: &Cid, range: std::ops::Range<u64>, store_id: Uuid) -> Result<Vec<u8>, CasError>;

    /// Check if a blob exists (fast metadata lookup).
    async fn exists(&self, cid: &Cid, store_id: Uuid) -> Result<bool, CasError>;

    /// Delete a blob reference for this store.
    async fn delete(&self, cid: &Cid, store_id: Uuid) -> Result<bool, CasError>;
    
    /// Batch store blobs (atomic/efficient for FUSE writes).
    /// Returns list of CIDs in order.
    async fn put_batch(&self, blobs: Vec<Vec<u8>>, store_id: Uuid) -> Result<Vec<Cid>, CasError>;

    /// List all blobs with metadata (from Redb index).
    /// Used for audits and GC analysis.
    async fn list(&self) -> Result<Vec<BlobInfo>, CasError>;

    /// Get total storage usage in bytes.
    async fn used_space(&self) -> Result<u64, CasError>;
}

pub struct BlobInfo {
    pub cid: Cid,
    pub size: u64,
    pub created_at: u64,
    pub last_accessed: u64,
    pub access_count: u64, // Support for LFU/ARC
    pub ref_count: u32,    // Physical Reference Count (0 = delete)
}

/// State machines expose their blob dependencies here.
pub trait StateMachine: Send + Sync {
    // ... existing methods ...

    /// Returns a list of CIDs referenced by the current state.
    /// Used for:
    /// - GC: Any blob not returned here (and not pinned) is garbage.
    /// - Sync: Peers need these CIDs to fully replicate this state.
    fn referenced_blobs(&self) -> Box<dyn Iterator<Item = Cid> + '_> {
        Box::new(std::iter::empty())
    }
}

// Future Extensions (M11B+):
// - fn blob_priority(&self, cid: &Cid) -> Priority; // High (User Pin) vs Low (prefetch)
// - fn on_blob_loaded(&self, cid: &Cid); // Trigger indexing or UI update

/// Defines how a store or hierarchy segment should be replicated.
/// Implemented by the System Store (TABLE_SYSTEM wrapper).
pub trait ReplicationPolicy {
    /// Target redundancy count (e.g. 3). Inherited from parent if not set.
    fn replication_factor(&self) -> u8;
    
    /// The CRUSH map defining the failure domains and weights.
    /// Inherited from parent (Root Store usually defines this).
    fn crush_map(&self) -> Option<CrushMap>;
}
```

### Integration
-   **Node Ownership**: The `Node` struct owns an `Arc<dyn CasBackend>`.
-   **Availability**: Exposed internally to `StoreManager` and `StateMachine`.

---

## Replication Strategy (M11B)

Replication is **Pull-Based**, driven by the Store's `StateMachine`.

### 1. The Trigger
-   **Metadata Sync**: Stores sync lightweight metadata (Ops) instantly via SigChain.
-   **Data Ingestion**: Handled by **Store Logic** (Native or WASM).
    -   Store receives data (e.g. `ProfileStore::set_avatar(stream)`).
    -   Store streams to `cas.put()`.
    -   Store submits Op with resulting CID.
-   **Op Processing**: When a node receives `Op::AddFile(cid)`:
    -   The `StateMachine` updates its state.
    -   The referenced `cid` becomes "meaningful" to the node.

### 2. The Fetch (Reconciler)
-   **Check**: The local Reconciler loop asks `cas.exists(cid)`.
-   **Action**: If missing, it initiates a fetch from peers **active in that specific store**.
-   **Discovery**: Uses the store's existing peer list (no global DHT lookup needed for private data).

### 3. Safety & Retention
Retention is the **Union** of Duty and Interest: `KeepSet = DutySet ∪ InterestSet`.

#### CasManager Interface (Draft)
The `CasManager` orchestrates retention and availability.

```rust
pub trait CasManager: Send + Sync {
    /// Ensure a blob is available locally (Fetch if needed).
    /// Used by Stores when they encounter a missing blob.
    async fn ensure(&self, cid: &Cid, store_id: Uuid) -> Result<(), CasError>;

    /// Calculate replication duties based on CRUSH map.
    /// Returns list of CIDs this node MUST store for safety.
    async fn calculate_duties(&self, crush_map: &CrushMap) -> Result<Vec<Cid>, CasError>;

    /// Trigger Garbage Collection.
    /// Deletes blobs referenced by neither Duty nor Interest.
    async fn gc(&self) -> Result<u64, CasError>;
}
```

-   **Duty (Safety)**: `CRUSH(cid)` selects this node. The node *must* store it to satisfy redundancy goals.
-   **Interest (Pins)**: `referenced_blobs()` includes `cid`. The node *wants* to store it (for local use/speed).
-   **Over-Redundancy (Soft Handoff)**: If a node is no longer a Duty holder (e.g. map change), it does NOT delete immediately.
    -   Data degrades to **Cached** status (Candidate for Eviction).
    -   If space allows, it stays (providing extra safety/speed).
    -   If space is needed, ARC eviction removes it.
-   **GC Policy**:
    -   If `cid` in Duty/Interest: PROTECT.
    -   If `cid` in Over-Redundance: CACHE (Subject to ARC).
    -   Else: DELETE.

### 5. Integration Patterns

#### 5.1 Store Operations (Logic vs Storage)
Stores process data, CAS stores it.
-   **Store Logic**: Handles Chunking, Hashing, Compression, Encryption.
-   **Host Functions**: Node exposes native algorithms (SHA2, Zstd, AES) to Wasm stores.
-   **CAS**: Pure storage. It does *not* compress or encrypt.

#### 5.2 Encryption (Privacy)
Encryption is a **Store-Level Responsibility**.
-   **Public Data**: Plaintext.
-   **Private Data**: Store encrypts *before* `cas.put()`.
    -   CAS stores opaque ciphertext.
    -   *Trade-off*: Encrypted blobs (different keys) do not deduplicate.

#### 5.3 Advanced Access (Direct CAS)
For fine-grained control (e.g., ZFS, Database):
1.  **Chunking**: Store breaks data into blocks (e.g. 4KB).
2.  **Direct Write**: Calls `CasRpc::Put(Hash, Data)`.
3.  **State Update**: Store updates internal Merkle Tree.
4.  **Commit**: Submits `Op::UpdateRoot(NewTreeRoot)`.

#### 5.4 FUSE Integration
Mapping POSIX filesystem to CAS:
-   **Read**: `read(inode, offset, len)` -> `cas.get_range(chunk_cid, offset, len)`.
-   **Write**: Buffers locally, then on `fsync`: chunks -> `cas.put_batch()` -> `Op::UpdateInode`.

#### 5.5 Wasm Integration (Host Handles)
Avoid passing large files through Wasm memory:
1.  **RPC**: Node buffers stream to disk/temp.
2.  **Handle**: Node passes integer `handle_id` to Wasm.
3.  **Host Functions**: Wasm calls `host.cas_put(handle)` or `host.chunk_file(handle)`.

### 6. Configuration (System Table)
CRUSH maps and policies are stored in `TABLE_SYSTEM`.
-   **Inheritance**: Child stores inherit these keys.
-   **Result**: Replication policy is hierarchical.

### 7. Future Scope: Pluggable Backends
The `CasBackend` trait abstracts storage:
-   **FsBackend** (M11A): General files (one file per blob).
-   **BlockBackend** (Future): ZFS/Databases (packed blocks/raw device).
-   **S3Backend** (Future): Cloud tier.

---

## Security & Privacy
Lattice enforces strict privacy to preventing "Known-CID" attacks.

### 1. Scoped Queries
Queries must be scoped: "In context of **Store Y**, do you have file X?". Global queries are forbidden.

### 2. Capability-Based Access
Requester must prove:
1.  **Membership** (Signed Capability).
2.  **Relevance** (Merkle Proof showing file X is in Store Y).

---

## Verification (Audit)
1.  **Implicit**: Accepting State Head implies availability of referenced blobs.
2.  **Explicit**: Random periodic audits ("Do you have CID X?").
