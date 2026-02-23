---
title: "Stability Frontier"
status: discussion
---

Design for tracking per-peer sync state, deriving a global stability frontier for log pruning, and surfacing per-device lag metrics to the user.

## Problem

Lattice nodes need to know what other nodes have, per store. Two outputs derive from this:

1. **Stability frontier** — the largest causally-closed set of intentions that *all* peers provably hold. Everything below this frontier is safe to prune (M15B).
2. **Lag metric** — per-peer divergence, surfaced to the user as a durability health indicator ("1 of 3 replicas is stale — some data exists on only one device").

## Key Insight: Per-Author Chains

Each node's witness log is a local linearization of the DAG. The interleaving of concurrent authors varies per node, so witness logs cannot be compared directly.

However, each **author's sub-chain** (`store_prev` links) is deterministic and identical across all nodes. This gives an unambiguous, comparable reference frame.

`TABLE_AUTHOR_TIPS` already stores `Author → LatestHash` on every node. If peers exchange their author-tip maps, the stability frontier is:

```
For each author A:
    frontier[A] = min(tip[A] across all peers)
```

This is automatically causally closed. The witness log only admits an intention once all its causal dependencies are resolved — an intention cannot be witnessed (and thus cannot advance an author tip) until every dependency is present locally. So any node attesting tip `A3` is guaranteed to hold all of `A3`'s transitive dependencies. The per-author minimum across peers can never produce an inconsistent cut.

## Tip Attestations

Nodes periodically publish **changed author tips** as a `SystemOp` within the `UniversalOp` envelope. Each attestation contains only the tips that advanced since the node's last attestation:

```rust
SystemOp::TipAttestation {
    tips: Vec<(PubKey, Hash)>,   // only changed authors
}
```

The `SystemStore` merges these into a full per-peer view using its existing LWW-CRDT semantics. Each entry is stored as a system key:

```
attestation/{attester}/{author} → Hash    (LWW by HLC)
```

The full tip map for any peer is always available by reading `attestation/{peer}/*` from `TABLE_SYSTEM`. No special attestation state machine — just system keys with the merge logic that already exists.

### Transitive Relay

Not all nodes connect directly — some only see each other through intermediaries.

1. Node C publishes its attestation as a `SystemOp`
2. Node B receives it via gossip or sync
3. B's store now contains C's attestation — it replicates to A during their next sync
4. A reads C's attestation from `TABLE_SYSTEM` and updates its view of C's state

A and C never need to connect directly.

### Trust Model

A node has no incentive to attest more than it holds — that would advance the frontier past its actual state, making pruned data unrecoverable for itself. Attesting less than it holds is conservative and safe (just slows the frontier).

### Attestation Frequency

Attestations are event-driven with a **minimum interval of ~5 minutes** between publications:

| Trigger | Notes |
|---------|-------|
| **After a completed sync session** | Directly ties the frontier to verified state |
| **Batch threshold** (every N witnessed intentions) | Bounds staleness for write-heavy, sync-light nodes |
| **Periodic heartbeat** (~15 min, if tips changed) | Catch-all for idle nodes |
| **Graceful shutdown** | Best-effort — may not propagate before the node goes down |

The periodic heartbeat is the reliable baseline. The last heartbeat that successfully propagated becomes the effective "last known state" for that peer.

On graceful shutdown, the node attempts a **best-effort final sync** (deadline ~5 seconds) to flush its attestation to the nearest reachable peer. If no peer responds in time, the node shuts down anyway — the next startup sync catches up.

Because attestations are delta-only, a node with 1000 authors that had 2 writes since its last attestation publishes **2 entries**, not 1000. Attestations replicate via the normal store sync (gossip + Negentropy) — no separate exchange protocol is needed.

## Deriving the Stability Frontier

Once a node has built the full view from `TABLE_SYSTEM` attestation keys:

```
For each author A:
    frontier[A] = min tip across all peers
    (verified by walking store_prev to confirm ancestry)

Prunable: all intentions from each author up to frontier[A]
```

The global frontier advances monotonically as peers sync.

### Comparing Tip Depth

The wire protocol intentionally carries no per-author sequence numbers — intentions are identified purely by hash. Determining "which tip is earlier" requires a local `store_prev` chain walk, which is O(chain length) against the on-disk DAG.

If this becomes a bottleneck, a **local-only index** (`Hash → depth` in redb, never transmitted) would make the comparison O(1) without touching the wire format.

## Lag Metric

The same per-peer attestation data produces a human-readable health view:

```
store a1b2:
  raspi:  alice ✓  bob -3  carol ✓    (3 behind, last seen 2h ago)
  iphone: alice ✓  bob ✓   carol ✓    (current)
  ⚠ raspi missing bob's last 3 writes — last synced 2h ago
```

This surfaces as `NodeEvent::PeerLagWarning` or via `store status` in the CLI.

## Relationship to Snapshots

The stability frontier defines *where* to snapshot; M15A/M15D provide the *mechanism*. New peers bootstrap from a snapshot rather than replaying the full log. The current system uses an implicit empty snapshot (genesis) — once pruning is implemented, the frontier becomes the explicit snapshot point.

On compaction, the latest attestation per peer is materialized into the snapshot (as part of `TABLE_SYSTEM` state). Individual attestation intentions in the pruned log are discarded — the snapshot carries the state forward. No pinning required.

Lifecycle: frontier advances → snapshot state at frontier (including attestations) → prune intentions below frontier → new peers bootstrap from snapshot + tail sync.

## Where It Fits

- `SessionTracker` — extended with `peer_tips` per store
- Pruning (M15C) — uses the frontier for `truncate_prefix`
- Snapshots (M15B) — frontier defines the snapshot point
- Future: the lag metric enables durability warnings in the SwiftUI app

## Open Questions

- **Peer state impact on frontier:** An offline or revoked peer's stale tips would permanently bottleneck the frontier. The frontier calculation likely needs to consider only `Active` peers from the `SystemStore`, but the exact policy (grace periods, revocation semantics) is to be designed separately.
- **Author identity cardinality:** The design assumes a manageable number of authors per store (tens to low hundreds). Stores with thousands of authors would increase attestation size and frontier computation cost.
