---
title: "Lattice Filesystem"
status: design
weight: 7
---

> **Status**: Unimplemented design (roadmap M21). Depends on M20 (CAS).

A replicated, CoW, snapshot-capable filesystem built as a Lattice store type. Borrows architectural ideas from ZFS (Merkle block trees, copy-on-write, datasets, snapshots, scrubbing) and maps them onto Lattice's two-channel model: the Intention DAG for causally ordered metadata, CAS for bulk data.

## Design Goals

1. **FUSE-mountable**: Appears as a regular directory on Linux/macOS. Standard tools (`cp`, `vim`, `rsync`) work without modification.
2. **Offline-first**: Full read/write capability without network. Changes sync when connectivity returns.
3. **Snapshot-capable**: Instant, cheap snapshots (ZFS-style). A snapshot pins a root CID at O(1) cost.
4. **Replicated**: Directory tree converges across peers via the DAG. File content distributes via CAS with CRUSH-based redundancy.
5. **Auditable**: Every file change is a signed intention in the DAG with author attribution and causal history.
6. **Conflict-visible**: Concurrent edits to the same file produce conflict markers (not silent overwrites).

## Architecture: Two-Channel Split

The filesystem uses the DAG and CAS for fundamentally different purposes, mirroring the reliability spectrum described in the [CAS design](../design/cas):

| Concern | Channel | Replication | Read Guarantee |
|---------|---------|-------------|----------------|
| Directory tree structure | DAG (intentions) | Eager — all peers | Always succeeds |
| File metadata (size, mtime, mode) | DAG (intentions) | Eager — all peers | Always succeeds |
| Snapshots, renames, deletes | DAG (intentions) | Eager — all peers | Always succeeds |
| Block pointer tree (indirect blocks) | CAS | Push to CRUSH peers | Recoverable |
| Data blocks (file content) | CAS | Push to CRUSH peers | Recoverable |

The analogy to ZFS is direct:

| ZFS Component | Lattice Equivalent | Channel |
|---|---|---|
| Uberblock (root pointer) | Intention: `UpdateRoot { root_cid, txg }` | DAG |
| ZIL (intent log) | The Intention DAG itself | DAG |
| Indirect blocks (block pointer tree) | CAS blobs containing child CID lists | CAS |
| Data blocks | CAS blobs (leaf content) | CAS |
| Space maps | Local-only (each node manages its own CAS storage) | Neither |

### The ZIL Analogy

The ZFS Intent Log records pending synchronous writes so they survive a crash before the next transaction group commits. It has two modes: **WR_COPIED** (small writes — data inlined in the log record) and **WR_INDIRECT** (large writes — log record contains only a block pointer, data written separately).

Lattice intentions follow the same split naturally:

| ZIL Mode | Lattice Equivalent |
|---|---|
| WR_COPIED (data + metadata inline, < 32 KB) | Small file operations inlined in intention payload |
| WR_INDIRECT (metadata + pointer to data block) | Intention payload contains CID reference, data in CAS |

The threshold is configurable per store. Below it, the full operation (including small file content) travels through the DAG for maximum reliability. Above it, only the CID pointer enters the DAG and the bulk data travels through CAS.

### CRUSH Replication Classes

Not all CAS blobs are equally critical. The filesystem store can assign different CRUSH replication factors by blob class:

| Blob Class | Content | Default CRUSH Factor | Rationale |
|---|---|---|---|
| `metadata` | Indirect blocks (block pointer tree nodes) | All peers (100%) | Loss means orphaned data blocks. Small, critical. |
| `data` | File content chunks | Store default (e.g., 3) | Loss is recoverable from other peers. Large, bulk. |
| `snapshot` | Snapshot manifest blobs | All peers | Loss means snapshot is unresolvable. Small, critical. |

The `StateMachine::referenced_blobs()` implementation returns CIDs along with their class, allowing the `CasManager` to apply different CRUSH policies per class. This mirrors ZFS's distinction between metadata and data redundancy (e.g., `copies=2` for metadata, `copies=1` for data).

## Data Model

### Inode Table (`TABLE_DATA`)

Every filesystem object (file, directory, symlink) is an inode. The inode table lives in `state.db` (projected from intentions) using the existing redb `TABLE_DATA`:

```
Key:   inode/{inode_id}
Value: InodeEntry (protobuf)
```

```protobuf
message InodeEntry {
    uint64 inode_id = 1;
    InodeType type = 2;              // FILE, DIRECTORY, SYMLINK
    uint32 mode = 3;                 // Unix permissions
    uint64 size = 4;                 // File size in bytes
    uint64 created_at = 5;           // HLC wall_time
    uint64 modified_at = 6;          // HLC wall_time
    bytes content_cid = 7;           // Root CID of content Merkle tree (files only)
    string symlink_target = 8;       // Target path (symlinks only)
    repeated ChunkRef chunks = 9;    // Inline chunk list for small files (< tree threshold)
}

message ChunkRef {
    bytes cid = 1;
    uint64 offset = 2;
    uint64 length = 3;
}
```

### Directory Table

Directory entries are stored as a separate key prefix to enable efficient `readdir`:

```
Key:   dir/{parent_inode_id}/{name}
Value: uint64 child_inode_id
```

A `readdir` becomes a prefix scan on `dir/{inode_id}/` — a single redb range query, entirely local.

### Root Pointer

The filesystem root inode is a well-known entry:

```
Key:   root
Value: uint64 root_inode_id
```

### Snapshot Table

```
Key:   snapshot/{name}
Value: SnapshotEntry (protobuf)
```

```protobuf
message SnapshotEntry {
    string name = 1;
    uint64 root_inode_id = 2;        // Inode ID of the root at snapshot time
    bytes root_content_state = 3;    // Opaque state reference for the frozen tree
    uint64 created_at = 4;           // HLC wall_time
    bytes creating_intention = 5;    // Hash of the snapshot intention (for causal anchoring)
}
```

### Transaction Counter

```
Key:   txg
Value: uint64 transaction_group_number
```

Monotonically increasing per node. Used to batch multiple writes into a single intention (see Write Path below).

## Block Tree Structure

File content is stored as a Merkle tree of CAS blobs, similar to ZFS's indirect block tree:

```
                    [Root CID]                    ← InodeEntry.content_cid
                   /          \
          [Indirect L1]    [Indirect L1]          ← CAS blobs: list of child CIDs
         /     |     \         |
   [Data]  [Data]  [Data]   [Data]                ← CAS blobs: raw file content chunks
```

- **Data blocks** (leaves): Raw file content, produced by content-defined chunking (FastCDC). Target size: 1 MiB, min 256 KiB, max 4 MiB.
- **Indirect blocks** (internal nodes): CAS blobs containing an ordered list of `ChunkRef { cid, offset, length }` entries. Each indirect block covers a contiguous byte range of the file.
- **Root CID**: The top of the Merkle tree, stored in `InodeEntry.content_cid`.

**Small file optimization**: Files under the minimum chunk size (256 KiB) are stored as a single CAS blob. The `InodeEntry.chunks` field holds the single `ChunkRef` inline — no indirect blocks, no tree. Files under an even smaller threshold (configurable, e.g. 4 KiB) could embed content directly in the intention payload (WR_COPIED mode).

**Integrity**: Every CAS blob is content-addressed (CIDv1, SHA2-256). The Merkle tree provides transitive integrity — verifying the root CID verifies the entire file. This is strictly equivalent to ZFS's block checksum tree but with cryptographic hashes.

**Scrubbing**: Walk the Merkle tree from root, verify each CAS blob's hash against its CID. Corrupt or missing blobs are re-fetched from CRUSH duty holders. This can run as a background maintenance task.

## Operations

### Intention Payload

```protobuf
message FsPayload {
    repeated FsOp ops = 1;
    uint64 txg = 2;                  // Transaction group number (for batching)
}

message FsOp {
    oneof op_type {
        CreateOp create = 1;
        MkdirOp mkdir = 2;
        UpdateFileOp update_file = 3;
        UnlinkOp unlink = 4;
        RenameOp rename = 5;
        ChmodOp chmod = 6;
        SnapshotOp snapshot = 7;
        BatchCreateOp batch_create = 8;
    }
}

message CreateOp {
    uint64 parent_inode = 1;
    string name = 2;
    uint32 mode = 3;
    InodeType type = 4;
    bytes content_cid = 5;           // Initial content (for files created with data)
    repeated ChunkRef chunks = 6;    // Chunk list
    uint64 size = 7;
}

message UpdateFileOp {
    uint64 inode_id = 1;
    bytes content_cid = 2;           // New root CID of content tree
    repeated ChunkRef chunks = 3;    // Updated chunk list
    uint64 size = 4;
}

message RenameOp {
    uint64 src_parent = 1;
    string src_name = 2;
    uint64 dst_parent = 3;
    string dst_name = 4;
}

message UnlinkOp {
    uint64 parent_inode = 1;
    string name = 2;
}

message SnapshotOp {
    string name = 1;
}

message BatchCreateOp {
    repeated CreateOp creates = 1;   // Batch file creation (for import, cp -r)
}
```

### Write Path (Buffered, Transaction-Group Model)

Writes are **buffered locally** and flushed on `fsync` or periodically (like ZFS transaction groups). This is critical — without buffering, a `cp 1GB_file ~/lattice/` would generate thousands of individual intentions.

```
FUSE write(inode, offset, buf)
  → Buffer in local page cache (mark dirty). No intention, no CAS write.

FUSE fsync(fd)    [or periodic flush, or close()]
  → Collect dirty pages for inode
  → Content-defined chunking (FastCDC) on modified regions
  → cas.put_batch(new_chunks) → Vec<Cid>
  → Build updated block tree (CoW: new indirect blocks for modified paths,
    reuse unchanged subtrees by CID)
  → cas.put(new_indirect_blocks) → new root CID
  → Submit intention: FsPayload {
        ops: [UpdateFileOp { inode, content_cid: new_root, chunks, size }],
        txg: next_txg()
    }
  → CasManager pushes new CAS blobs to CRUSH duty holders (async)
```

**Transaction groups**: Multiple writes across multiple files can be batched into a single intention (single `FsPayload` with multiple `FsOp`s). The FUSE layer accumulates dirty state and flushes at a configurable interval (default: 5 seconds, matching ZFS's txg commit interval). This amortizes the Ed25519 signing and DAG overhead across many file operations.

**Metadata-only operations** (rename, chmod, mkdir, unlink) are submitted immediately as tiny intentions — no buffering needed since they don't involve CAS blobs.

### Read Path

```
FUSE read(inode, offset, len)
  → state.db lookup: inode/{id} → InodeEntry { content_cid, chunks }
  → Resolve offset to chunk:
      If inline chunks: binary search ChunkRef list
      If tree: walk indirect blocks (CAS reads) to find leaf
  → cas.get_range(leaf_cid, local_offset, local_len)
  → If CAS hit: return data
  → If CAS miss: request from CRUSH peers, block until available (or EIO on timeout)
```

**Caching**: An in-memory ARC cache (keyed by `(cid, range)`) serves hot reads without CAS filesystem I/O. Size the cache to available RAM (configurable, default 256 MiB).

**Readahead**: Detect sequential access patterns and prefetch upcoming chunks from CAS (or from peers if not local). This enables streaming video playback from a FUSE-mounted Lattice filesystem.

### Copy-on-Write

Every write produces new CAS blobs (new data blocks + new indirect blocks on the modified path). Unchanged subtrees are shared by CID. This is identical to ZFS's CoW semantics:

```
Before:     Root_v1 → [Indirect_A] → [Data_1, Data_2, Data_3]

Write to offset in Data_2:

After:      Root_v2 → [Indirect_A'] → [Data_1, Data_2', Data_3]
                                        (shared) (new)   (shared)

Root_v1, Indirect_A, Data_2 remain in CAS (immutable).
A snapshot pinning Root_v1 retains the old tree.
Without a snapshot, old blobs become GC-eligible.
```

### Snapshots

A snapshot is a single intention that records the current root state:

```
FsOp::Snapshot { name: "before-upgrade" }

apply():
  → Read current root inode state
  → Store SnapshotEntry { name, root_inode_id, root_content_state, created_at }
  → Cost: one intention (~100 bytes). No data copies.
```

Because all CAS blobs are immutable and content-addressed, the snapshot is just a metadata marker. The Merkle tree rooted at the snapshot's `content_cid` is preserved as long as the snapshot exists. GC cannot collect blobs reachable from any live snapshot.

**Browsing snapshots**: Snapshots are read-only views. A FUSE mount could expose them as `.snapshots/{name}/` virtual directories (like ZFS `.zfs/snapshot/`), resolving reads against the frozen inode table state.

**Incremental replication** (analogy to `zfs send/recv`): Diff two snapshots by walking their Merkle trees from the root. CIDs present in the new tree but absent from the old tree are the incremental changeset. Transfer only those blobs. The DAG provides the snapshot bookkeeping; CAS provides the block transfer.

## Conflict Resolution

The filesystem store does **not** use LWW for file content. It implements a tree CRDT with conflict markers.

### File Content Conflicts

When two peers edit the same file offline, each produces a new `UpdateFileOp` with a different `content_cid`. These are concurrent intentions (neither author had seen the other's write).

The state machine detects this via the `heads` mechanism (same as KVTable, but per-inode): `|heads(inode_id)| > 1` means concurrent content updates.

**Resolution strategy**: Conflict files.

```
report.docx                                    ← One version (most recent by HLC)
report.conflict-alice-2026-03-23T14:30.docx    ← The other version
```

The state machine creates a synthetic directory entry pointing to the losing version's CID. The user sees both files and chooses which to keep. Deleting the conflict file (or overwriting the main file) produces an intention that causally depends on both heads, collapsing the conflict.

### Structural Conflicts (Tree CRDT)

Directory operations introduce global conflicts that no single inode's headlist can detect (see [Conflict Resolution Architecture](../design/meet-vs-crdt), filesystem section). The state machine handles these at `apply()` time:

| Conflict | Detection | Resolution |
|---|---|---|
| **Name collision** (two peers create `/docs/report.txt`) | `dir/{parent}/{name}` has multiple heads | Auto-rename: `report.txt`, `report (2).txt` |
| **Orphan** (delete parent while child is created) | Child's `parent_inode` points to a deleted inode | Re-parent to `lost+found/` |
| **Cycle** (move A into B, move B into A concurrently) | Parent-chain walk during `apply()` | LWW on move operations — one move wins, other is reverted |
| **Move + rename** (move file, rename same file concurrently) | Inode has multiple parent pointers in heads | LWW on the inode's parent/name, conflict file for content if applicable |

The `DagQueries` handle passed to `apply()` provides `find_lca()` and `is_ancestor()` for the causal context needed to resolve these correctly.

## FUSE Daemon (`lattice-fs`)

Following the [Service Architecture](../design/services) "Kernel & User-Space" model, the FUSE daemon is an **external process**:

```
lattice-fs (daemon)
  │
  ├── Connects to lattice-node via UDS (Unix Domain Socket)
  ├── Authenticates with a Service Token
  ├── Opens the target store (must be core:filestore type)
  ├── Mounts FUSE at a configured path
  │
  ├── Read path:  FUSE → inode lookup (state.db) → CAS get_range
  ├── Write path: FUSE → local page cache → fsync → CAS put_batch → submit intention
  └── Watch path: subscribes to FsState events for live updates from other peers
```

**Live updates**: When another peer's intention arrives and is projected, the FUSE daemon receives a `WatchEvent` (via the `StreamProvider` subscription) and invalidates affected inodes in its local cache. The next `stat()` or `read()` sees the updated state.

**Crash isolation**: If `lattice-fs` crashes, the Lattice node is unaffected. Unflushed writes in the page cache are lost (standard POSIX behavior — same as if the kernel crashed with dirty pages). CAS blobs from completed `put_batch` calls that lack a corresponding intention are orphans, cleaned up by GC after the grace period.

## Selective Sync

Not all peers need all file content. A device can sync the full directory tree (metadata, via the DAG) but only fetch content for selected subtrees:

```
system/fs/sync_policy → SyncPolicy proto
```

```protobuf
message SyncPolicy {
    repeated SyncRule rules = 1;
}

message SyncRule {
    string path_prefix = 1;          // e.g., "/photos/", "/videos/"
    SyncMode mode = 2;
}

enum SyncMode {
    FULL = 0;                        // Fetch all content CAS blobs
    METADATA_ONLY = 1;               // DAG only, no CAS blobs (directory listing works, reads fail)
    ON_DEMAND = 2;                   // Fetch CAS blobs on first access, cache locally
}
```

The `referenced_blobs()` implementation filters CIDs based on the sync policy: only CIDs for `FULL` subtrees are reported to the `CasManager` as required. `METADATA_ONLY` subtrees show in `readdir` and `stat` (size, mtime from inode metadata in `state.db`) but `read()` returns `EIO` or triggers an on-demand fetch.

This enables the multi-device tiering pattern:

| Device | Policy | Behavior |
|---|---|---|
| Phone | `/photos/ → ON_DEMAND`, everything else `METADATA_ONLY` | Browse tree, fetch photos on tap |
| Laptop | `/documents/ → FULL`, `/videos/ → ON_DEMAND` | Documents always available, videos on demand |
| NAS | `/ → FULL` | Everything, always. The cold archive. |

## Performance Characteristics

### Throughput Estimates

| Operation | Mechanism | Expected Performance |
|---|---|---|
| `readdir` (100 entries) | redb prefix scan, pure local | < 1 ms |
| `stat` | redb point lookup | < 0.1 ms |
| Sequential read (local) | ARC cache + CAS filesystem read | 200-500 MB/s (SSD-bound) |
| Sequential read (LAN peer) | QUIC chunk streaming | 50-200 MB/s (network-bound) |
| Random read (cached) | In-memory ARC | Sub-millisecond |
| Random read (CAS miss, local) | redb lookup + filesystem seek | ~0.1-1 ms (SSD) |
| File write (buffered) | RAM page cache | Memory speed |
| fsync (1 MB dirty) | CAS put + sign + commit | ~5-15 ms |
| fsync (100 MB dirty, txg flush) | Batch CAS put + sign + commit | ~200-500 ms |
| mkdir / rename / unlink | Single intention (tiny) | ~1-5 ms |
| Snapshot | Single intention (tiny) | ~1-5 ms |

### Write Amplification

Per file save (fsync):
1. CAS blob writes (one per new/modified chunk + indirect blocks)
2. Intention write (log.db — one redb transaction)
3. Witness record write (log.db — batched with above)
4. Projection write (state.db — one redb transaction)

Total: **3 durable writes** per fsync. Comparable to a journaling filesystem (ext4: 1-2 writes). The transaction-group batching model amortizes this across many file operations.

### Scaling Limits

- **Intention signing**: Ed25519 at ~60 us/op = ~16K intentions/sec. With txg batching (many file ops per intention), this is not the bottleneck for filesystem workloads.
- **Inode count**: redb handles millions of keys. 1M files = ~1M inode entries + ~1M directory entries = manageable.
- **CAS blob count**: At 1 MiB average chunk size, 1 TB of data = ~1M blobs. The sharded `blobs/{shard}/{cid}` layout handles this (256 shards, ~4K files per shard).
- **DAG depth**: A filesystem with 1M file operations over its lifetime has a deep DAG. LCA computation is O(depth) but only needed for conflict resolution, not normal reads.

## Comparison with Alternatives

| Feature | Syncthing | Dropbox | Lattice FS |
|---------|-----------|---------|------------|
| Architecture | Peer-to-peer | Client-server | Peer-to-peer |
| Conflict handling | Conflict files | Conflict copies (server merge) | Conflict files + tree CRDT |
| Large file sync | 128 KiB block-level | Proprietary chunking | Content-defined chunking (FastCDC) |
| Selective sync | Per-folder ignore patterns | Selective Sync (paid) | Per-subtree sync policy |
| Versioning | None (external only) | 180-day history (paid) | Full DAG history + snapshots |
| Offline support | Full | Limited (cached files) | Full (local-first) |
| Audit trail | None | Limited | Full — every change is a signed intention |
| Snapshots | No | No | Yes (ZFS-style, instant, cheap) |
| Integrity verification | Block-level hash | Opaque | Merkle tree (transitive, cryptographic) |
| Encryption | TLS in transit | At rest + transit (server has keys) | Store-level E2E (client-side) |

## Store Type Registration

```rust
pub const STORE_TYPE_FILESTORE: &str = "core:filestore";

// In lattice-runtime:
self.with_opener(STORE_TYPE_FILESTORE, || {
    direct_opener::<SystemLayer<FsState>>()
})
```

`FsState` implements `StateLogic` with `type Event = FsEvent`. The `SystemLayer<FsState>` wrapping provides the system leg (peers, hierarchy, invites) automatically. The `FsState::apply()` decodes `FsPayload` from the intention and mutates the inode/directory tables in `TABLE_DATA`.

## Open Questions

1. **Chunk manifest threshold**: At what file size should the inline `ChunkRef` list in `InodeEntry` be replaced by a CAS blob manifest? Candidate: when the chunk list exceeds ~64 KB (roughly 1500 chunks at 40 bytes each, corresponding to a ~1.5 GB file). Above this, the intention payload should contain only a manifest CID.

2. **Hardlinks**: ZFS supports hardlinks (multiple directory entries pointing to the same inode). This is feasible (the inode table is separate from the directory table) but adds refcounting complexity for GC. Defer to a later milestone unless required.

3. **Extended attributes (xattrs)**: Useful for macOS metadata, SELinux labels, etc. Could be stored as additional keys under `xattr/{inode_id}/{name}`. Not critical for MVP.

4. **mmap support**: FUSE `mmap` requires the file to be locally available in full. With lazy CAS replication, this means either blocking until all chunks are fetched, or limiting `mmap` to files with `SyncMode::FULL`. Needs investigation.

5. **Deduplication across files**: Content-defined chunking provides implicit dedup within a store (same chunk content = same CID). Cross-store dedup is not supported (per-store CAS sidecar). Within-store dedup is free and automatic.
