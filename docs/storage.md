# Storage Format

## Directory Layout

```
~/.local/share/lattice/
├── identity.key              # Ed25519 private key (not replicated)
├── meta.db                   # Global metadata (redb)
└── stores/{uuid}/
    ├── sigchain/             # SigChainManager owns this directory
    │   └── {author}.log      # Append-only SignedEntry stream
    ├── state/                # Backend owns this directory
    │   └── state.db          # Per-store backend state (e.g. redb for KvState)
    └── sync/                 # Sync metadata
```

## meta.db (redb)

| Table   | Key           | Value              | Purpose                      |
|---------|---------------|--------------------|------------------------------|
| stores  | UUID (16B)    | created_at (u64)   | Known stores                 |
| meta    | "root_store"  | UUID (16B)         | Auto-opened on CLI startup   |

## state.db (redb, per store)

For `KvState`:

| Table   | Key      | Value       | Purpose                |
|---------|----------|-------------|------------------------|
| kv      | String   | Vec<u8>     | Key-value data         |
| meta    | String   | Vec<u8>     | last_seq, last_hash    |

## Log Files

Each `{author}.log` contains length-delimited `LogRecord` messages:

```protobuf
message LogRecord {
  bytes hash = 1;         // BLAKE3 hash of entry_bytes
  bytes entry_bytes = 2;  // Serialized SignedEntry
}
```

Hashes are verified on read; corruption causes `LogError::HashMismatch`.
