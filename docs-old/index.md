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
| **Intention DAG Identity** | Identity is a cryptographic progression of signed operations (Intention DAG), not a database row. Trust is transitive and proven via the chain. |

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
| `lattice-model` | **Foundation**. Core types, cryptographic primitives, Weaver Protocol. |
| `lattice-proto` | **Foundation**. Protobuf schemas defining wire formats. |
| `lattice-net-types` | **Foundation**. Abstraction breaking cyclic dependencies for stores. |
| `lattice-sync` | **Foundation**. Negentropy-based set reconciliation protocol. |
| `lattice-storage` | **Core Storage**. Common persistence, ACID, and DAG verification. |
| `lattice-systemstore` | **Core State**. Administrative middleware isolating system/app data. |
| `lattice-logstore` | **Core State**. Append-only log state machine. |
| `lattice-kvstore` | **Core State**. DAG-CRDT Key-Value state machine. |
| `lattice-kernel` | **Core State**. Weaver replication engine and DAG orchestration. |
| `lattice-node` | **Runtime**. Fractal store routing, MetaStore, and peer auth. |
| `lattice-net` | **Runtime**. P2P Gossip, sync protocols, and Iroh endpoints. |
| `lattice-api` | **Runtime**. IPC layer over UDS, exposing gRPC services. |
| `lattice-runtime` | **Runtime**. Bootstrapper for Daemon and In-Process modes. |
| `lattice-bindings` | **Clients**. UniFFI exports and dynamic reflection for mobile. |
| `lattice-kvstore-client` | **Clients**. Strongly-typed asynchronous KV client. |
| `lattice-mockkernel` | **Testing**. Synchronous test utilities bypassing replication. |
| `lattice-daemon` | **Binaries** (`latticed`). Headless orchestrator service. |
| `lattice-cli` | **Binaries** (`lattice`). Interactive REPL with DAG visualization. |

## Data Directory

```
~/.local/share/lattice/
├── identity.key        # Ed25519 private key
├── meta.db             # Global metadata
├── latticed.sock       # Daemon UDS socket (when running)
└── stores/{uuid}/      # Per-store data
    ├── logs/           # Append-only intention logs
    └── state.db        # KV state snapshot
```

## Roadmap & RFCs

Lattice is currently a "Kernel" project. The core replication engine is rigorous, but we are looking for contributors to help architect the ecosystem layers:

- **Negentropy (O(1) Reconciliation)**: Implement range-based set reconciliation to scale sync to millions of intentions.
- **Capabilities (ACLs)**: Move beyond "invite-only" meshes to Signed Capability (OCAP) tokens for granular permissions.
- **CRDT Overlays**: Standardize interfaces for Map/Set/Text CRDTs on top of the KV foundation.
- **CAS / Shared Network Drive**: Future integration with a Content-Addressable Storage (CAS) layer (or Garage sidecar) to enable a fully decentralized, local-first shared network drive.

## License

This project is licensed under the **GNU Affero General Public License v3.0 (AGPL-3.0)**. We believe infrastructure for the common good should remain open. If you modify Lattice and provide it over a network, you must share your improvements.

