---
title: "Connected DAG & Genesis Commitment"
status: design
weight: 4
---

> **Status**: Design

Every store's causal DAG should be a connected graph, rooted at a deterministic genesis intention. The genesis hash becomes the store's **external identity** — committed in the parent's `ChildAdd` for child stores and in the invite token for root stores. Epochs chain back to the genesis, forming a sparse spine through history. The full store lifecycle — creation, operation, archival, and destruction — is modeled through the parent's SystemTable.

## Problem

The causal DAG (`Condition::V1` edges) currently allows disconnected subgraphs. Each author's first intention can have `Condition::V1(vec![])`, producing independent chains with no cross-author causal link. The `store_prev` chains keep each author's history linear, but cross-author connectivity is entirely opt-in.

This has consequences:

1. **DAG queries degrade.** `find_lca`, `is_ancestor`, and `get_path` are undefined or degenerate for pairs of intentions in disconnected subgraphs. Algorithms must special-case `Hash::ZERO` as a virtual root.
2. **Weak causal record.** An intention with empty conditions makes no claim about what state the author observed. The DAG cannot distinguish "author saw nothing" from "author chose not to declare dependencies."
3. **No cross-store integrity.** The parent's `ChildAdd` declares a child exists (by UUID), but nothing ties it to the child's actual content. A forged child store with the correct UUID is indistinguishable from the real one.
4. **UUID-based identity is weak.** Stores are identified by random UUIDs that carry no commitment to content. Two stores with the same UUID but different histories are indistinguishable without replaying their full DAGs.
5. **No destruction protocol.** "Deleting" a store is a local soft-archive (`ChildSetStatus(Archived)`). There is no mechanism for the mesh to agree on data destruction, no way for a peer returning from offline to learn that a store was destroyed, and no audit trail distinguishing "archived" from "permanently deleted."

## Design

### Part 1: Genesis Intention

Every store has a **genesis intention** — the first intention ever witnessed in that store. It is the root of the causal DAG.

```rust
Intention {
    author,
    timestamp: HLC::now(),
    store_prev: Hash::ZERO,   // first write by this author
    causal_deps: vec![],      // no prior intentions exist
    ops: GenesisOp { store_type, nonce },
}
```

```protobuf
message GenesisOp {
    string store_type = 1;  // e.g. "system", "core:kvstore"
    uint32 nonce = 2;       // random — ensures unique genesis hash
}
```

The genesis hash (`blake3(borsh(genesis_intention))`) **is** the store ID. There is no UUID, and there is no `store_id` field on `Intention`. Intentions do not carry an explicit store identifier — store membership is determined by the DAG structure. Every non-genesis intention has non-empty `causal_deps` that chain back to the genesis. The genesis hash is used everywhere: database paths, in-memory maps, sync protocol, invite tokens, `ChildAdd`.

During sync, the protocol messages already specify which store an intention belongs to. The receiving peer verifies membership by checking that the intention's `causal_deps` resolve to intentions already in that store's log. An intention whose deps don't exist in the store is rejected (or floats until the deps arrive via the same store's sync).

The genesis payload contains `store_type` and a random `nonce`. Mutable metadata (name, peer strategy, peer status) belongs in subsequent intentions, not in the genesis. Since the genesis hash is the store's permanent identity, everything in the genesis is immutably bound to it.

The `nonce` is necessary because the other genesis fields can collide: `author` is the same for all stores created by one peer, `timestamp` uses the node's global HLC which is only monotonic per-store (the genesis is the first intention — there's no prior HLC state for that store), and `store_type` is often identical across stores. Without the nonce, two `create_store` calls in quick succession from the same peer with the same type could produce identical genesis hashes.

The genesis is the only intention permitted to have empty `causal_deps`. All subsequent intentions must include at least one hash — epoch 0 cites genesis, and all system/data ops cite epoch 0 (or a later epoch) directly or transitively.

### Part 2: Genesis Hash as Store Identity

The genesis hash (`blake3(borsh(genesis_intention))`) **is** the store ID — the only identifier. There is no UUID, and no `store_id` field on `Intention`. The genesis hash is used everywhere: database directory names, in-memory maps, `StoreManager` keys, sync protocol handshakes, invite tokens, `ChildAdd`, and `TABLE_META`.

The genesis `ops` payload uses a known kernel-level format (`GenesisOp { store_type, nonce }`). Unlike all other intentions — whose `ops` are opaque application payloads decoded only by the state machine — the genesis `ops` are decoded by the `IntentionStore` at the infrastructure level. At creation time, the identity fields (`store_id` = genesis hash, `store_type`) are cached in `TABLE_META` in `log.db`. The genesis hash makes the cached `store_type` verifiable: given the genesis intention bytes, any node can recompute the hash and confirm it matches.

This means:

- **Invite tokens** contain the genesis hash as `store_id`.
- **`ChildAdd` ops** reference children by genesis hash.
- **Sync protocols** use genesis hash to address stores.
- **Database paths** use the genesis hash (hex-encoded or base58) as directory names.
- **`store_type`** is cached in `TABLE_META` at creation time. The genesis payload uses a known format (`GenesisOp { store_type, nonce }`) that the `IntentionStore` can decode without the state machine — it is the only intention whose `ops` format is defined at the infrastructure level rather than being opaque to the store layer.
- **`store_id` removed from `Intention`**. Store membership is determined by the DAG structure — every non-genesis intention's `causal_deps` chain back to the genesis. During sync, protocol messages specify the target store; the receiving peer verifies by checking that deps resolve within the store's log.

### Part 3: Connected Graph Invariant

**Simplification:** The `Condition` enum (`Condition::V1(Vec<Hash>)`) is replaced with a plain `causal_deps: Vec<Hash>` field on `Intention`. There is no need for a versioned wrapper — causal deps are a list of hashes pointing to other intentions in the DAG. If a richer format is ever needed, it can be added then.

**Rule:** Only the genesis may have empty `causal_deps`. Every other intention — including epochs, epoch acks, system ops, and data ops — must have non-empty `causal_deps`. Epoch 0 cites genesis. Subsequent epochs cite genesis + all author tips. System and data ops cite the current epoch or a descendant.

This is enforced at **witnessing time** — the same point where `store_prev` linearity and dependency resolution are already checked. An intention with empty `causal_deps` (that is not the store's genesis) is rejected. A second intention with `store_prev: Hash::ZERO` and empty `causal_deps` is also rejected (at most one genesis per store).

The practical effect: every author's first write to a store must cite at least one existing intention. Since epoch 0 is always present, this is always satisfiable — the author includes the current epoch (or any descendant) in their `causal_deps`. This proves they have synced at least once before writing.

#### Two partitions

A store's DAG has two logical partitions that form separate subgraphs:

- **System partition** — peer management, store names, hierarchy, invites. Deps come from `_get_deps()` on `TABLE_SYSTEM` keys.
- **Data partition** — application data (kvstore keys, etc.). Deps come from `_get_deps()` on `TABLE_DATA` keys.

Both partitions root at genesis (via epoch 0 — see below), but they do not cross-reference each other. A data op's `causal_deps` never cites a system op, and vice versa. This means a data op cannot prove, through DAG structure alone, that its author saw a particular system change (e.g., a peer revocation).

#### Epoch intentions — bridging the partitions

**Epoch intentions** are kernel-level ops that bridge the system and data partitions. An epoch captures a snapshot of the full DAG frontier — citing genesis and all author tips — creating a point that both partitions can reference.

```protobuf
message EpochOp {
    uint64 seq = 1;                    // monotonic epoch counter (0 = first epoch)
    repeated bytes required_acks = 2;  // pubkeys that must ack for settlement
}
```

The epoch payload embeds the acker set so it is self-contained after pruning. The kernel receives the list from the SSM (via `EpochRequired`) and stores it in the payload. The kernel does not interpret peer semantics — it treats pubkeys as opaque identifiers for settlement tracking.

`required_acks` is the subset of peers whose acknowledgment is needed for settlement. The SSM decides this based on its peer governance model — read-only or worker peers that never write may be excluded. Peers not in `required_acks` can handle the epoch transition by re-syncing from a settled peer; they don't block settlement. The epoch creator is always excluded from `required_acks` (creating the epoch is their implicit ack).

Revocation information is NOT embedded in the epoch payload. It is recoverable from the epoch's `causal_deps`: the epoch cites the revoker's author tip, and the revocation intention is transitively reachable from that tip (via the revoker's `store_prev` chain). After pruning, the revoker's tip survives as a direct dep of the epoch, and the revocation intention is reachable from it. The SSM re-derives revocation state by re-projecting from the frontier tips.

**Epoch 0** is created by the actor during projection of the initial system batch (which includes `AddPeer(self)`). The SSM detects "peers exist, no epoch" and returns `EpochRequired { required_acks: [] }`. Epoch 0's `causal_deps` cite genesis and the creator's tip (the system batch). Subsequent system and data ops cite epoch 0.

**Subsequent epochs** are triggered by membership-critical system changes. The epoch's `causal_deps` always cite:
- **Genesis** — maintains the sparse spine (genesis ← epoch N, one hop)
- **All author tips** — a complete DAG frontier snapshot

```
genesis ← epoch 0 ← system ops (add_peer, etc.)
                   ← data ops (put x=1, etc.)

           system ops → revoke_peer C
           revoke_peer C → epoch 1 (deps: [genesis, A_tip, B_tip, C_tip])

           epoch 1 ← epoch_ack (explicit acknowledgment from each peer)
                   ← data ops (may cite epoch 1 transitively via CRDT deps)
```

For **revoked authors**, the cited tip in the epoch is a hard cutoff — intentions from that author beyond the frontier are excluded from projection. For **active authors**, the cited tip is informational — their writes beyond it remain valid because they are still authorized peers.

**Operations that trigger a new epoch:**
- **PeerRevoke** — remaining peers must prove they saw the revocation.
- **Epoch key rotation** — proves the next intention was created with knowledge of the new key.
- **PruneCut** — proves the author saw the pruning decision; future intentions won't reference pruned state.

**Not epoch triggers:** `SetStoreName`, `PeerAdd`, `ChildAdd`, `InviteOp` — these don't have security implications that require forced acknowledgment.

#### Epoch creation: actor-driven, local writes only

Epochs are created by the actor during the **local write path** (`Submit` command), never during sync ingestion. The flow:

1. A locally-authored system op (e.g., `revoke_peer C`) is submitted via `SystemBatch::commit()` → `Submit` command to the actor.
2. The actor processes the intention: insert → witness → project. During projection, the SSM's `apply()` processes the system op and returns `SystemEvent::EpochRequired { required_acks }`. The acker set is computed by the SSM from its own state — the kernel never reads `TABLE_SYSTEM` directly. `required_acks` is the subset of active peers whose acks are needed for settlement (excluding read-only or worker peers that can re-sync independently, and excluding the epoch creator who has an implicit ack).
3. The actor detects `EpochRequired` in the projection result. Since it's processing a `Submit` (local write), it creates an `EpochOp { seq: current + 1, required_acks }` intention with `causal_deps: [genesis, ...all_author_tips]`. This epoch intention goes through the same insert → witness → project path inline, before the `Submit` command returns.
4. The actor caches `required_acks` in memory for settlement tracking.

**Why only on local writes:** When a synced intention (via `IngestBatch`) triggers a membership change, the SSM's `apply()` may also return `EpochRequired`. The actor ignores it because the authoring node already created (or will create) the epoch — it will arrive via sync. This prevents competing epochs from multiple nodes.

The SSM owns the **policy** (when is an epoch needed? who must ack?). The kernel owns the **mechanics** (building the epoch, embedding the acker set, tracking settlement, authorizing pruning). Neither knows the other's internals beyond the event interface.

On restart, the kernel re-derives the acker set from the latest epoch's `required_acks` field — no need to query the SSM. The epoch is the single source of truth for its own settlement criteria.

#### Epoch acknowledgment

Each active peer must acknowledge a new epoch by submitting an `EpochAckOp` — a kernel-level no-op whose `causal_deps` cite the epoch hash and the peer's current tip.

```protobuf
message EpochAckOp {
    uint64 epoch_seq = 1;  // which epoch this ack is for
}
```

The **epoch creator** does not submit a separate ack — creating the epoch IS their acknowledgment (the epoch is in their `store_prev` chain, so their author tip transitively cites it). The kernel uses the epoch's `required_acks` field directly — the creator is already excluded by the SSM.

When a node receives a new epoch, it automatically submits an `EpochAckOp`. If the node has pending writes, the ack can be combined with the next write (the write's `causal_deps` cite the epoch, which counts as an implicit ack). The explicit `EpochAckOp` is only needed when the node has nothing else to write.

#### Epoch settlement and pruning safety

An epoch is **settled** when every peer in the epoch's `required_acks` list has an intention that transitively cites the epoch hash. The kernel tracks this by checking author tips against `required_acks`, which is read from the epoch payload on creation (or re-derived from it on restart).

Once an epoch is settled:
- All pre-epoch history is safe to prune (no peer will request it during sync)
- Orphaned branches from revoked peers are garbage collected
- The DAG converges to the sparse spine: `genesis ← epoch N ← acks + tail`

A peer that never acks (permanently offline) blocks epoch settlement. This is resolved by removing the stale peer from the active peer set via a system op — which triggers a new epoch whose `required_acks` excludes the stale peer.

#### Revocation enforcement via epochs

Revocation is enforced at three layers:

**1. Gossip topic rotation (transport-level).** The gossip topic (iroh `TopicId`) is derived from the store ID **and the current epoch hash**: `content_hash("lattice/{store_id}/{epoch_hash}")`. When a new epoch is created, active peers switch to a new gossip topic. The revoked peer is still subscribed to the old topic — they never see post-epoch gossip messages because they're on a different channel entirely. No filtering logic needed at the gossip layer.

As a defense-in-depth measure, the gossip ingester also checks `can_accept_gossip()` and rejects broadcasts from revoked peers (in case a revoked peer discovers the new topic). Outbound gossip stops broadcasting to revoked peers.

**Multi-epoch gossip participation.** During the transition window between epochs, a node subscribes to multiple gossip topics simultaneously. When epoch N is created, peers that haven't acked yet are still on the epoch N-1 topic. The node listens on both topics to receive their writes and epoch acks. Once epoch N is settled (all `required_acks` received), the node unsubscribes from epoch N-1's topic. In practice, a node participates in at most 2 concurrent epochs (current + previous). Settled epochs are dropped.

**2. Negentropy sync (epoch-scoped).** The sync handshake includes the connecting peer's identity. If the responder's current epoch revokes that peer, negentropy is scoped to intentions witnessed up to and including the revoking epoch. The revoked peer receives everything up to the epoch (including the epoch itself — so they learn about the revocation) but nothing after.

The negentropy sync range is defined as a **DAG partition**, not a witness seq range. The epoch's `causal_deps` precisely partition the DAG:
- **Below the epoch** (reachable from the epoch's deps): the snapshot subgraph. Excluded from negentropy fingerprints.
- **Above the epoch** (not reachable from deps): the tail. Included in negentropy sync.

Transition-window writes from active peers (created without epoch awareness but beyond the frontier) are NOT reachable from the epoch's deps — they are correctly included in the sync range.

This means:
- For revoked peers: negentropy is scoped to the epoch's ancestor subgraph only. They receive intentions reachable from the epoch's deps (which they mostly already have) plus the epoch itself. Nothing above.
- The revoked peer's own post-revocation writes are not accepted during sync with them — there is no convergence goal with a peer outside the mesh.
- Between active peers, the full above-epoch range is synced. Intentions from a revoked author that arrived before the epoch (via earlier sync) are part of the ancestor subgraph and not re-synced.

Each node tags intentions as "below epoch N" via a reachability walk from the epoch's `causal_deps`. Once tagged, the negentropy fingerprint is computed over the untagged set. This tagging shares mechanics with pruning (18D) — the same reachability walk determines what is prunable.

**3. Bootstrap.** Refused entirely for revoked peers — they are not part of the mesh.

**4. Projection (state-level).** For intentions from revoked authors that are already in the log (synced between active peers before revocation awareness propagated):
- Intentions **beyond the epoch frontier** (not reachable from the epoch's deps) are excluded from materialized state. The SSM determines who is revoked from its own projected state — the kernel queries it via a `ProjectionFilter` trait.
- Intentions from all other authors beyond the epoch frontier are applied normally.
- This is a local check: the kernel asks the SSM "is this author revoked?", and checks whether the intention is reachable from the epoch's deps.

On a node that already applied a revoked peer's post-frontier writes (because the epoch hadn't arrived yet), receiving the epoch triggers re-projection: the state for keys affected by the revoked peer is re-evaluated using only intentions reachable from the epoch frontier. This re-projection is scoped to the revoked peer's data — active peers' state is untouched.

### Part 4: Genesis Commitment in `ChildAdd`

When a parent store creates a child, the flow becomes:

1. Create the child store's genesis intention. Compute `genesis_hash = blake3(borsh(genesis_intention))`.
2. Write the `ChildAdd` to the parent store, keyed by genesis hash:

```protobuf
message ChildAdd {
    bytes genesis_hash = 1;     // blake3 hash of child's genesis intention (32 bytes)
    string alias = 2;
}
```

Both `target_id` and `store_type` are removed. The genesis hash **is** the child's store ID — no UUID to extract. The `store_type` is extracted from the genesis payload. The joining peer receives the genesis intention during sync, verifies `blake3(borsh(genesis)) == genesis_hash`, and reads `store_type` from it.

The parent's `ChildAdd` intention is signed and hashed into the parent's DAG, which is in turn rooted in *its* parent's DAG, all the way up to the root store. The child's genesis hash is now transitively committed to by the entire hierarchy.

### Part 5: Genesis Commitment in Invite Tokens

Root stores have no parent `ChildAdd`. Instead, the invite token commits to the genesis:

```protobuf
message InviteToken {
    bytes genesis_hash = 1;     // replaces store_id
    bytes secret = 2;
    bytes inviter_pubkey = 3;
}
```

The genesis hash **is** the store ID — there is no separate `store_id` field. The joining node receives the genesis intention during bootstrap and verifies that its hash matches the token. If it doesn't match, the bootstrap is rejected.

This makes the invite token a commitment to a specific store *and* its history root. The person who gives you the token is vouching for a specific cryptographic identity, not a random UUID.

### Part 6: Epochs and the Sparse Spine

The genesis intention is never pruned. Instead, it is kept alive by **epoch intentions** that causally chain back to it.

#### Epochs as Checkpoints

Every epoch cites genesis directly in its `causal_deps`. This creates the sparse spine — a chain of epoch checkpoints, each one hop from genesis:

```
genesis ←── epoch 0 ←── epoch 1 ←── epoch 2 ←── current tips
               pruned       pruned
```

Each epoch captures the full DAG frontier (all author tips) at a specific moment. The state at any epoch is deterministic: project all intentions reachable from the epoch's deps.

#### The epoch as a virtual barrier

The epoch is a **virtual barrier** — a reference point in the DAG, not a hard partition of the log. Active peers' writes flow freely across epoch boundaries. Transition-window writes (created by active peers without epoch awareness) are valid and applied normally. The epoch only acts as a hard cutoff for **revoked peers** — their writes beyond the frontier are excluded.

The epoch defines:
- A **deterministic snapshot boundary** — project everything reachable from the epoch's deps. Same result on all nodes.
- A **revocation cutoff** — revoked authors' writes beyond the frontier are excluded.
- A **pruning boundary** — once settled, the ancestor subgraph (everything reachable from deps) is prunable.
- A **negentropy sync range** — the complement of the ancestor subgraph is the sync range.

#### The Sparse Spine

Pruning happens in two phases:

**Phase 1 (epoch settled, acks still arriving):** Everything below the epoch frontier except the epoch's immediate deps (frontier tips) is pruned. The frontier tips are retained because some peers may still need them for sync catch-up.

**Phase 2 (all acks received):** Every active peer's `store_prev` has advanced past the epoch. The frontier tips are no longer needed — they are pruned too. The DAG converges to:

```
genesis ←── epoch_latest ←── acks + tail
```

Two layers remain:
1. **Genesis** — permanent identity, never pruned
2. **Latest epoch + tail** — epoch acks and subsequent writes

The epoch's `causal_deps` still reference the pruned frontier tips by hash, but those hashes are historical — no node needs to resolve them. The state at the epoch frontier was computed locally by each node (by projecting the frontier's intentions) and stored in `state.db` before pruning. This locally-computed **state snapshot** is served to new peers during bootstrap — it is NOT embedded in the epoch intention payload.

Everything between genesis and the epoch is gone. Intermediate epochs are also prunable once a newer epoch is settled.

#### Witness log split on epoch settlement

On epoch settlement, each node starts a **new witness log**. The old log is retained for lagging peers, then pruned. The new log is clean — no interleaving of below-epoch and above-epoch intentions:

```
New witness log:
  seq 0: genesis         (re-witnessed)
  seq 1: epoch N         (re-witnessed)
  seq 2: epoch_ack(s)    (re-witnessed)
  seq 3+: transition-window writes, then post-ack writes
```

The node re-signs the new witness chain locally (the witness log is node-local; each node is the authority over its own chain). The new log's epoch boundary is a clean seq cutoff (seq 1), eliminating the need for DAG reachability tagging during negentropy fingerprint computation.

Transition-window writes (created by active peers before seeing the epoch) may have `causal_deps` referencing intentions from the old log. These dangling deps are safe — the witness chain proves they were valid at witness time. Bootstrapping nodes trust the witness signatures.

See `docs/content/design/epoch-pruning.dot` and `docs/content/design/witness-log-epochs.dot` for diagrams.

#### Why the Genesis Survives

The genesis is never prunable because every epoch cites it directly. The pruning rule is: an intention is prunable when it is below the settled epoch frontier and no unpruned intention depends on it. Since every epoch includes genesis in its `causal_deps`, and the latest settled epoch is always retained, the genesis always has a live dependent.

No special pinning logic is needed. The genesis survives by the same causal dependency rules that govern all other intentions.

### Part 7: Store Creation Flow

The genesis and epoch 0 intentions are kernel-level operations. The `ReplicationController` creates them directly — the state machine never sees them (the projection loop skips `GenesisOp` and `EpochOp` intentions before calling `apply()`). They are encoded as `UniversalOp::Genesis(...)` and `UniversalOp::Epoch(...)` variants, distinct from the `UniversalOp::System(SystemOp)` and `UniversalOp::AppData(...)` variants used by all subsequent intentions.

#### Root Store Creation

1. Create the **genesis intention** — `GenesisOp { store_type, nonce }` with `store_prev: Hash::ZERO` and `causal_deps: []`.
2. Compute `store_id = blake3(borsh(genesis_intention))`. This is the store's permanent identity.
3. Create the store's databases (`log.db`, `state.db`) using the genesis hash as the directory name.
4. Commit the genesis intention to `log.db`.
5. Cache `store_id` (= genesis hash) and `store_type` in `TABLE_META`.
6. Commit a **system intention** — `SystemBatch { AddPeer(self), SetStoreName, SetPeerStrategy(Independent) }` — with `causal_deps` citing genesis. During projection, the SSM processes the peer add, detects "peers exist, no epoch," and returns `EpochRequired { required_acks: [] }`. The actor creates **epoch 0** inline — `EpochOp { seq: 0 }` with `causal_deps: [genesis, creator_tip]`.
7. Register in `meta.db` keyed by genesis hash.
8. Commit peer name and further intentions (citing epoch 0).

The genesis hash is included in invite tokens for this store. When another peer joins, it receives the genesis intention during bootstrap, verifies the hash, and reads `store_type` from it. Epoch 0 is created automatically by the actor — `StoreManager::create()` does not need to submit it explicitly.

#### Child Store Creation

1. Create the **genesis intention** — `GenesisOp { store_type, nonce }` with `store_prev: Hash::ZERO` and `causal_deps: []`.
2. Compute `store_id = blake3(borsh(genesis_intention))`.
3. Create the child store's databases using the genesis hash as the directory name.
4. Commit the genesis intention to `log.db`.
5. Cache `store_id` (= genesis hash) and `store_type` in the child's `TABLE_META`.
6. Commit a **system intention** to the child — `SystemBatch { AddPeer(self), SetStoreName, SetPeerStrategy(Inherited) }` — with `causal_deps` citing genesis. The actor creates **epoch 0** inline (same mechanism as root store creation).
7. Commit `ChildAdd(genesis_hash, alias)` to the **parent** store's SystemTable.
8. Commit `ChildSetStatus(genesis_hash, Active)` to the parent.

The ordering constraint is critical: the child's genesis must be fully committed (step 4) before the parent's `ChildAdd` (step 7). The genesis hash cannot be computed until the genesis intention exists.

### Part 8: Genesis-Rooted `store_prev`

Every author's `store_prev` chain is rooted at the genesis intention instead of `Hash::ZERO`. The genesis is the only intention permitted to have `store_prev: Hash::ZERO`.

#### Problem

Every author's first intention in a store currently uses `store_prev: Hash::ZERO`. This creates N independent chain roots (one per author) with a synthetic zero value that every code path must special-case:

- `witness_ready()` iterates all author tips *plus* `Hash::ZERO` to find new-author candidates.
- `detect_gap()` skips gap detection when `store_prev == Hash::ZERO`.
- `walk_back_until()` treats `Hash::ZERO` as a termination sentinel.
- `verify_and_update_tip()` has a separate branch for `Hash::ZERO`.
- `author_tip()` returns `Hash::ZERO` for unknown authors.

`Hash::ZERO` is not a real intention. It carries no proof of store membership and cannot be fetched, verified, or walked through.

#### Rule

For any intention `I` in a store:

- If `I` is the **genesis**: `I.store_prev == Hash::ZERO`.
- If `I` is the author's **first write** (and not the genesis): `I.store_prev == genesis_hash`.
- Otherwise: `I.store_prev == author_tip[author]` (the author's previous intention).

`Hash::ZERO` appears exactly once per store — in the genesis. Every other `store_prev` is a real, fetchable hash.

#### Separation from `causal_deps`

| Field | Purpose | Scope |
|-------|---------|-------|
| `store_prev` | **Author linearization.** Per-author chain integrity. | Structural — automatic, not chosen by the state machine. |
| `causal_deps` | **State machine causality.** Cross-author causal dependencies. | Semantic — chosen by the author/state machine based on observed state. |

The genesis reference in `store_prev` is structural: it proves the author obtained the store's genesis before writing. It does not express causal dependency on the genesis state — that remains the domain of `causal_deps`.

#### Validation at witness time

```
if intention is the store's genesis:
    assert store_prev == Hash::ZERO
else if author has no prior tip:
    assert store_prev == genesis_hash
else:
    assert store_prev == author_tip[author]
```

#### Impact

- **`witness_ready()`** — new authors' floaters indexed under `genesis_hash` instead of `Hash::ZERO`.
- **`detect_gap()`** — a new author's first intention triggers a genesis fetch if missing (no silent skip).
- **`walk_back_until()`** — walking back hits the genesis (a real intention) instead of `Hash::ZERO`.
- **`verify_and_update_tip()`** — "no tip for author → require `genesis_hash`" instead of `Hash::ZERO`.
- **`author_tip()`** — returns `genesis_hash` for unknown authors (or `None` so callers handle the new-author case explicitly).

Together with Part 3 (connected graph via `causal_deps`), this provides two independent connectivity guarantees: causal (`causal_deps`) and linear (`store_prev`). Either alone is sufficient for graph connectivity; both together make it robust.

## Store Lifecycle

The parent's SystemTable tracks the child's lifecycle state. All transitions are replicated CRDT operations visible to every peer.

### States

```
Created ──→ Active ──→ Archived ──→ Destroyed
                          │
                          └──→ Active  (un-archive)
```

```protobuf
enum ChildStatus {
    CS_UNKNOWN = 0;
    CS_ACTIVE = 1;
    CS_ARCHIVED = 2;
    CS_DESTROYED = 3;
}
```

### Phase 1: Creation

See Part 7 for the detailed creation flow. In summary:

1. The creating node commits the genesis intention and computes `genesis_hash`.
2. The creating node commits `AddPeer(self)` + `SetStoreName` + `SetPeerStrategy`. The actor creates epoch 0 inline during projection (SSM returns `EpochRequired`).
3. The creating node writes `ChildAdd(genesis_hash, alias)` + `ChildSetStatus(Active)` to the parent's SystemTable.
4. The `RecursiveWatcher` on each peer sees `ChildLinkUpdated`, syncs the child store, receives and verifies the genesis intention, and opens it.

The child is now **Active**.

### Phase 2: Operation

Normal read/write/sync cycle. Epochs chain back to genesis, forming the sparse spine. Pruning removes history below settled epochs. The parent's `ChildAdd` entry is immutable — genesis hash and alias never change.

### Phase 3: Archival (Soft Delete)

A peer writes `ChildSetStatus(genesis_hash, Archived)` to the parent's SystemTable.

**Effect on each peer:**
- `RecursiveWatcher` sees `ChildStatusUpdated(_, Archived)`.
- The child store's actor is shut down. Sync and gossip stop.
- On-disk data (`log.db`, `state.db`) is retained.
- The child is excluded from network operations (no sync, no gossip).

**Reversible:** Writing `ChildSetStatus(genesis_hash, Active)` restores the child. The watcher rediscovers it, reopens the store, and resumes sync. No data was lost.

### Phase 4: Destruction (Hard Delete)

Destruction is a coordinated, consensus-driven process. It permanently removes the child store's data from all peers.

#### Prerequisites

1. The child must be in `Archived` status. Destruction of an active store is rejected.
2. All active peers must have synced the archive status. This is verifiable via epoch settlement — if the archival triggered an epoch, all peers must have acked it.

#### Protocol

1. **Propose destruction.** A peer writes `ChildSetStatus(genesis_hash, Destroyed)` to the parent's SystemTable. This intention causally depends on the `ChildSetStatus(Archived)` intention, ensuring the archive was witnessed first.

2. **Peer acknowledgment.** Each peer that sees the `Destroyed` status:
   - Stops all operations on the child store (if not already stopped from archival).
   - Writes a `DestroyAck(genesis_hash)` attestation to the parent's SystemTable — confirming it has seen the destruction and will not serve the child's data.
   - Does **not** delete on-disk data yet.

3. **Safe deletion.** Once all active peers have emitted `DestroyAck` (verified by scanning `TABLE_SYSTEM` attestation keys), each peer is authorized to delete the child's on-disk data directory.

4. **Tombstone.** The parent's SystemTable retains the `ChildAdd` entry (genesis hash, alias) and the `Destroyed` status as a permanent tombstone. This serves as an audit trail — proof that the store existed, who created it, and when it was destroyed.

#### Returning Peers

A peer that was offline during destruction will, on reconnect:

1. Sync the parent store — receive the `ChildSetStatus(Destroyed)` intention.
2. See the child is destroyed.
3. If it still has the child's data locally, delete it.
4. Emit its own `DestroyAck`.

The peer does **not** attempt to sync the child store — the watcher skips stores in `Destroyed` status (same as `Archived`, but without the option to un-archive).

#### Root Store Destruction

Root stores have no parent SystemTable. Destruction of a root store is a peer-governance decision within the root store itself:

1. A peer writes a `SystemOp::StoreDestroy` to the root store's own SystemTable.
2. Other peers acknowledge via `DestroyAck` attestations within the root store.
3. Once all peers have acknowledged, each peer deletes the root store's data and removes it from `meta.db`.

The root store's genesis hash remains in the invite token and any external references, but no node serves its data.

### Lifecycle Summary

| Phase | Parent SystemTable | Child Store | On-Disk Data | Reversible |
|-------|-------------------|-------------|--------------|------------|
| **Created** | `ChildAdd` written | Genesis exists | Created | — |
| **Active** | `status = Active` | Fully operational | Growing | — |
| **Archived** | `status = Archived` | Actor stopped, no sync | Retained | Yes |
| **Destroyed** | `status = Destroyed` + `DestroyAck`s | N/A | Deleted after all acks | No |

## Verification

**Child stores (first sync):**

1. Read the parent's `ChildAdd` for this child — extract `genesis_hash` (which is also the `store_id`).
2. Receive the child's genesis intention via sync.
3. Verify: `blake3(borsh(received_genesis)) == genesis_hash`.
4. Read `store_type` from the genesis payload (`GenesisOp`).
5. Cache `store_id` (= genesis hash) and `store_type` in the child's `TABLE_META`.
6. If mismatch, reject the child store.

**Root stores (join flow):**

1. Parse the invite token — extract `genesis_hash` (which is also the `store_id`).
2. Bootstrap the witness log from the inviter.
3. Verify: the first witnessed intention hashes to `genesis_hash`.
4. Read `store_type` from the genesis payload (`GenesisOp`).
5. Cache `store_id` (= genesis hash) and `store_type` in `TABLE_META`.
6. If mismatch, reject the bootstrap.

**After pruning (epoch bootstrap):**

1. Receive `genesis_hash` from token or parent `ChildAdd`.
2. Receive the genesis intention (still in `log.db` — never pruned).
3. Verify: `blake3(borsh(genesis)) == genesis_hash`.
4. Receive the latest settled epoch intention. Verify its `causal_deps` includes `genesis_hash`.
5. Receive the **state snapshot** at the epoch frontier from the bootstrap peer. This is a locally-computed artifact — not part of the epoch payload — containing the full system + data state at the epoch boundary.
6. Load the snapshot as initial state.
7. Sync the tail (post-epoch intentions) via negentropy from the epoch boundary forward.

The trust chain is: token/parent → genesis hash → genesis intention → epoch (via causal dep) → state snapshot (deterministic projection of the epoch's frontier) → tail → current state.

The state snapshot is deterministic: every node that projects the same epoch frontier produces the same result. A bootstrapping node can verify consistency by checking the snapshot against multiple peers.

## Ordering Constraint

The child's genesis must be fully constructed before the parent's `ChildAdd` is created. The current `create_child_store` flow already satisfies this — the child store is opened first, then the `ChildAdd` batch is committed to the parent. The genesis hash extraction slots in between these two steps.

## Concurrent Child Creation

If two nodes concurrently create a child (both issue `ChildAdd` with different genesis hashes), LWW resolution in the parent's `SystemTable` picks a winner. The losing node must:

1. Detect the mismatch (its local genesis hash differs from the winning `ChildAdd`).
2. Discard its local child store.
3. Sync the winning child store from a peer (using the winning genesis hash to verify).

This is analogous to how concurrent writes to any key resolve today — the DAG preserves both, LWW picks a physical winner, and the losing value is superseded.

## Impact

### Stability Frontier

Epoch settlement replaces the per-intention stability frontier for pruning decisions. An epoch is settled when every peer in its `required_acks` list has acked. The epoch's frontier (its cited author tips) defines the prune-safe boundary. The per-author tip tracking (`min(tip[A] across peers)`) is still useful for sync scheduling but is no longer the pruning trigger.

### Pruning

A connected DAG with epochs simplifies pruning. Each settled epoch defines a precise frontier: everything reachable from the epoch's deps is the canonical state; everything below the frontier (not cited by the epoch) is prunable once all active peers have acked.

The genesis intention is never pruned — every epoch cites it directly. All other intentions below the settled epoch frontier are prunable, including intermediate epochs and their acks.

### Fork Detection

With a connected DAG, any two intentions share a common ancestor (at worst, the genesis). This makes fork detection well-defined for all intention pairs, not just those within the same author's chain.

### Bootstrap Trust

The genesis commitment — whether in `ChildAdd` or invite token — gives bootstrap a cryptographic trust anchor. A joining node verifies the store's identity before accepting content. Without this, a malicious peer could serve a fabricated store with forged history.

### Sync Protocol

The genesis hash **is** the `store_id` used in the Negentropy/sync handshake. Two nodes verify they're syncing the same store by comparing store IDs, which are genesis hashes — cryptographically unique and unforgeable.

## Migration

No production stores exist. Development stores are migrated in-place:

1. **Synthetic genesis.** On first open of a pre-genesis store, a deterministic genesis intention is synthesized and ingested. The keypair is derived from the store UUID (`blake3("lattice-synthetic-genesis" || uuid)`), ensuring all nodes produce the same genesis hash. See `genesis::build_synthetic_genesis()`.

2. **Forward-only invariant.** The connected graph invariant (non-empty `causal_deps`) and the genesis-rooted `store_prev` invariant (Part 8) are enforced only for new intentions. Pre-genesis intentions are grandfathered — they were already witnessed and won't be re-validated. Existing `store_prev: Hash::ZERO` entries for authors' first writes remain valid.

3. **Pruning cleanup.** Once log pruning is implemented and an epoch cuts above the synthetic genesis, all pre-genesis intentions are pruned. The synthetic genesis becomes indistinguishable from a real one. The grandfather rule becomes dead code and can be removed.

The synthetic genesis author (deterministic keypair) is not in the peer set and will never write again. Epoch settlement uses the epoch's `required_acks` list, which will never include the synthetic author.

## Open Questions

- **~~Genesis payload contents.~~** Resolved: the genesis carries **`store_type` + a random `nonce`**. There is no UUID and no `store_id` field on `Intention` — the genesis hash is the store ID, and store membership is proven by DAG structure (condition deps chain to genesis). Mutable metadata (name, peer strategy, peer status) goes in subsequent intentions. The nonce ensures unique genesis hashes even when the same peer creates multiple stores of the same type in the same HLC tick.
- **~~`JoinRequest` routing.~~** Resolved: the genesis hash **is** the store ID. `JoinRequest` contains the genesis hash, and the inviter looks it up directly in `StoreManager` — no mapping needed.
- **~~Auto-deps mechanism.~~** Resolved: replaced by **epoch intentions**. Membership-critical system changes trigger a new epoch (via `SystemEvent::EpochRequired` from the SSM). The epoch captures the full DAG frontier. Peers acknowledge via `EpochAckOp`. The epoch frontier defines revocation cutoffs and pruning boundaries.
- **~~Snapshot spine.~~** Resolved: the **epoch spine** replaces dedicated snapshot intentions for DAG structure. Each epoch cites genesis directly and captures all author tips. The sparse spine is `genesis ← epoch N ← tail`. Materialized state snapshots (for bootstrap optimization) are a separate concern from the causal structure.
- **Human-readable identifiers.** Genesis hashes are 32-byte blobs. For CLI/UI display, a truncated hex prefix (e.g., `a1b2c3d4`) or a Base58 short form may be needed.
- **Re-projection on epoch arrival.** When a node receives a new epoch that excludes a revoked peer's writes it already applied, the affected state must be re-projected. The scope is limited to keys last written by the revoked peer. The mechanism (full replay from epoch, or targeted key re-evaluation) needs design.
- **~~Who creates the epoch?~~** Resolved: the actor creates epochs only during the **local write path** (`Submit` command). When a synced intention triggers `EpochRequired` during projection via `IngestBatch`, the actor ignores it — the authoring node already created the epoch. See "Epoch creation: actor-driven, local writes only" in Part 3.
- **Epoch creation timing.** The epoch creator should sync with all peers before creating the epoch to maximize the frontier coverage. Writes from active peers that the creator hadn't synced are beyond the frontier but still valid. The gap is harmless but results in a slightly stale frontier snapshot.
- **KvTable head metadata for pruning.** The `KvTable` on-disk `Value` proto stores head hashes but NOT their timestamps or authors. LWW conflict resolution calls `dag.get_intention()` to compare HLC timestamps. After pruning, the intention may be gone and the lookup fails. Fix: store `(hash, timestamp, author)` per head in the `Value` proto so LWW resolution is self-contained. This is a prerequisite for pruning.
- **Transition-window writes have dangling `causal_deps` after log split.** Intentions written by active peers before seeing the new epoch reference deps from the old log. After the witness log splits on epoch settlement, these deps are unresolvable in the new log. This is safe: the witness chain proves the deps were valid at witness time. Bootstrapping nodes trust the witness signatures and do not re-verify deps. The snapshot's CRDT state (head hashes per key) provides sufficient context for projection.
- **Stale peer blocking settlement.** A permanently offline peer blocks epoch settlement (and therefore pruning). Resolution: remove the stale peer from the active set via a system op, which triggers a new epoch excluding them.
- **Destruction without full consensus.** What if a peer is permanently lost (hardware failure)? The `DestroyAck` protocol stalls. Options: (a) remove the lost peer from the active peer set first (existing peer governance), which unblocks the ack threshold; (b) allow destruction after a supermajority of peers ack; (c) time-bounded grace period after which missing acks are assumed.
- **Recursive destruction.** Destroying a store that has children. Should destruction cascade automatically (destroy all descendants depth-first), or should the parent be required to destroy children explicitly before itself?
- **Data retention policy.** Some deployments may require retaining destroyed store data for a grace period (regulatory, backup). The destruction protocol could support a configurable retention period before physical deletion.

## Diagrams

See `docs/content/design/*.dot` (render with `dot -Tpdf`) for detailed DAG diagrams:
- **revocation-dag.dot** — Per-node view during a partition (A has revocation, B and C don't)
- **revocation-merged.dot** — Converged DAG with epoch bridge, epoch ack, transition-window writes
- **revocation-node-b.dot** — Node B's four-phase perspective: write, epoch transition, ack, prune
- **epoch-pruning.dot** — Pruning phases: before, after, bootstrap from pruned store
- **witness-log-epochs.dot** — Witness log split on epoch settlement, before/after view
