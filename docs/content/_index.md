---
title: "Lattice"
---

Lattice is a decentralized syncing engine. It runs on localhost for offline durability, but securely syncs state across a distributed mesh network. It handles cryptographic identity under the hood. It currently ships with key-value and append-only log data structures, with the architecture designed for custom CRDTs in the future.

## Overview

All data lives on disk locally. Each write produces a signed, append-only log entry. Nodes sync these logs with each other over QUIC (via Iroh), directly peer-to-peer. No server in between.

## Quick Start

**Prerequisites**: Rust 1.75+, Protobuf compiler (`protoc`)

```bash
cargo build --release
```

**Daemon mode**:

```bash
# Terminal 1: start the daemon
latticed

# Terminal 2: use the CLI
lattice
lattice> store create --root
lattice:a1b2> put hello world
lattice:a1b2> get hello
world
```

**Embedded mode** (standalone, no daemon):

```bash
lattice --embedded
```

## Project Layout

| Crate | Purpose |
|-------|---------|
| `lattice-model` | Core types, cryptographic primitives, Weaver protocol traits. |
| `lattice-proto` | Protobuf schemas defining wire formats. |
| `lattice-storage` | Common persistence, ACID transactions, and DAG verification. |
| `lattice-systemstore` | Y-Adapter middleware isolating system metadata from app data. |
| `lattice-logstore` | Append-only log state machine. |
| `lattice-kvstore` | DAG-CRDT key-value state machine. |
| `lattice-kernel` | Weaver replication engine and DAG orchestration. |
| `lattice-node` | Fractal store routing, MetaStore, and peer authorization. |
| `lattice-net` | P2P gossip, sync protocols, and Iroh endpoints. |
| `lattice-api` | IPC layer over UDS, exposing gRPC services. |
| `lattice-runtime` | Bootstrapper for daemon and in-process modes. |
| `lattice-bindings` | UniFFI exports for Swift/Kotlin. |
| `lattice-daemon` | Headless orchestrator binary (`latticed`). |
| `lattice-cli` | Interactive REPL binary (`lattice`). |

## Key Dependencies

- [Iroh](https://iroh.computer) — QUIC transport, NAT traversal, peer discovery
- [Redb](https://www.redb.org) — Embedded ACID key-value storage
- [UniFFI](https://mozilla.github.io/uniffi-rs/) — FFI bindings for Swift/Kotlin

## Data Directory

```
<app-data>/lattice/
├── identity.key        # Ed25519 node keypair
├── meta.db             # Global inventory (rootstores table, node name)
├── latticed.sock       # Daemon UDS socket (when running)
└── stores/{uuid}/      # Per-store data
    ├── intentions/
    │   └── log.db      # Intention DAG (Weaver)
    └── state/
        └── state.db    # Materialized state (KV / system tables)
```

`<app-data>` is platform-dependent: `~/Library/Application Support` on macOS, `~/.local/share` on Linux.

## License

GNU Affero General Public License v3.0 (AGPL-3.0).
