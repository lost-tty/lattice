# Architecture Summary: The Lattice Protocol (Ly)

Ly is a sovereign, local-first "userspace kernel" designed to decouple data ownership from infrastructure. It replaces the centralized "Cloud Database" model with a Subjective Mesh of personal logs, enabling privacy, resilience, and offline-first collaboration.

## 1. The Core Philosophy: Subjective Reality
Unlike Blockchains (Global Consensus) or Raft (Cluster Consensus), Ly operates on **Subjective Consensus**.

- **No Single Truth**: There is no "Global Chain." Every node maintains its own independent Log (The Malm).
- **Convergence**: Nodes exchange operations ("Intentions"). Through deterministic topological sorting and CRDTs, all nodes that witness the same set of Intentions derive the exact same State.

## 2. The Data Model: "The Weaver Protocol"
The fundamental primitive is not the Block, but the **Intention**.

### A. The Intention (The Signal)
An atomic, immutable operation signed by an Author.
- **Structure**: `{ Payload, Author, Seq, PrevHash, Sig }`
- **Nature**: It is a "floating" message. It has no parent hash and is valid independent of the transport chain.

#### The Distinction: Physical vs. Logical
- **Physical Log (The Envelope)**: "Entry #500" on disk. Proves *when* a node saw it.
- **Logical Chain (The Letter)**: "Intention #42" from Alice. Proves *when* she wrote it relative to her history.
> **Note**: `Seq` acts as a Logical Counter, enabling O(1) pruning and efficient sync without global consensus.

#### The Three Superpowers of Seq
1.  **Vector Clock Pruning (Critical)**:
    -   Because Alice numbers her ops (1, 2, ... 100), we can summarize state as `{ Alice: 100, Bob: 50 }` instead of listing 150 individual hashes.
    -   This allows `Prune(UpTo: {Alice: 99})` to be an O(1) definition rather than O(N).
    -   Without Seq, "Up to Hash X" is meaningless; you would have to list every tip.

### B. The Witness (The Record)
Nodes do not "copy" other chains; they **witness** Intentions into their own chain.
- **Mechanism**: `Entry { Parent: MyTip, Event: Witness(Intention), Sig: MySig }`
- **Benefit**: This solves the "Ghost Parent" problem. Users can share subsets of data (Intentions) without exposing their entire private history.

### C. The Envelope Pattern (Gossip)
1. **Ingest**: Node A creates an Intention → Wraps in `Entry_A`.
2. **Transport**: Node A unwraps `Entry_A` → Sends raw Intention to Node B.
3. **Digest**: Node B validates Intention → Wraps in `Entry_B`.
- **Result**: Data provenance (Author Sig) is preserved, but the transport path is ephemeral.

## 3. The Compute Model: "The Active Hearth"
The Node is not a passive bucket; it is an active **Gatekeeper**.

### The Write Path (The Funnel)
1. **Network Ingest**: Ops land in the **Intention Log (WAL)**. Status: Durability Achieved.
2. **The Gatekeeper (Wasm)**: The State Machine reads the WAL.
    - **Validation**: Checks signatures, quotas, and state logic (e.g., "Cannot move file that doesn't exist").
    - **Sanitization**: Strips malicious scripts or expands macros (e.g., Upload → StoreBlob + IndexMetadata).
3. **Commit**: Valid Ops are appended to the Main Log. Invalid Ops are dropped.

### The Pre-requisite: Deterministic State
The Wasm State Machine is **deterministic**.

- **Convergence**: If Node A and Node B have witnessed the same set of Intentions (even in different orders), they **must** calculate the exact same Merkle Root for the Key-Value Store.
- **Conflict**: If they disagree on the State Root, it means they are missing data (different Intentions).
- **Resolution**: They must sync the missing Intentions before they can agree on a checkpoint.

## 4. The Privacy Model: Smart vs. Blind
The network is bifurcated into two node types to enable "Zero-Knowledge Infrastructure."

| Feature | The Hearth (Smart Node) | The Constellation (Blind Node) |
| :--- | :--- | :--- |
| **Role** | User Device / Home Server | Cloud VPS / Friend's Backup |
| **Keys** | Holds Private Key | No Keys (BYOK) |
| **Visibility** | Decrypts & Indexes Data | Stores Opaque Blobs |
| **Logic** | Runs Wasm State Machine | Runs only Sync Protocol |
| **Sync** | Syncs via Views (Scoped) | Syncs via Raw Log (All-or-Nothing) |

**Negentropy Sync**: Used by both. Blind nodes reconcile hashes of encrypted blobs. Smart nodes reconcile hashes of semantic views.

## 5. The Lifecycle: "Agrarian GC"
To prevent infinite log growth without central authority:

- **Quorum Compaction**: When a quorum of trusted peers agrees on history up to Time T, the node creates a Snapshot.
- **Pruning**: The log prior to T is truncated.
- **Bootstrapping**: New peers join by trusting an existing peer's Snapshot + Invitation (Web of Trust), rather than downloading the entire history of the universe.
