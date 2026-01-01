# Lattice

A distributed, offline-first key-value store with Ed25519-signed append-only logs.

## Quick Start

1. **Initialize the first node**:
   ```bash
   cargo run --package lattice-cli
   lattice:no-store> mesh init
   ```

2. **Invite a second node**:
   On the second node, get its public key with `node status`. Then on the first node:
   ```bash
   lattice:060e0f0d> mesh invite <second-node-pubkey>
   ```

3. **Join the mesh**:
   On the second node, join the first node using its Node ID and the Mesh ID (from `mesh status` on first node):
   ```bash
   lattice:no-store> mesh join <first-node-id> <mesh-id>
   ```

## CLI Commands

```
[node]
  node status            Show local identity info

[mesh]
  mesh init              Create a new mesh (root store)
  mesh status            Show mesh info
  mesh join <nodeid> <meshid>  Join a mesh
  mesh peers             List peers
  mesh invite <pubkey>   Invite a peer
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

```
lattice/
├── lattice-core/       # Core logic: Node, Store, SigChain, Entry
├── lattice-net/        # Networking: LatticeServer, Gossip, Sync
├── lattice-cli/        # Interactive CLI
├── proto/              # Protocol buffers (entries, messages)
└── docs/               # Architecture & roadmap
```

## Data Directory

```
~/.local/share/lattice/
├── identity.key        # Ed25519 private key
├── meta.db             # Global metadata
└── stores/{uuid}/      # Per-store data
    ├── logs/           # Append-only entry logs
    └── state.db        # KV state snapshot
```

## Documentation

- [Architecture](docs/architecture.md) - Design concepts
- [Roadmap](docs/roadmap.md) - Development progress
- [KV Store](docs/kvstore.md) - Store design notes
