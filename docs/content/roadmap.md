---
title: "Roadmap"
---

## Completed

| Milestone        | Summary                                                                                                                                       |
|------------------|-----------------------------------------------------------------------------------------------------------------------------------------------|
| **M1–M3**        | Single-node log with Intention DAG/HLC, DAG conflict resolution (LWW), async actor pattern, two-node sync via Iroh, Mesh API with token-based join |
| **M4**           | Generic RSM platform: extracted `lattice-model`, `lattice-kvstore`, WAL-first persistence, gRPC introspection                                 |
| **M5**           | Atomic multi-key transactions: `batch().put(k1,v1).put(k2,v2).commit()`                                                                       |
| **M6**           | Multi-store: root store as control plane, StoreManager with live reconciliation, per-store gossip                                             |
| **Decoupling**   | Node/Net separation via `NodeProvider` traits, `NetEvent` channel owned by net layer, unified StoreManager                                    |
| **Kernel Audit** | Orphan resolution refactor, `MAX_CAUSAL_DEPS` limit, zero `.unwrap()` policy, clean module hierarchy                                          |
| **M7**           | Client/Daemon split: library extraction, `latticed` daemon with UDS gRPC, `LatticeBackend` trait, RPC event streaming, socket security        |
| **M8**           | Reflection & introspection: `lattice-api` crate, deep type schemas, `ReflectValue` structured results, store watchers & FFI stream bindings   |
| **M10**          | Fractal Store Model: replaced `Mesh` struct with recursive store hierarchy, `TABLE_SYSTEM` with HeadList CRDTs, `RecursiveWatcher` for child discovery, invite/revoke peer management, `PeerStrategy` (Independent/Inherited), flattened `StoreManager`, `NetworkService` (renamed from `MeshService`), proper `Node::shutdown()` |
| **M11**          | Weaver Migration & Protocol Sync: Intention DAG (`IntentionStore`), Negentropy set reconciliation, Smart Chain Fetch, stream-based Bootstrap (Clone) Protocol without gossip storms |
| **M12**          | Network Abstraction & Simulation: `Transport`/`GossipLayer` traits, `IrohTransport` extracted to `lattice-net-iroh`, `ChannelTransport`/`BroadcastGossip` in `lattice-net-sim`, gossip lag tracking, event-driven gap handling, `SessionTracker` decoupling, symmetric Negentropy sync |

---

## Milestone 13: Crate Dependency Architecture

Remove improper architectural couplings and establish clean abstraction boundaries between crates.

- [x] **Bindings → Model:** Removed phantom `lattice-model` from `lattice-bindings` (zero imports). `lattice-store-base` stays (used for `StreamDescriptor`).
- [ ] **Network Isolation:** Remove `lattice-kernel` from `lattice-net` — move proto types to `lattice-proto`, move `intention_from_proto`/`intention_to_proto` converters, elevate `MissingDep`/`StateError`/`PeerSyncInfo` to `lattice-store-base`.
- [ ] **Network Types Isolation:** Remove `lattice-kernel` from `lattice-net-types` — elevate `SyncProvider`, `IngestResult`, `StateError` to `lattice-store-base`.
- [ ] **Store Hierarchy:** Investigate and abstract `lattice-systemstore` dependency on `lattice-kernel`.
- [x] **API Boundary:** Removed phantom `lattice-net` from `lattice-api` (zero imports).
- [ ] **Node Boundary:** Investigate `lattice-node` direct concrete store dependencies.

---

## Milestone 14: Intelligent Reconciliation ("Meet" Operator)

Add the "Meet" operator: find the common ancestor of two divergent heads, extract the deltas, and attempt a 3-way merge.

### 14A: Kernel Primitives
- [ ] **Store Trait Abstraction:** Decouple `StateMachine` from `redb`. Introduce a generic `Store` trait (get/put/del) that can be implemented by both `RedbStore` (disk) and `MemoryStore` (RAM). This is the shared "ephemeral replay" primitive: replay a subset of intentions into a temporary state without mutating live storage. Used by the Meet operator (replay LCA → each head) and frontier snapshotting in M15B (replay last snapshot → frontier).
- [ ] **LCA Index:** Maintain an efficient index (likely `Hash -> (Height, Parents)`) to avoid O(N) scans. "Height" allows fast-forwarding the deeper node before scanning for intersection.
- [ ] **Meet Query (`find_lca`):** `fn find_lca(hash_a, hash_b) -> Result<Hash>`
- [ ] **Diff Query (`get_path`):** `fn get_path(from, to) -> Result<Vec<Intention>>`
    - Returns the list of operations to replay from the Meet (common ancestor) to the Head.
    - Usage: `get_path(LCA, Head_A)` yields Alice's local changes; `get_path(LCA, Head_B)` yields Bob's.

### 14B: The "Meet" Operator (Core Logic)
- [ ] **Computation:** When two divergent Heads (A and B) are detected, use `find_lca` to locate M.
- [ ] **Delta Extraction:** Use `get_path` to compute ΔA (`M->A`) and ΔB (`M->B`).
- [ ] **3-Way Merge:** Attempt to apply both ΔA and ΔB to M.
    - **Non-Conflicting:** If they touch different fields, both are applied. The DAG collapses from 2 heads back to 1.
    - **Conflicting:** If they touch the exact same field, the conflict is surfaced to the user.

### 14C: Store Integration (`lattice-kvstore`)
- [ ] **Patch/Delta Operations:** Introduce operations that describe mutations (e.g., "increment field X", "set field Y") instead of simple overwrites.
- [ ] **Read-Time Merge:** Update `get()` to check for conflicting Heads and invoke meet logic dynamically.
- [ ] **Snapshotting:** Cache values at specific "Checkpoints" (common ancestors) so calculating the state at M doesn't require replaying the entire history.

---

## Milestone 15: Log Lifecycle & Pruning

Manage log growth on long-running nodes via stability frontier, snapshots, pruning, and finality checkpoints.

> **See:** [Stability Frontier](stability-frontier.md) for the full design.

### 15A: Stability Frontier & Tip Attestations
- [ ] `SystemOp::TipAttestation { tips: Vec<(PubKey, Hash)> }` — publish changed author tips as SystemOps
- [ ] Attestation keys in `TABLE_SYSTEM`: `attestation/{attester}/{author} → Hash` (LWW by HLC)
- [ ] Derive per-author frontier: `frontier[A] = min(tip[A] across all Active peers)`
- [ ] Attestation triggers: post-sync, batch threshold, periodic heartbeat (~15 min), graceful shutdown
- [ ] **Lag metric:** Per-peer divergence from local tips, surfaced via `NodeEvent::PeerLagWarning` and `store status`

### 15B: Snapshotting
- [ ] **Frontier snapshot via replay:** The frontier is behind the current state. Generating a snapshot at the frontier requires replaying intentions from the last snapshot up to the frontier cut, producing the correct state at that point. Uses the ephemeral `MemoryStore` from M14A.
- [ ] Store snapshots in `snapshot.db` (per-store, includes `TABLE_SYSTEM` attestation state)
- [ ] Bootstrap new peers from snapshot + tail sync instead of full log replay
- [ ] **Future optimization:** Inverse operations stored in `WitnessRecord` could allow rolling back from current state to frontier in O(head − frontier), avoiding replay. Deferred until profiling shows replay is a bottleneck.

### 15C: Pruning
- [ ] `truncate_prefix` for all intentions per author up to `frontier[A]`
- [ ] Discard individual attestation intentions below the frontier (snapshot carries state forward)
- [ ] Preserve intentions newer than frontier

### 15D: Checkpointing / Finality
- [ ] Periodically finalize state hash (protect against "Deep History Attacks")
- [ ] Signed checkpoint intentions in sigchain
- [ ] Nodes reject intentions that contradict finalized checkpoints

### 15E: Recursive Store Bootstrapping (Pruning-Aware)
- [ ] `RecursiveWatcher` identifies child stores from live state, not just intention logs.
- [ ] Bootstrapping a child store requires a **Two-Phase Protocol** because Negentropy cannot sync from an implicit zero-genesis if the store has been pruned.
- [ ] **Phase 1 (Snapshot):** Request the opaque snapshot (`state.db`) from the peer for the discovered child store.
- [ ] **Phase 2 (Tail Sync):** Run Negentropy to sync floating intentions that occurred *after* the snapshot's causal frontier.
- [ ] **Replace Polling with Notify:** `register_store_by_id` and boot sync use `sleep()` polling loops. Replace with `tokio::sync::Notify` or channel-based signaling.

### 15F: Hash Index Optimization ✅
- [x] Replace in-memory `HashSet<Hash>` with on-disk index (`TABLE_WITNESS_INDEX` in redb)
- [x] Support 100M+ intentions without excessive RAM

### 15G: Advanced Sync Optimization (Future)
- [ ] **Modular-Add Fingerprints:** Replace XOR-based fingerprints with modular addition (mod 2^256). XOR is linear and cancels duplicates (`a ⊕ a = 0`); mod-add is strictly more robust at identical cost. Affected sites:
  - `IntentionStore::xor_fingerprint()` / `derive_table_fingerprint()` / `fingerprint_range()` in `lattice-kernel`
  - `SyncProvider` trait docs in `lattice-kernel/src/sync_provider.rs`
  - Reconciler mock in `lattice-sync` tests
- [ ] **Persistent Merkle Index / Range Accumulator:**
  - Avoid O(N) scans for range fingerprints (currently linear)
  - Pre-compute internal node hashes in a B-Tree or Merkle Tree structure
- [ ] **High-Radix Splitting:**
  - Increase branching factor (e.g., 16-32 children) to reduce sync rounds (log32 vs log2)
  - Parallelize range queries

---

## Milestone 16: Content-Addressable Store (CAS)

Node-local content-addressable blob storage. Replication policy managed separately. Requires M11 and M12.

### 16A: Low-Level Storage (`lattice-cas`)
- [ ] `CasBackend` trait interface (`fs`, `block`, `s3`)
- [ ] Isolation: Mandatory `store_id` for all ops
- [ ] `redb` metadata index: ARC (Adaptive Replacement Cache), RefCounting
- [ ] `FsBackend`: Sharded local disk blob storage

### 16B: Replication & Safety (`CasManager`)
- [ ] `ReplicationPolicy` trait: `crush_map()` and `replication_factor()` from System Table
- [ ] `StateMachine::referenced_blobs()`: Pinning via State declarations
- [ ] Pull-based reconciler: `ensure(cid)`, `calculate_duties()`, `gc()` with Soft Handoff

### 16C: Wasm & FUSE Integration
- [ ] **Wasm**: Host Handles (avoid linear memory copy)
- [ ] **FUSE**: `get_range` (random access) and `put_batch` (buffered write)
- [ ] **Encryption**: Store-side encryption (client responsibility)

### 16D: CLI & Observability
- [ ] `cas put`, `cas get`, `cas pin`, `cas status`

---

## Milestone 17: Lattice File Sync MVP

File sync over Lattice. Requires M11 (Sync) and M15 (CAS).

### 17A: Filesystem Logic
- [ ] Define `DirEntry` schema: `{ name, mode, cid, modified_at }` in KV Store
- [ ] Map file operations (`write`, `mkdir`, `rename`) to KV Ops

### 17B: FUSE Interface
- [ ] Write `lattice-fs` using the `fuser` crate
- [ ] Mount the Store as a folder on Linux/macOS
- [ ] **Demo:** `cp photo.jpg ~/lattice/` → Syncs to second node

---

## Milestone 18: Wasm Runtime

Replace hardcoded state machines with dynamic Wasm modules.

### 18A: Wasm Integration
- [ ] Integrate `wasmtime` into the Kernel
- [ ] Define minimal Host ABI: `kv_get`, `kv_set`, `log_append`, `get_head`
- [ ] Replace hardcoded `KvStore::apply()` with `WasmRuntime::call_apply()`
- [ ] Map Host Functions to `StorageBackend` calls (M9 prerequisite)

### 18B: Data Structures & Verification
- [ ] Finalize `Intention`, `SignedIntention`, `Hash`, `PubKey` structs for Wasm boundary
- [ ] Wasm-side Intention DAG verification (optional, for paranoid clients)
- **Deliverable:** A "Counter" Wasm module that increments a value when it receives an Op

---

## Milestone 19: N-Node Simulator

Scriptable simulation framework for testing Lattice networking at scale. Built on the `lattice-net-sim` crate (M12B).

- [ ] **Simulator library** (`Simulator` API): Scriptable — `add_node`, `take_offline`, `bring_online`, `join_store`, `put`, `sync`, `assert_converged`
- [ ] **Rhai scripting**: Embed Rhai for scenario scripts (loops, conditionals, dynamic topology changes)
- [ ] **Standalone binary**: CLI that loads and runs `.rhai` scenario files
- [ ] **Fix flaky `test_large_dataset_sync`:** Intermittent partial sync failures (misses items). Likely race between auto_sync boot task and explicit `sync_all_by_id`. Currently mitigated by disabling auto_sync in test.
- [ ] **Gate:** 20+ node convergence simulation with metrics: sync calls, items transferred, convergence %, wall-clock time

---

## Milestone 20: Embedded Proof ("Lattice Nano")

Run the kernel on the RP2350.

> Because CLI is already separated from Daemon (M7) and storage is abstracted (M9), only the Daemon needs porting.
> **Note:** Requires substantial refactoring of `lattice-kernel` to support `no_std`.

### 20A: `no_std` Refactoring
- [ ] Split `lattice-kernel` into `core` (logic) and `std` (IO)
- [ ] Replace `wasmtime` (JIT) with `wasmi` (Interpreter) for embedded target
- [ ] Port storage layer to `sequential-storage` (Flash) via `StorageBackend`

### 20B: Hardware Demo
- [ ] Build physical USB stick prototype
- [ ] Implement BLE/Serial transport
- [ ] Sync a file from Laptop → Stick → Phone without Internet

---

## Technical Debt

- [ ] **REGRESSION**: history command list filtering (backend side) capability
- [ ] **REGRESSION**: Graceful reconnect after sleep/wake (may fix gossip regression)
- [ ] **Store Name Lookup Optimization**: `find_store_name()` in `store_service.rs` and `backend_inprocess.rs` does O(meshes × stores) linear search. Store names live in mesh root KV stores (StoreDeclaration). Consider caching in StoreManager or adding index.
- [ ] **Data Directory Lock File**: Investigate lock file mechanism to prevent multiple processes from using the same data directory simultaneously (daemon + embedded app conflict). Options: flock, PID file, or socket-based detection.
- [ ] **Denial of Service (DoS) via Gossip**: Implement rate limiting in GossipManager and drop messages from peers who send invalid data repeatedly.
- [ ] **Payload Validation Strategy**: Decide where semantic validation occurs and what happens on failure. Options: build-time only, versioned rules, intention replacement, or separate chain/payload advancement. See `test_rejected_entry_breaks_chain` in `lattice-kvstore/src/kv.rs`.
- [ ] **Signer Trait**: Introduce a `Signer` trait (sign hash → signature) to avoid passing raw `SigningKey` through the stack. Affects intention creation (`SignedIntention::sign`), witness signing (`WitnessRecord::sign`), and the M11 migration path.
- [ ] **Optimize `derive_table_fingerprint`**: Currently recalculates the table fingerprint from scratch. For large datasets, this should be optimized to use incremental updates or caching to avoid O(N) recalculation.
- [ ] **Sync Trigger & Bootstrap Controller Review**: Review how and when sync is triggered (currently ad-hoc in `active_peer_ids` or `complete_join_handshake`). Consider introducing a dedicated `BootstrapController` to manage initial sync state, retry logic, and transition to steady-state gossip/sync.

---

## Active Migrations & Backfills

- [x] **System Table Type Backfill** (`backfill_child_types` in `mesh.rs`)
  - **Purpose**: Populates `store_type` in System Table for legacy stores that have `type="unknown"`.
  - **Mechanism**: On startup, checks local disk headers for unknown stores and issues `ChildAdd` op.
  - **Completion Condition**: Can be removed once all nodes have cycled and updated the System Table.

- [ ] **Legacy Root Store Data Migration** (`migrate_legacy_data` in `mesh.rs`)
  - **Purpose**: Moves root store declarations from `/stores/` keys to System Table.
  - **Mechanism**: One-time migration on startup.
  - **Destructive**: Deletes legacy keys after successful migration.

- [x] **Legacy Peer Data Migration** (`migrate_legacy_peer_data` in `mesh.rs`)
  - **Purpose**: Moves peer status/metadata from raw keys to System Table.
  - **Mechanism**: One-time migration on startup.

- [ ] **StateDB Table Renaming** (`state_db.rs`)
  - **Purpose**: Renames legacy "kv" or "log" redb tables to standard "data" table.
  - **Mechanism**: Checked on every `StateBackend` open.

- [ ] **Legacy Store Type Aliases** (`lattice-model`, `lattice-runtime`)
  - **Purpose**: Supports opening stores with legacy type strings ("kvstore", "logstore").
  - **Mechanism**: `StoreRegistry` maps these to modern equivalents.

---

## Future

- TTL expiry for long-lived orphans (received_at timestamp now tracked)
- Mobile clients (iOS/Android)
- **Root Identity & Key Hierarchy**: Four-layer key model: Seed (24 words, offline) → Root Identity (Ed25519, signs authorizations only) → Device Keys (per-node, sign intentions) → DAG operations. Peers tracked by root identity in SystemStore; each root identity has an authorized device key list. New SystemOps: `DeviceAuthorize(root, device, sig)`, `DeviceRevoke(root, device)`. Device keys validated against authorization chain. Existing DAG/chain/gossip/sync works unchanged at the device-key level.
- **Seed-Based Disaster Recovery**: BIP-39 mnemonic (24 words) generated during onboarding. Recovery flow: enter seed on fresh device → regenerate identity → connect vault-node → revoke lost device keys → resume. Setup flow includes a recovery drill (verify backup words before setup is complete).
- **Shamir Key Splitting**: Split recovery seed into N shares, require K to reconstruct (e.g. 3 shares, any 2 recover). Shares printed as QR codes on labeled cards. For high-value / paranoid use cases.
- **Vault-Node Assisted Recovery**: Physical recovery button or PIN sequence puts vault into pairing mode. Two-factor: physical access + seed/recovery PIN. Optional e-ink challenge-response display.
- **Vault-Node Autonomous Updates**: Authenticated software update mechanism with rollback for vault-nodes when owner is unreachable. Authenticated against root identity.
- Secure storage of node key (Keychain, TPM)
- **Salted Gossip ALPN**: Use `/config/salt` from root store to salt the gossip ALPN per mesh (improves privacy by isolating mesh traffic).
- **HTTP API**: External access to stores via REST/gRPC-Web (design TBD based on store types)
- **Inter-Store Messaging** (research): Stores exchange typed messages using an actor model. Parent stores have a store-level identity (keypair derived from UUID + node key). Children trust the parent's store key — not individual parent authors — preserving encapsulation. Parent state machines decide what to relay; child state machines process messages as ops in their own DAG. Key design questions: message format, delivery guarantees (at-least-once via DAG?), bidirectional messaging (child→parent replies), and whether messages are durable (intentions) or ephemeral.
- **Audit Trail Enhancements** (HLC `wall_time` already in Intention):
  - Human-readable timestamps in CLI (`store history` shows ISO 8601)
  - Time-based query filters (`store history --from 2026-01-01 --to 2026-01-31`)
  - Identity mapping layer (PublicKey → User name/email)
  - Tamper-evident audit export (signed Merkle bundles for external auditors)
  - Optional: External audit sink (stream to S3/SIEM)
- **USB Gadget Node**: Hardware device (Pi Zero, RISC-V dongle) that enumerates as a USB Ethernet gadget and runs a Lattice node, peering with the host over the virtual interface.
- **Blind Ops / Cryptographic Revealing** (research): Encrypted intention payloads revealed only to nodes possessing specific keys (Convergent Encryption or ZK proofs). Enables selective disclosure within atomic transactions.
- **S-Expression Intention View**: Enhance `store history` and `store debug` to optionally display Intentions as S-expressions, providing a structured, verifiable view of the underlying data for debugging.
- **Async/Task-Based Bootstrapping**:
    - Treat bootstrapping as a persistent state/task (`StoreState::Bootstrapping`).
    - Retry indefinitely if peers are unavailable.
    - Recover from "not yet bootstrapped" state on restart.
    - Inviter sends list of potential bootstrap peers in `JoinResponse`.
    - Node can bootstrap from any peer in the list, not just the inviter.
- **Blind Node Relays**: Untrusted VPS relays that sync the raw Intention DAG via Negentropy. No store keys, no Wasm. Can perform graph-based pruning using the `state_independent` flag: prune linear sub-chains below state-independent intentions at the frontier (pure graph operation, no state machine). Full nodes can also push computed snapshots to relays, making them full bootstrap sources. Two-tier pruning: relays do structural pruning (safe by construction), full nodes do semantic pruning (more compact).
