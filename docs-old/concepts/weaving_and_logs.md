# Weaving and the Intention DAG

Lattice does not use a typical central database transaction log. Instead, it uses **Weaver**, a decentralized replication protocol based on Directed Acyclic Graphs (DAGs).

## The Intention DAG Identity
In traditional databases, your "identity" is a row in a `users` table. In Lattice, your identity is cryptographic, and your actions form a continuous chain.

1.  **Append-Only:** Nodes can only add to their own history.
2.  **Cryptographic Links:** Every action (an **Intention**) contains the BLAKE3 hash of the author's previous Intention. This forms a tamper-proof chain.
3.  **Transitive Trust:** If you trust Intention N because the signature checks out, you implicitly trust Intentions 1 through N-1.

## Causal Dependencies (DAG Resolution)
When multiple offline nodes edit the same data concurrently, a linear sequence is impossible. Lattice embraces this by forming a DAG.

Every Intention includes a `causal_deps` field. This field contains the hashes of the exact state the author was looking at when they made the change (the "parents").

### The Fork and Merge Model
1.  **Write (Linear):** A node writes a value. It cites the current state hash as its parent.
2.  **Concurrent Fork:** Two nodes go offline and edit the same key independently. They both cite the same old parent. The history branches into two "tips."
3.  **Deterministic Read:** When the system sees a forked key, it doesn't crash. It uses a deterministic rule (Highest HLC Vector, tie-broken by Author ID) to pick a physical "winner" for reads, preserving both edits in history.
4.  **Causal Merge:** When a synced node writes a new value to that key, it cites *both* divergent tips in its `causal_deps`. The fork is merged back into a single tip.

This guarantees that no offline work is ever silently overwritten or lost—it is preserved in the graph until a subsequent write cleanly supersedes both.

## Hybrid Logical Clocks (HLC)
To order events in a decentralized mesh without a central time server, Lattice uses Hybrid Logical Clocks (`<wall_time, logical_counter>`).

Nodes use **Causal Clamping** to enforce sanity:
- If a received Intention's timestamp is drifting too far into the future (beyond `MAX_DRIFT`), the local node clamps its logical progression to ensure the DAG remains causally valid.
- Time is driven forward by the progression of the DAG itself, not strictly the system clock.

## IntentionStore vs StateBackend
The storage architecture reflects this split between the "truth" (the DAG) and the "view" (the current state):

-   `log.db` (Managed by `IntentionStore`): The raw, append-only cryptographic DAG. This is the source of truth that is replicated across the network over Iroh.
-   `state.db` (Managed by `StateBackend`): The materialized state (e.g., Redb key-value store). When the `IntentionStore` receives a validated Intention, it passes the payload to the `KvState` machine, which applies the update and updates its "tips." 

The DAG provides rigorous resilience; the materialized state provides rapid `O(1)` query performance.
