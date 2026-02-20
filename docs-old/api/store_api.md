# Store Context API

The core interaction point with Lattice data is via the `StoreManager` and the specific handles it issues. Any interaction with the database happens through an `AuthorizedStore` wrapper.

## Interacting with the State Machine

When you open a store via the `StoreManager`, you receive an `AuthorizedStore` handle, which provides an abstraction over the underlying `ReplicationController` and `StateMachine`.

```rust
// lattice-kvstore example interaction
let handle = store_manager.open_store("my-workspace").await?;

// The handle verifies permissions before committing
handle.put(b"key1", b"value1").await?;

let val = handle.get(b"key1").await?;
```

Because Lattice utilizes Intention DAGs (not simple registers), the physical writes are translated behind the scenes:

1.  **Operation Formation:** `put(b"key1")` produces an `Op::Put`.
2.  **Causal Dependency:** The `ReplicationController` queries the current DAG heads for `key1` to cite as `causal_deps`.
3.  **Signature:** The `Op` and dependencies are hashed and signed by the node's Ed25519 identity, forming an Intention.
4.  **Logging & Broadcasting:** The Intention is durably written to `log.db`, pushed out via Gossip, and finally applied to the `KvState` in `state.db`.

## Implementing Custom Stores

The power of Lattice lies in its generic replication engine. The `ReplicationController` does not care whether the data is a Key-Value pair, an OR-Set, a CRDT Counter, or a BLOB. 

To implement a custom data structure (e.g., a collaborative Text Editor), you implement the `StateMachine` trait:

```rust
pub trait StateMachine: Send + Sync {
    fn apply(&self, intention: &SignedIntention, txn: &mut dyn StorageTransaction) -> Result<()>;
}
```

The Engine guarantees that your `apply` method will only be called with cryptographically valid Intentions that form an unbroken mathematical chain. The StateMachine's only job is to update `state.db` according to the payload's business logic.
