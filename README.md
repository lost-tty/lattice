# Lattice

_Building a future with clear skies._

Lattice is a local-first, mesh-native replicated state machine platform designed for the edge. It provides a pluggable store architecture (key-value, append-only logs, and custom CRDTs) with rock-solid local infrastructure, resilient synchronization, and offline-first durability running entirely on localhost.

<p align="center">
  <img src="docs/mergetree.png" alt="Lattice Merge Tree Visualization">
  <br>
  <i>Lattice's DAG visualizer showing concurrent edits merging deterministically.</i>
</p>

> [!NOTE]
> **This is an experiment.** Lattice is research software built to explore local-first topology-agnostic data replication. It works, it's fun, but it might not work out in the end. Use at your own risk and enjoy the ride.

## Philosophy

We believe software should be as resilient as the hardware it runs on. Most modern applications are fragile thin clients that break the moment connectivity drops. Lattice inverts this model.

- **Uncompromising Availability**: Reads and writes always happen locally on disk. Zero latency, zero downtime, fully offline-capable by default.
- **Sovereign Data**: You own your data physically. It lives on your nodes, under your cryptographic control, not merely in a provider's database.
- **Fluid Topology**: Data flows like water—over local Wi-Fi, Mesh VPNs, or the public Internet—finding the most direct path between peers without central bottlenecks.

## Architecture

<p align="center">
  <img src="docs/Lattice Architecture.png" alt="Lattice Architecture">
</p>

### 1. Core Primitives
| Component | Description |
|-----------|-------------|
| **Hybrid Logical Clocks (HLC)** | Provides localized causal ordering without a central time authority. Enables conflict detection across network partitions. |
| **SigChain Identity** | Identity is a cryptographic log (append-only Signature Chain), not a database row. Trust is transitive and proven via the chain. |

### 2. Data Layer
- **DAG Model**: History is a Directed Acyclic Graph, allowing localized concurrent edits without global locking.
- **Storage Engine**: `redb` provides ACID-compliant, embedded, high-performance local storage.
- **Conflict Resolution**: Deterministic Last-Write-Wins (LWW) by default, extensible to custom CRDTs.

### 3. Network & Sync
- **Transport**: Built on Iroh, utilizing QUIC for NAT traversal across LAN and Internet.
- **Reconciliation**: Bidirectional protocol ensures eventual consistency.
- **Gossip**: Efficient message propagation for real-time updates.

## Quick Start

1. **Create a mesh on the first node**:
   ```bash
   cargo run --package lattice-cli
   lattice:no-mesh> mesh create
   ```

2. **Generate an invite token**:
   ```bash
   lattice:060e0f0d> mesh invite
   # Outputs: 2aWDipfQ...
   ```

3. **Join the mesh** (on second node):
   ```bash
   lattice:no-mesh> mesh join <token>
   ```

## CLI Commands

```
[node]
  node status            Show local identity info

[mesh]
  mesh create            Create a new mesh (can create multiple)
  mesh list              List all meshes
  mesh use <id>          Switch to a mesh (partial ID supported)
  mesh status            Show current mesh info
  mesh invite            Generate a one-time invite token
  mesh join <token>      Join a mesh using invite token
  mesh peers             List peers
  mesh revoke <pubkey>   Revoke a peer

[store]
  store create           Create a new store (subordinate)
  store use <uuid>       Switch to a store
  store list             List all stores
  store status           Show store info
  store sync             Sync with peers
  store author-state     Show author sync state (use -a for all)

[kv]
  put <key> <value>      Store a value
  get <key>              Retrieve a value  
  delete <key>           Delete a key
  list [prefix]          List keys
  history [key]          Show key history

[general]
  help                   Show all commands
  quit                   Exit
```

## Project Layout

| Crate | Purpose |
|-------|---------|
| `lattice-model` | **Shared types**. Core types, traits, and protocol definitions used across crates. |
| `lattice-kernel` | **The replication engine**. Implements Store, SigChain, HLC, and entry validation. |
| `lattice-kvstore` | **KV state machine**. LWW-based key-value state with atomic batch operations. |
| `lattice-logstore` | **Log state machine**. Append-only log with HLC-ordered persistence. |
| `lattice-node` | **Application layer**. Node, Mesh, PeerManager orchestration and store lifecycle. |
| `lattice-net` | **Networking layer**. Iroh endpoints, Gossip, and Sync protocols. |
| `lattice-net-types` | **Network abstractions**. Shared traits for decoupling net from node. |
| `lattice-cli` | **Interactive shell** for managing nodes and debugging state. |
| `lattice-proto` | **Protocol Buffers** definitions for wire format and disk storage. |

## Data Directory

```
~/.local/share/lattice/
├── identity.key        # Ed25519 private key
├── meta.db             # Global metadata
└── stores/{uuid}/      # Per-store data
    ├── logs/           # Append-only entry logs
    └── state.db        # KV state snapshot
```

## Roadmap & RFCs

Lattice is currently a "Kernel" project. The core replication engine is rigorous, but we are looking for contributors to help architect the ecosystem layers:

- **Negentropy (O(1) Reconciliation)**: Implement range-based set reconciliation to scale sync to millions of entries.
- **Capabilities (ACLs)**: Move beyond "invite-only" meshes to Signed Capability (OCAP) tokens for granular permissions.
- **CRDT Overlays**: Standardize interfaces for Map/Set/Text CRDTs on top of the KV foundation.
- **CAS / Shared Network Drive**: Future integration with a Content-Addressable Storage (CAS) layer (or Garage sidecar) to enable a fully decentralized, local-first shared network drive.

## License

This project is licensed under the **GNU Affero General Public License v3.0 (AGPL-3.0)**. We believe infrastructure for the common good should remain open. If you modify Lattice and provide it over a network, you must share your improvements.

