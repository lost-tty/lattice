---
title: "Weaver and the Intention DAG"
weight: 2
---

Lattice does not use a typical central database transaction log. Instead, it uses **Weaver**, a decentralized replication protocol based on Directed Acyclic Graphs (DAGs).

## The Intention DAG Identity

In traditional databases, your "identity" is a row in a `users` table. In Lattice, your identity is cryptographic, and your actions form a continuous chain.

1. **Append-Only:** Nodes can only add to their own history.
2. **Cryptographic Links:** Every action (an **Intention**) contains the BLAKE3 hash of the author's previous Intention in this store (`store_prev`), plus a `store_id` identifying the target store. This forms a tamper-proof per-store chain.
3. **Transitive Trust:** If you trust Intention N because the signature checks out, you implicitly trust Intentions 1 through N-1.

## Causal Dependencies (DAG Resolution)

When multiple offline nodes edit the same data concurrently, a linear sequence is impossible. Lattice embraces this by forming a DAG.

Every Intention includes a `condition` field containing the hashes of the exact state the author was looking at when they made the change (the "parents"). See the [Weaver Protocol Specification](../protocol/weaver.md) for the full data model.

### The Fork and Merge Model

1. **Write (Linear):** A node writes a value. It cites the current state hash as its parent.
2. **Concurrent Fork:** Two nodes go offline and edit the same key independently. They both cite the same old parent. The history branches into two "tips."
3. **Deterministic Read:** When the system sees a forked key, it doesn't crash. It uses a deterministic rule (Highest HLC, tie-broken by Author ID) to pick a physical "winner" for reads, preserving both edits in history.
4. **Causal Merge:** When a synced node writes a new value to that key, it cites *both* divergent tips in its `condition`. The fork is merged back into a single tip.

This guarantees that no offline work is ever silently overwritten or lost—it is preserved in the graph until a subsequent write cleanly supersedes both.

## Hybrid Logical Clocks (HLC)

To order events in a decentralized mesh without a central time server, Lattice uses Hybrid Logical Clocks (`<wall_time, counter>`).

Nodes use `clamp_future` to enforce sanity:
- If a received Intention's timestamp drifts too far into the future (beyond `DEFAULT_MAX_DRIFT_MS`), the local node replaces the HLC entirely with the parent's timestamp incremented by one, keeping the DAG causally valid.
- Time is driven forward by the progression of the DAG itself, not strictly the system clock.

## IntentionStore vs StateBackend

The storage architecture reflects this split between the "truth" (the DAG) and the "view" (the current state):

- `log.db` (Managed by `IntentionStore`): The raw, append-only cryptographic DAG. This is the source of truth that is replicated across the network.
- `state.db` (Managed by `StateBackend`): The materialized state (e.g., Redb key-value store). Derived by replaying the witness log through the state machine. Can always be rebuilt from `log.db`.

The DAG provides rigorous resilience; the materialized state provides efficient query performance via B-tree lookups.

See [Stores](stores.md) for the complete on-disk layout and table reference.
