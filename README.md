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

## Building

**Prerequisites**: Rust 1.75+, Protobuf compiler (`protoc`)

```bash
# Build release binaries
cargo build --release

# Binaries are in target/release/
ls target/release/lattice target/release/latticed
```

Or install directly:
```bash
cargo install --path lattice-cli
cargo install --path lattice-daemon
```

## Quick Start

### Option 1: Daemon Mode (recommended)

1. **Start the daemon**:
   ```bash
   latticed
   ```

2. **In another terminal, use the CLI**:
   ```bash
   lattice
   lattice:no-mesh> mesh create
   ```

### Option 2: Embedded Mode (standalone)

```bash
lattice --embedded
lattice:no-mesh> mesh create
```

### Connecting Nodes

1. **Generate an invite token**:
   ```bash
   lattice:060e0f0d> mesh invite
   # Outputs: 2aWDipfQ...
   ```

2. **Join the mesh** (on second node):
   ```bash
   lattice:no-mesh> mesh join <token>
   ```

## CLI Commands

```
General:
  quit                     Exit the CLI

Mesh operations (init, join, invite, peers):
  mesh create              Create a new mesh
  mesh list                List all meshes
  mesh use                 Switch to a mesh
  mesh status              Show mesh status
  mesh join                Join a mesh using an invite token
  mesh peers               List all peers
  mesh invite              Generate a one-time invite token
  mesh revoke              Revoke a peer from the mesh

Node operations:
  node status              Show node info (local identity)
  node set-name            Set display name for this node

Store management:
  store create             Create a new store
  store list               List stores
  store use                Switch to a store
  store delete             Delete (archive) a store
  store status             Show store status
  store debug              Debug graph output
  store orphan-cleanup     Clean up stale orphaned entries
  store history            Show history for a key
  store author-state       Show author sync state
  store sync               Sync with all peers
  store inspect-type       Explore a message type's schema
  store subs               List active stream subscriptions
  store unsub              Stop a subscription by ID (or "all")

Store Operations:
  put <key> <value>        Store a key-value pair
  get <key> <verbose>      Get value for key
  delete <key>             Delete a key
  list <prefix> <verbose>  List keys by prefix

Store Streams:
  watch [pattern]          Subscribe to key changes matching a regex pattern
```

## Project Layout

| Crate | Purpose |
|-------|---------|
| `lattice-model` | **Shared types**. Core types, traits, and protocol definitions. |
| `lattice-kernel` | **Replication engine**. SigChain, HLC, DAG, and entry validation. |
| `lattice-kvstore` | **KV state machine**. LWW key-value with atomic batches. |
| `lattice-logstore` | **Log state machine**. Append-only log with HLC ordering. |
| `lattice-node` | **Application layer**. Node, Mesh, PeerManager orchestration. |
| `lattice-net` | **Networking**. Iroh endpoints, Gossip, and Sync protocols. |
| `lattice-net-types` | **Network abstractions**. Shared traits decoupling net from node. |
| `lattice-api` | **API layer**. Protobuf definitions, gRPC services, backend trait. |
| `lattice-runtime` | **Runtime**. Backend implementations (InProcess, RPC). |
| `lattice-daemon` | **Daemon binary** (`latticed`). Headless service with UDS socket. |
| `lattice-cli` | **CLI binary** (`lattice`). Interactive shell for node management. |
| `lattice-bindings` | **FFI bindings**. UniFFI exports for Swift/Kotlin. |

## Data Directory

```
~/.local/share/lattice/
├── identity.key        # Ed25519 private key
├── meta.db             # Global metadata
├── latticed.sock       # Daemon UDS socket (when running)
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

