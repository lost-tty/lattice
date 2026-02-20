# The Storage Engine

Lattice operates fundamentally as a decentralized log replicator. However, applications need highly efficient ways to query that data (e.g., fetching a user profile by its key without replaying a million logs). 

Consequently, Lattice isolates persistence into three distinct domains.

## The Storage Layout

```text
~/.local/share/lattice/
├── identity.key              # Ed25519 node private key
├── meta.db                   # Global Inventory (the `rootstores` table)
└── stores/
    └── {store_uuid}/
        ├── logs/             # The append-only Intention DAG
        └── state.db          # The materialized KV State query engine
```

## 1. The Global Meta DB (`meta.db`)
This single Redb instance is managed by the `StoreManager` at the root `<appDataDir>` level. It contains:
- The **`rootstores`** table, tracking which root workspaces this node has locally initialized.
- Local configuration overrides (which ports to bind to, node naming).
It *never* contains user data.

## 2. The Intention Logs (`logs/`)
Managed by the `IntentionStore` (inside `lattice-kernel`).

When users construct a transaction (an **Intention**), it is cryptographically signed and forms a chain with previous Intentions. These are persisted locally in append-only log files.

- **Replication Over RPC:** When two nodes sync via `lattice-net`, they stream these raw intent logs to each other.
- **Verifiable Truth:** Because the signature signs over the payload and the causal history, the `IntentionStore` validates the entire mathematical sequence of reality before it ever allows the data into `state.db`.

## 3. The Materialized State (`state.db`)
Managed by a specific `StateMachine` (like `lattice-kvstore` for Key-Value data).

Once an Intention passes cryptographic verification in the `IntentionStore`, the physical bytes of the payload are handed down to the `StateMachine.apply()` function.

The `KvStore` parses the payload into physical Redb commands:
- `Op::Put(key, value)` translates directly to an ACID write in `state.db`.
- **Causal Linking:** Because `state.db` maintains multiple tips (frontiers) for unresolved forks, reads deterministically pick the winner. 

## The `StateBackend` Interface
Both the `IntentionStore` and the various `StateMachine` types communicate with Redb through a generic `StateBackend` trait. 

```rust
pub trait StorageTransaction {
    fn set(&mut self, col: Collection, key: &[u8], value: &[u8]) -> Result<()>;
    fn get(&self, col: Collection, key: &[u8]) -> Result<Option<Vec<u8>>>;
    // ...
}
```

This ensures strict isolation of Column Families (Tables) preventing a user application from accidentally overwriting chain metadata. It also forms the foundation for embedded devices (like the RP2350 `no_std` environments) to implement a custom memory backend that respects the identical trait abstractions.
