# DAG-Based Pruning Architecture

## 1. The Metric: Ancestry, not Index

Instead of `Seq` (linear index), we use the **Causal Graph**.

- **Rule**: An Intention is "Stable" (Prunable) if it is an **ancestor** of the Lowest Common Frontier of the quorum.
- **Mechanism**:
    1.  Node A gossips: *"I have checkpointed state covering DAG Frontier `{Hash_A, Hash_B}`."*
    2.  Node B gossips: *"I have checkpointed state covering DAG Frontier `{Hash_A}`."*
    3.  **Intersection**: The safe "Common Backbone" is `{Hash_A}` (and all its ancestors).

    > **Optimization (Vector Clock)**: Since Intentions carry a Logical `Seq`, we can represent these Frontiers efficiently as Vector Clocks (e.g., `{Alice: 100, Bob: 50}`) instead of raw hash sets. This makes intersection checks O(1).

## 2. The Physical Operation: Log Rewriting

We cannot just cut the file. We must rewrite it to create a new "Base Layer."

### Phase A: The Current Log (The Mess)
Your disk currently looks like a mixed bag of old committed history and new floating ops.
- **File**: `log.wal`
- **Content**: `[Genesis] -> [Op 1] -> [Op 2 (Stable)] -> [Op 3 (Unstable)] -> [Op 4 (Stable)]`
> Note: Physical order is mixed because ops arrived asynchronously.

### Phase B: The Compaction (The Filter)
The Pruner runs in the background. It calculates the **Stability Set** (all hashes that are strictly ancestors of the Quorum's common checkpoint).

It creates a new file `log.new`:
1.  **Header**: Writes the Common Snapshot (The new Genesis).
    -   Payload: `{ StateRoot: 0x..., Anchor_Frontier: [Hash_A] }`
2.  **Body**: Iterates through the old `log.wal`.
    -   If Op is in **Stability Set** → **Skip / Omit** (It is baked into the Snapshot).
    -   If Op is **NOT** in Stability Set → **Copy to `log.new`** (It is still pending/floating).
3.  **Link**: Re-signs the copied entries to point to the new Snapshot Anchor (wrapping them in new local envelopes if necessary, or just linking the first one to the snapshot).

### Phase C: The Switch
Atomic rename: `mv log.new log.wal`.

**Result**: The log file shrinks. The history is gone. The semantic chain remains unbroken because the Snapshot "Satisfies" the dependencies of the floating ops.

## 3. The "Determinism" Aspect

We rely on **State Determinism** to prevent orphans.

- **Concept**: *"Let the system prune deterministically."*
- **Logic**: If every node runs the exact same logic: `Safe_Set = Intersection(All_Peer_Frontiers)`
- **Outcome**: Then every node will—independently but identically—decide that `Op 1` and `Op 2` are garbage.

> My Node deletes Op 1.
> Your Node deletes Op 1.
> We both keep Op 3.

We stay in sync not because we coordinated the deletion, but because we coordinated the **State (Checkpoint)**, and the deletion is just a deterministic side effect of that state.
