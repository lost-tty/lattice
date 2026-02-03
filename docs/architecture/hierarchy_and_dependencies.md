# Architecture Decision: Store Hierarchy & Dependency Graph

## The Design: "Parent-Knows-Child"

We adopt a **Directory-Style** hierarchy where the Parent Store holds the reference to the Child Store. The Child does not know its Parent.

### Rationale
1.  **Portability**: Detached stores can be moved, renamed, or aliased without modifying the store's internal state.
2.  **Aliasing**: A single store instance can be mounted in multiple locations (multiple parents).
3.  **Permissions**: Access capabilities flow down from the Parent (Directory) to the Child.

## The "Meta Interface" for Dependencies

While the *logical* hierarchy is defined in the Parent's data (e.g., a "Directory" `KvStore`), the *physical* references MUST be exposed to the Host (Node) via the `meta` table.

### Problem
If child references are buried in opaque `data` tables (serialized inside app-specific logic), the Host cannot efficiently:
-   **Garbage Collect**: Find orphaned stores that are no longer referenced.
-   **Deep Sync**: Recursively sync a store and all its nested children.
-   **Traverse**: Visualize the store graph.

### Solution: `KEY_DEPENDENCY` in `meta`
The `StateBackend` shall expose a standard interface for registering dependencies.

-   **Logic**: When `StateLogic` applies an Op that adds a child (e.g. `LinkStore`), it calls `backend.add_dependency(child_uuid)`.
-   **Persistence**: This writes to a standard prefix in the `meta` table (e.g., `dep/<uuid>`).
-   **Host Access**: The `StoreManager` can scan `meta` tables to build the global dependency graph *without* instantiating store logic.

### Implementation Contract
1.  **Source of Truth**: The Log/Data is the source of truth.
2.  **Derived State**: The `meta` dependencies are derived state, maintained by the `StateLogic` during `apply()`.
3.  **Invariant**: `StateLogic` MUST ensure the `meta` consistency (add/remove dependencies as they are added/removed from data).

## Mesh Propagation: The "Create-Link-Sync" Cycle

With the Parent-Knows-Child model, adding a store across the mesh leverages the existing sync protocol:

1.  **Create (Local)**: Node A creates a new store (UUID_B) with `store_type="type.v1"`.
2.  **Link (Parent Op)**: Node A sends an Op to the Parent Store (UUID_A): `LinkStore(path="/chat", id=UUID_B)`.
3.  **Propagate**: Node B syncs the Parent Store (UUID_A) and sees the new Link.
4.  **Discovery (Lazy)**:
    -   Node B effectively "sees" the folder `/chat`.
    -   When accessed (or if "Deep Sync" is active), Node B requests UUID_B from the mesh.
    -   Node A (or others) provide the `store_type` and sync data for UUID_B.

This creates an **Interest-Based Graph** where nodes can choose to sync:
-   **Roots Only**: Minimal footprint.
-   **Subtrees**: Sync a specific Project folder.
-   **Deep Clone**: Recursively sync everything reachable from Root.
