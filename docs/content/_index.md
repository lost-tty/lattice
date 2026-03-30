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
# Start the daemon (web UI starts at http://localhost:8123)
lattice --daemon

# Use the CLI in another terminal
lattice
lattice> store create --root
lattice:a1b2> put hello world
lattice:a1b2> get hello
world
```

The web UI is enabled by default on port 8123 in daemon mode. Use `--web <port>` to change the port or `--no-web` to disable it.

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
| `lattice-cli` | Single binary (`lattice`): REPL, daemon mode (`--daemon`), embedded mode (`--embedded`). |

## Key Dependencies

- [Iroh](https://iroh.computer) — QUIC transport, NAT traversal, peer discovery
- [Redb](https://www.redb.org) — Embedded ACID key-value storage
- [UniFFI](https://mozilla.github.io/uniffi-rs/) — FFI bindings for Swift/Kotlin

## Data Directory

```
<app-data>/lattice/
├── identity.key        # Ed25519 node keypair
├── meta.db             # Global inventory (rootstores table, node name)
├── lattice.sock        # Daemon UDS socket (when running)
└── stores/{uuid}/      # Per-store data
    ├── intentions/
    │   └── log.db      # Intention DAG (Weaver)
    └── state/
        └── state.db    # Materialized state (KV / system tables)
```

`<app-data>` is platform-dependent: `~/Library/Application Support` on macOS, `~/.local/share` on Linux.

## Community

Join the Matrix room: [#latticesync:matrix.org](https://matrix.to/#/%23latticesync:matrix.org)

## License

Mozilla Public License 2.0 (MPL-2.0).
