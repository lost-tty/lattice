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

The `StoreMeta` table (present in *every* store) gains:
- `children: Vec<StoreLink { id, name, mode }>`
- `parents: Vec<StoreLink { id }>` (for back-references/traversal)

### Peer Inheritance

A store can have a `PeerStrategy`:
- **Independent:** Manages its own peer list (like a Mesh today).
- **Inherited:** Uses the parent's peer list (live reference).
- **Snapshot:** Copies parent's peers at creation but diverges thereafter.

### Common Store API (Meta Table)

Every store exposes a common API for managing peers and substores using its local `StoreMeta` table. This moves the current `Mesh` logic into `Store` itself.

| Table | Managed By | Contents |
|-------|------------|----------|
| **Meta** (all stores) | `lattice-node` | Peer **Authorization** (status), Substore list (`children`), Config |
| **Data** (root store) | `State Machine` | Peer **Display Name Cache** (`/nodes/{pubkey}/name`) - replicated |

A "Root Store" is simply a store with `PeerStrategy::Independent` that acts as the trust anchor for a hierarchy. Its *Data* section provides a specialized cache for display names, while its *Meta* section handles the actual list of authorized peers.

**Note:** The root store is implicitly discoverable by walking up the `parents` links. All stores in the hierarchy—even those with `Independent` peer strategy—resolve peer display names from the root's cache to ensure consistent identity.


```rust
/// Substore management (replaces Mesh::create_store)
trait SubstoreManager {
    /// Declare a substore reference in this store's children list (Meta table).
    async fn declare_substore(&self, id: Uuid, name: &str, store_type: &str, peer_strategy: PeerStrategy) -> Result<()>;
    
    /// List all declared substores.
    async fn list_substores(&self) -> Result<Vec<StoreRef>>;
    
    /// Remove a substore reference.
    async fn remove_substore(&self, id: &Uuid) -> Result<()>;
}

/// Peer management (replaces Mesh::invite_peer, etc.)
trait PeerManager {
    /// Add or update a peer in this store's peer list (Meta table).
    async fn set_peer(&self, pubkey: PubKey, status: PeerStatus) -> Result<()>;
    
    /// Get the effective peer list (may be inherited from parent).
    async fn list_peers(&self) -> Result<Vec<PeerInfo>>;
    
    /// Get this store's peer strategy.
    fn peer_strategy(&self) -> PeerStrategy;
}

enum PeerStrategy {
    /// Owns its own peer list (/nodes/* keys in this store).
    Independent,
    /// Delegates to parent's peer list (live lookup).
    Inherited,
    /// Copied parent's peers at creation, now independent.
    Snapshot,
}

// Extend existing StoreRef with peer_strategy for substore relationships
struct StoreRef {
    id: Uuid,
    store_type: String,
    name: String,
    archived: bool,
    peer_strategy: PeerStrategy,  // NEW: how this store resolves peers
}
```

**Key Points:**
- `StoreHandle` implements both traits.
- `Inherited` strategy means `list_peers()` follows the parent pointer, not local data.
- A root store always uses `Independent` (no parent to inherit from).

## Consequences

### Positive
- **Service Composition:** A service (e.g., "Notes") can own substores for text, attachments, sync metadata.
- **Unified API:** No more `mesh.create_store()`. Just `parent_store.declare_substore()`.
- **Simplification:** Removes the entire `Mesh` struct and its management overhead.

### Negative / Challenges
- **Cycle Detection:** We must prevent `A -> B -> A` loops in the hierarchy.
- **Garbage Collection:** If a parent is deleted, what happens to substores? (Ref-counting or specialized "owned" vs "linked" edges).
- **Discovery Performance:** Finding a store by ID might require an index if we don't know the path.

## Implementation Plan

1.  **Schema Update:** Add `children` column to `StoreMeta`.
2.  **Migration:** Convert existing `Mesh` root stores and their subordinates to the new graph model.
3.  **Refactor Node:** Remove `Mesh` struct. `Node` holds a list of root stores.

