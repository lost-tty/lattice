---
title: "PN-Counter Example"
status: example
---

> **Status**: Design template. Not implemented. Useful as a reference for building custom CRDTs on Lattice.

A state-based PN-Counter (Positive-Negative Counter) implemented as a module on top of raw KV APIs.

## Storage

Single key per counter, value is serialized `CounterState`:

```rust
struct CounterState {
    p: HashMap<PubKey, u64>,  // positive increments per node
    n: HashMap<PubKey, u64>,  // negative decrements per node
}

impl CounterState {
    fn total(&self) -> i64 {
        let pos: u64 = self.p.values().sum();
        let neg: u64 = self.n.values().sum();
        pos as i64 - neg as i64
    }
    
    fn merge(&mut self, other: &Self) {
        for (node, val) in &other.p {
            let entry = self.p.entry(*node).or_insert(0);
            *entry = (*entry).max(*val);
        }
        for (node, val) in &other.n {
            let entry = self.n.entry(*node).or_insert(0);
            *entry = (*entry).max(*val);
        }
    }
}
```

## API

```rust
pub struct Counter<'a> {
    store: &'a StoreHandle,
    node_id: PubKey,
    key: Vec<u8>,
}

impl Counter<'_> {
    pub async fn get(&self) -> Result<i64, NodeError> {
        let heads = self.store.get(&self.key).await?;
        // Decode each head as CounterState, merge all, return total
    }
    
    pub async fn incr(&self, delta: i64) -> Result<(), NodeError> {
        // Get merged state
        // If delta > 0: state.p[self.node_id] += delta
        // If delta < 0: state.n[self.node_id] += -delta
        // put() serialized state
    }
}
```

## Multi-Head Merge

When multiple heads exist (concurrent writes from different nodes):

1. Decode each head as `CounterState`
2. Merge: take max per slot for both `p` and `n` maps
3. Result: `sum(p) - sum(n)`

Each node only ever increases its own slots, so max is the correct merge.

## Proto

```protobuf
message CounterState {
    map<bytes, uint64> p = 1;  // positive slots
    map<bytes, uint64> n = 2;  // negative slots
}
```
