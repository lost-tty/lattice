
## Research & Protocol Evolution

Research areas and papers that may inform future Lattice development.

### Trust Foundation (Future)

**Goal:** Ensure signed Wasm blobs can be independently verified as the output of known source code.

**Reproducible Build Attestation:**
- `BuildManifest` schema: `source_hash`, `toolchain`, `output_hash`, `builder_id`
- Store attestations alongside Wasm reference in sigchain
- Require N matching attestations before execution (configurable policy)

**Remote Attestation Protocol:**
- Attestation challenge/response in sync handshake
- Hardware roots of trust (TPM 2.0, Apple Secure Enclave, ARM TrustZone)
- Fallback: software attestation via signed kernel hash

| Challenge | Mitigation |
|-----------|------------|
| Rust/LLVM non-determinism | Pinned toolchain + `SOURCE_DATE_EPOCH` + locked `Cargo.lock` |
| Build environment variance | Nix flake or Docker for hermetic builds |
| Builder trust model | Named orgs, stake-weighted, or web-of-trust attesters |

### Byzantine Fork Detection
Formal framework for detecting equivocation (same seq# with different content). Generate `FraudProof` for permanent blocklisting.
- **Apply to:** `sync_state.rs` fork detection → punitive action
- **Ref:** [Kleppmann & Howard (2020)](https://arxiv.org/abs/2012.00472)

### Sybil Resistance & Gossip Scaling
Mechanisms to handle millions of nodes and prevent gossip flooding from malicious actors.
- **Problem:** Unbounded gossip from "millions of nodes" (Sybil attack) overwhelms bandwidth/storage.
- **Mitigation:** Resource constraints (PoW), Web-of-Trust gossip limits (only gossip for friends-of-friends), or reputation scores.

### Byzantine Fault Tolerance (BFT)
Ensure system resilience against malicious peers who may lie, omit messages, or attempt to corrupt state (beyond simple forks).
- **Objective:** Validated consistency without a central authority or global consensus.
- **Strategy:** Local verification of all data (Intention DAGs), cryptographic prohibition of history rewriting, and detection/rejection of invalid CRDT merges.

### Deep vs. Shallow Log Architecture

**Goal:** Enable seamless collaboration while preserving privacy and history ownership.

**Concept:**
- **Owner Nodes (Deep Log):** Maintain full version history (Genesis → Now), undo capability, and audit trails.
- **Guest Nodes (Shallow Log):** Maintain a "fresh" view (Snapshot → Now). They have zero knowledge of previous edits, deleted drafts, or past versions.

#### 1. The Architecture: Deep vs. Shallow Views
You don't create a separate database for the friend. You simply refuse to serve them the raw log.

- **The Owner's View (Deep Log):**
  - **Data:** `[Op1, Op2 ... Op1000]`
  - **State:** Derived by applying Op1 through Op1000.
  - **Capability:** Full Read/Write/History.

- **The Friend's View (Synthetic Genesis):**
  - **Data:** `[Synthetic_Snapshot_Op, Op1001, Op1002...]`
  - **State:** Derived by loading the Snapshot, then applying new Ops.
  - **Capability:** Partial Read/Write/No-History.

#### 2. The Mechanism: The "Synthetic Genesis"
When a friend connects to sync, your node performs a specific sequence to "scrub" the history.

- **Step A: The Cut (Snapshot Generation):** The Wasm State Machine generates a snapshot at the current Head. Crucially, this must be "Collapsed" (e.g., deleted text is physically removed).
- **Step B: The Vector Clock Handover:** Send Causal Context (Vector Clock) so they can write back without breaking the mesh.
  - *Packet:* "State valid at Logical Time [Alice: 50, Bob: 20]."
  - *Effect:* Friend starts numbering operations at Bob: 21.
- **Step C: The "Ephemeral" Sync:** Only replicate New Operations (T > Now). They see live updates but never the ops before the handshake.

#### 3. Handling the "Write Back" (The Merge)
How does a friend edit a document without the history?

- **State-based CRDTs:** Trivial; snapshot provides current value.
- **Op-based CRDTs:** The "Partial Snapshot" must include Unique IDs of active elements (or Tombstones).
  - *Example:* You send "Hello" (IDs: A1-A5). Friend deletes "lo" (IDs: A4, A5).
  - *Merge:* Your node looks up A4/A5 in the Deep Log and marks them deleted, even if the friend didn't know A4 was created years ago.

#### 4. The Privacy Guarantee (Forward Secrecy)
If a friend's device is seized or malicious:
- **They have:** The state of the project at sync time.
- **They do NOT have:** Deleted drafts, timestamp metadata of past writes, or identity of previous contributors (signatures stripped).

#### Summary
- **Storage:** Log (Append-only).
- **Transport:** State (Snapshot).
- **Updates:** Future Ops (Stream).
- **Role:** You act as a "Time Gateway," allowing intention to the stream 'now' but not 'travel back'.

### Use Case: The Serverless Compute Grid

By implementing **Shallow Views** (Snapshot-only) and **Blind Operations** (State-based updates), this architecture naturally enables a **Serverless Compute Grid**.

If a connected node can view a specific "Current State" (a task context) and submit a "Future Op" (the computation result) without needing access to the causal history, it effectively functions as a stateless **Worker**.

- **Workflow:**
  1. **Owner** prepares a task state (e.g., a matrix to multiply, a frame to render).
  2. **Worker** (Guest) receives the "Synthetic Genesis" of just that task state.
  3. **Worker** computes the result.
  4. **Worker** emits a single Op: `Complete(Result)`.
  5. **Owner** merges the result.

This allows offloading compute to ephemeral peers without exposing the project's edit history or sensitive development timeline.


