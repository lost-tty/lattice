---
title: "KV Store Watch & CRDT Design"
status: design
---

> **Status**: Design exploration. The patterns described here (prefix watch, regex watch) are not yet fully implemented.

## I. Prefix Watch + CRDTs + KV Store

### Layer Architecture

The architecture consists of three distinct layers with specific responsibilities.

```
┌─────────────────────────────────────────────────────────────────┐
│                         Application                             │
├─────────────────────────────────────────────────────────────────┤
│   Prefix Watch         │         CRDTs                          │
│   (reactive layer)     │   (merge semantics layer)              │
├─────────────────────────────────────────────────────────────────┤
│                    KV Store (storage layer)                     │
│             Keys → Values (arbitrary bytes or typed)            │
└─────────────────────────────────────────────────────────────────┘
```

* **Prefix Watch:** Determines *when* to react (notification mechanism).
* **CRDTs:** Determines *how* to merge data (conflict resolution).
* **KV Store:** Determines *where* data persists (storage).

These components are orthogonal and composable. The storage layer handles byte persistence, while the application layer composes the watch mechanism with CRDT deserialization.

### Semantic Scope

| Concern              | Prefix Watch               | CRDTs                                                  | Overlap |
|----------------------|----------------------------|--------------------------------------------------------|---------|
| **Change Detection** | ✅ Notifies on put/delete  | ❌ No notification mechanism                           | None    |
| **Merge Strategy**   | ❌ Handles raw bytes       | ✅ Defines merge rules (associativity/commutativity)   | None    |
| **Data Structure**   | ❌ Opaque bytes            | ✅ Typed structure (Map, Set, Counter)                 | None    |

### Interaction Considerations

#### 1. Internal Updates vs. External Puts

CRDTs maintain internal state (e.g., a `{node_id: count}` vector clock or dot vector).

* **Watch behavior:** The watch triggers whenever the serialized byte array associated with the key changes.
* **Implication:** If an internal state update (metadata) changes the byte representation without changing the user-facing value, a watch event is still generated.

#### 2. Granularity Mismatch

* **Prefix Watch:** Operates at **key-level** granularity (e.g., `/peers/abc123`).
* **CRDT OR-Set:** Operates at **element-level** granularity (e.g., a member added/removed from a set).

If multiple peers are stored within a single CRDT OR-Set under the key `/peers`, a change to one peer results in a `Put` event for the entire set. The application must deserialize and diff the set to determine the specific element change.

### Implementation Models

| Approach             | Architecture                                     | Characteristics                                                                                             |
|----------------------|--------------------------------------------------|-------------------------------------------------------------------------------------------------------------|
| **Decoupled**        | Prefix Watch at key level; CRDTs as value types. | Watch layer is agnostic to data types. Application handles deserialization and logic.                       |
| **Entity−per−Key**   | One key per entity (e.g., `/peers/{id}`).        | Aligns storage granularity with CRDT logic. Minimizes serialization overhead on updates.                    |
| **Deep Integration** | Store is CRDT−aware.                             | Capable of emitting semantic events (e.g., `CounterChanged`). Increases coupling between storage and logic. |

---

## II. Prefix Watch vs. Regex/Glob Watch

### Watch Mechanism Comparison

```rust
// Prefix Match
store.watch_prefix(b"/peers/")

// Glob Match
store.watch_glob("/nodes/*/status")

// Regex Match
store.watch_regex(r"/counters/\d+")
```

### Performance Analysis

| Approach   | Index Compatability   | Time Complexity      | Access Pattern                                    |
|------------|-----------------------|----------------------|---------------------------------------------------|
| **Prefix** | ✅ B−tree Range Scan  | O(log N + M)         | Contiguous keys (e.g., directory structures).     |
| **Glob**   | ⚠️ Partial            | O(log N + M) (prefix)| Wildcards used in the middle of keys.             |
| **Regex**  | ❌ Full Scan          | O(N)                 | Arbitrary pattern matching.                       |

*Note: In B-tree based stores (like redb or sled), prefix searches utilize range scans. N is total keys, M is number of matches.*

### Key Hierarchy Design

The efficiency of the watch mechanism depends on the key hierarchy structure relative to the access pattern.

**Pattern A: Entity-First Hierarchy**

* **Structure:** `/users/{id}/settings/theme`
* **Query:** "Watch all themes"
* **Mechanism:** Requires Regex/Glob (`/users/*/settings/theme`) or multiple watches.
* **Performance:** Higher complexity for aggregation queries.

**Pattern B: Feature-First Hierarchy**

* **Structure:** `/settings/theme/users/{id}`
* **Query:** "Watch all themes"
* **Mechanism:** Prefix Watch (`/settings/theme/`) covers all target keys.
* **Performance:** O(log N + M) via range scan.

Aligning the key hierarchy with the primary read/watch patterns allows for the use of prefix scans over linear scans.
