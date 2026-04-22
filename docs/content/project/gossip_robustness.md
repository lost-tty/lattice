# Gossip robustness

## Problem

Peer connections can silently rot. The sync layer only reacts to `PeerConnected` events from gossip; if those stop firing, nothing notices.

Known failure modes:
- **Mobile**: wifi change, sleep/wake. iOS app already calls `store_sync` on scene-phase `.active`, but that goes through the same potentially-dead gossip/transport.
- **Daemon wake**: no OS-level hook wired up.
- **Network-path changes (Linux)**: rtnetlink and systemd signals available, not consumed.

## Current state

- Gossip has two rejoin loops (`lattice-net-iroh/src/gossip.rs`): stream-close, and zero-neighbors. Both driven by gossip-internal events.
- `run_auto_sync` (`lattice-net/src/network/service.rs:265`) is one task per store, reactive to `PeerConnected` only.
- `SessionTracker` stores `Instant` per online peer but nothing ages entries out — peers stay "online" until an explicit `NeighborDown`.
- `PeerInfo.last_seen_ms` is on the wire and rendered in the UI; not used for any logic.
- No FFI entry point for "rejoin gossip." `store_sync` is the closest thing and only hits the sync layer.

## Direction

A store-level **rejoin gossip** action, triggered by events.

Triggers:
- **iOS**: scene-phase `.active`, `NWPathMonitor` path change.
- **macOS / desktop**: OS network-change notifications.
- **Linux daemon**: rtnetlink link/address events; systemd `sleep.target` resume.
- **Internal suspicion**: new local data to sync and no (or few) online peers.

All triggers are async and throttled; concurrent triggers across stores dedupe at the service level.

## Open questions

- Should `SessionTracker` age entries out independently, so the UI stops lying about "online" status even if it doesn't act on it?
- What's the right throttle window — per-store or global?
- HyParView only surfaces direct neighbors (~5 per node) via `NeighborUp`. Non-neighbor peers are reachable transitively through gossip forwarding but never marked online locally, so negentropy sync never runs against them. For small swarms this means some peer pairs may never sync directly even when both are online.

## Tracking peer knowledge

When a node receives an intention from peer X, its causal deps (references to other authors' intentions) prove X had seen those hashes at authoring time. Walking X's witnessed intentions and unioning their causal deps gives a lower bound on X's knowledge, derived from data already in the witness log.

Could serve as a basis for later watermark detection.
