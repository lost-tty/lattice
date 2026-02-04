# ADR 001: The Fractal Store Model

## Context

Lattice currently distinguishes between a `Mesh` (container for stores and peers) and a `Store` (single data unit). A `Mesh` owns a `RootStore` which tracks peers and declared sub-stores.

This distinction creates friction:
*   **Duplicated Logic:** Peer management exists in `Mesh` but logically belongs to `Store`.
*   **Discovery Limits:** Finding a store requires knowing which Mesh it belongs to.

## Proposal

We propose the **Fractal Store Model** where stores can reference other stores, effectively making the `Mesh` concept superfluous.

> **Note:** This is *not* a filesystem. A single store might contain an entire filesystem, database, or application state. The hierarchy is for **grouping services** — e.g., a "Photos" service with substores for metadata, thumbnails, and sync state.

The primary motivations are **Discovery** and **Context Inheritance**:
1.  **Discovery:** Finding a store requires traversing from a known root, rather than knowing a global ID.
2.  **Context Inheritance:** Substores can automatically inherit peers and permissions from their parent, creating shared trust domains without manual replication.

In this model:
1.  **Mesh Obsolescence:** A "Mesh" is simply a Store that acts as a root.
2.  **Recursive Structure:** Stores form a graph where edges represent both discovery paths and potential permission inheritance.

## Decision

We will adopt the **Fractal Store Model**:

1.  **Universal Container:** Every `Store` can declare references to substores.
2.  **Mesh Obsolescence:** The concept of `Mesh` is removed. A "Mesh" is simply a Store we treat as a root.
3.  **Inheritance:** Substores can inherit properties (peers, permissions, config) from their parent.

### Data Model Changes

We replace the flat `Mesh` -> `RootStore` -> `Map<StoreId, StoreType>` structure with a graph-based model.

### Data Model Changes

We replace the flat `Mesh` -> `RootStore` -> `Map<StoreId, StoreType>` structure with a graph-based model.

#### 1. Universal Envelope

All operations in the system are wrapped in a `UniversalOp` to safeguard the distinction between Application Data and System Operations.

```protobuf
message UniversalOp {
    oneof op {
        bytes app_data = 1;      // Forwarded to inner logic (e.g. KvState)
        SystemOp system = 2;     // Handled by shared StateBackend
    }
}

message SystemOp {
    oneof kind {
        HierarchyOp hierarchy = 1; // Manage children/parents
        PeerOp peer = 2;           // Manage authorized peers
    }
}
```

#### 2. System Table (CRDT)

The `StateBackend` maintains a dedicated `system` table using `HeadList` CRDTs to manage infrastructure state.
- **Hierarchy:** `child/{id}`, `parent/{id}`
- **Peers:** `peer/{pubkey}`
- **Invites:** `invite/{hash}` (one-time tokens for peer authorization)
- **Config:** `config/strategy` (PeerStrategy persistence)

This ensures that concurrent modifications to the hierarchy OR peer list are preserved. For example, if two admins concurrently authorize different peers, both remain authorized (set union).

### Peer Inheritance

A store can have a `PeerStrategy` (stored in `StoreMeta` as configuration):
- **Independent:** Manages its own peer list (like a Mesh today).
- **Inherited:** Uses the parent's peer list (live reference).
- **Snapshot:** Copies parent's peers at creation but diverges thereafter.

### Common Store API (Meta Table)

Every store exposes a common API for managing peers and substores.

| Table | Managed By | Contents |
|-------|------------|----------|
| **Hierarchy** (all stores) | `StateBackend` | Substore list (`children`), Parent links (`parents`) - via `SystemOp` |
| **Meta** (all stores) | `lattice-node` | Peer **Authorization** (status), Peer Strategy, Config |
| **Data** (root store) | `State Machine` | Peer **Display Name Cache** (`/nodes/{pubkey}/name`) - replicated |

A "Root Store" is simply a store with `PeerStrategy::Independent` that acts as the trust anchor for a hierarchy.

**Note:** The root store is implicitly discoverable by walking up the `parents` links. All stores in the hierarchy—even those with `Independent` peer strategy—resolve peer display names from the root's cache to ensure consistent identity.


```rust
/// Substore management (replaces Mesh::create_store)
trait SubstoreManager {
    /// Declare a substore reference in this store's hierarchy.
    /// Emits a SystemOp::Hierarchy(AddRelation).
    async fn declare_substore(&self, id: Uuid, name: &str, store_type: &str, peer_strategy: PeerStrategy) -> Result<()>;
    
    /// List all declared substores (resolved from HeadList).
    async fn list_substores(&self) -> Result<Vec<StoreRef>>;
    
    /// Remove a substore reference.
    async fn remove_substore(&self, id: &Uuid) -> Result<()>;
}
```

**Key Points:**
- `StoreHandle` implements both traits.
- `Inherited` strategy means `list_peers()` follows the parent pointer, not local data.
- A root store always uses `Independent` (no parent to inherit from).

## Consequences

### Positive
- **Future Proof:** `SystemOp` allows adding ACLs, Quotas, and Policy without breaking store implementations.
- **Robustness:** Hierarchy changes are safe under concurrency (no lost children).
- **Service Composition:** A service (e.g., "Notes") can own substores for text, attachments, sync metadata.
- **Unified API:** No more `mesh.create_store()`. Just `parent_store.declare_substore()`.
- **Simplification:** Removes the entire `Mesh` struct and its management overhead.

### Negative / Challenges
- **Cycle Detection:** We must prevent `A -> B -> A` loops in the hierarchy.
- **Garbage Collection:** If a parent is deleted, what happens to substores? (Ref-counting or specialized "owned" vs "linked" edges).
- **Discovery Performance:** Finding a store by ID might require an index if we don't know the path.

## Implementation Plan

1.  **Proto Refactor:** Move `HeadList` to `storage.proto` and define `UniversalOp`. ✅ Done
2.  **System Store Crate:** Created `lattice-systemstore` with `TABLE_SYSTEM` and `SystemStore` trait. ✅ Done
3.  **Migration:** Legacy peer/invite ops must be recognized in `apply()` and redirected to `TABLE_SYSTEM`. ⏳ In Progress
4.  **Hierarchy:** Add child/parent links to system table.
5.  **Refactor Node:** Remove `Mesh` struct. `Node` holds a list of root stores.

## Current Status

The `lattice-systemstore` crate now provides:
- `SystemStore` trait with `get_peer`, `update_peer`, `get_peers` methods
- `PersistentState<T>` wrapper that intercepts `SystemOp::Peer` during `apply()`
- HeadList CRDT storage in `TABLE_SYSTEM` with `peer/{pubkey}` keys

Remaining work:
- Invite handling (move from `TABLE_DATA` to `TABLE_SYSTEM`)
- Hierarchy links (child/parent)
- Op-based migration for existing stores
