# PN-Counter Test Cases

Summary of all counter test cases with pseudo-code descriptions.

## Basic Operations

### `test_counter_basic_increment`
```
Alice: incr(key, 10) → 10
```

### `test_counter_basic_decrement`
```
Alice: incr(key, 10) → 10
Alice: decr(key, 3) → 7
```

### `test_counter_negative_total`
```
Alice: decr(key, 5) → -5
```

## Concurrent Operations (Two Authors)

### `test_counter_concurrent_increments_two_nodes`
```
Alice: incr(key, 10, parent=None)  // Creates head A
Bob:   incr(key, 5, parent=None)   // Creates head B (concurrent)

Result: 2 heads, merged total = 15
```

### `test_counter_concurrent_inc_dec_two_nodes`
```
Alice: incr(key, 10, parent=None)  // +10
Bob:   decr(key, 3, parent=None)   // -3 (concurrent)

Result: merged total = 7
```

### `test_counter_same_key_inc_dec_different_authors`
```
Alice: incr(key, 10)
sync to store2
Bob:   decr(key, 3, citing Alice's head)

Result: single head, Alice (10,0) + Bob (0,3) = 7
```

## Sequential Operations

### `test_counter_merge_when_citing_parent`
```
Alice: incr(key, 100)  // Intention 1
Alice: incr(key, 50, parent=Intention1)  // Intention 2

Result: total = 150 (not 100+100+50)
```

### `test_counter_sequential_reset`
```
Alice: incr(key, 10)
Alice: delete(key, parent=incr)
Alice: incr(key, 5, parent=delete)

Result: total = 5 (reset by delete)
```

## Idempotency & Order Independence

### `test_counter_replay_head_dedup`
```
Apply same intention to store1 and store2
Result: both stores have identical heads (no duplicates)
```

### `test_counter_order_independence`
```
Store1: apply [intention_a, intention_b]
Store2: apply [intention_b, intention_a]

Result: same total regardless of apply order
```

## Multi-Node Sync

### `test_counter_offline_operations_then_sync`
```
Alice: incr(key, 10)
sync to Bob
-- both go offline --
Alice: incr(key, 5) × 3
Bob:   incr(key, 2) × 2
-- sync --

Result: 10 + 15 + 4 = 29 (all ops preserved)
```

### `test_counter_three_nodes_late_joiner`
```
Alice: incr(key, 10)
sync to Bob
Alice: incr(key, 5)
Bob:   incr(key, 3)
-- Charlie joins, syncs from Alice and Bob --

Result: Charlie sees 10 + 5 + 3 = 18
```

## Type Conflicts

### `test_counter_type_conflict_put_vs_incr`
```
Alice: incr(key, 10)  // Counter head
Bob:   put(key, "hello")  // Raw head (concurrent)

Result: get() returns type of highest HLC winner
```

### `test_counter_incr_merges_raw_and_counter_heads`
```
Alice: put(key, "hello")
Bob:   incr(key, 10, parent=None)  // Concurrent
sync
Alice: incr(key, 5, citing both heads)

Result: Counter (raw bytes ignored), total = 15
```

## Delete Interactions

### `test_counter_increment_concurrent_with_delete`
```
shared: incr(key, 10)
Alice: delete(key, parent=shared)  // Tombstone
Bob:   incr(key, 1, parent=shared)  // Concurrent (Add-Wins)
sync

Result: total = 11 (live counter wins over tombstone)
```

## Edge Cases

### `test_counter_overflow_safety`
```
Alice: incr(key, u64::MAX)
Alice: incr(key, 1000)

Result: saturates at i64::MAX (no panic)
```

### `test_counter_atomic_batch_ops`
```
Intention with ops: [incr(key, 10), decr(key, 2), incr(key, 5)]

Result: total = 13 (all ops in batch applied atomically)
```

### `test_counter_intra_intention_same_key_accumulation`
```
Intention with ops: [incr(key, 1), incr(key, 1)]

Result: total = 2 (not 1, both ops accumulated)
```

## Actor Layer

### `test_counter_concurrent_via_handle`
```
StoreHandle: incr(key, 10)
StoreHandle: incr(key, 5, from another "node")

Result: verifies actor layer correctly handles concurrent increments
```
