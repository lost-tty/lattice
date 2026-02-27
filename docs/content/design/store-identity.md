---
title: "Store Identity & Lifecycle"
---

> **Status**: Implemented

Lattice stores must be **self-descriptive**. A store on disk should contain all the information required to identify its logic (type) and data format (schema version), allowing the node to dynamically load the correct `StoreOpener` without prior configuration.

## The `meta` Table Schema

Every Lattice store (regardless of type) maintains a `meta` table in its `state.db` (Redb). This table is the "boot sector" of the store.

| Key                | Value Type        | Description                                                  |
| :----------------- | :---------------- | :----------------------------------------------------------- |
| `store_id`         | `[u8; 16]` (UUID) | Unique Instance ID. Validates we opened the right store.     |
| `store_type`       | `String` (UTF-8)  | **Logical Type Identity**. Determines which Opener to use.   |
| `schema_version`   | `u64` (LE)        | Data layout version. Managed by store logic for migrations.  |
| `tip/<pubkey>`     | `[u8; 32]`        | Last applied Op Hash per author (Causal Checkpoint).        |

## Store Type Semantics

The `store_type` is a **Logical Identifier**, not a code hash.

- **Format**: Namespaced string with colon delimiter (e.g., `core:kvstore`, `core:logstore`, `org.example.chat`).
- **Resolution**: The Node maintains a **Registry** mapping `store_type` -> `Opener`.
- **Immutability**: The `store_type` is set at creation and **NEVER changes** for the lifetime of the store.

## The "Immutable Type" Philosophy

To ensure deterministic convergence in a distributed system, the interpretation of a store's history must remain constant.

1. **Identity = Logic + Data**: A store instance is defined by its initial logic type. You cannot "swap" the logic of an existing store instance (e.g., from Key-Value to SQL) without breaking the state hash history.
2. **Upgrades via Migration**: To change logic significantly (e.g., v1 -> v2 with new rules):
   - Create a **New Store** (new UUID, `store_type="my.app.v2"`).
   - Migrate data from Old -> New.
   - This prevents "Split Brain" scenarios where peers disagree on the rules of a single store ID.
3. **Hot Fixes (Native & Wasm)**:
   - **Optimization is Safe**: You can change the code (swap the Wasm binary or native build) if it does **not** affect the resulting `state_hash` for a given history.
   - **Maintainer Responsibility**: The system enforces correctness via the State Hash. If a maintainer pushes a Wasm update that changes the logic (and therefore the hash) without a migration, the node will simply reject the state as corrupt. The burden is on the maintainer to ensure `v1.1` binary behaves identical to `v1.0` binary for all historical Ops.

## Generic Open Workflow

The `StoreManager` can open any store without knowing its type in advance:

1. **Peek**: The manager reads `state.db/meta` to extract `(store_id, store_type, version)`. This is done via `StateBackend::peek_info()` without initializing the full store logic.
2. **Resolve**: The manager looks up `store_type` in its `registry`.
   - **Native**: Maps to registered `StoreOpener` closures.
   - **Wasm** (Future): Maps to a `WasmStoreOpener` that fetches the specific Wasm binary for that type/version.
3. **Open**: The resolved Opener initializes the store logic.

## Future: Wasm & Content-Addressable Storage

This architecture supports "Code-as-Data" without baking it into the persistence layer:

- **Registry**: A distributed registry (or gossip) can map `store_type="my.app.v1"` to a **CID** (IPFS/CAS hash).
- **Loader**: The `WasmStoreOpener` uses this map to fetch the Wasm blob from a local cache or peer, then instantiates the store.
- **Safety**: The `store_type` string acts as the stable anchor. The CID provides the immutable code.
