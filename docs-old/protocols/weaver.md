# Weaver Protocol Specification (v1)

This document defines the core data structures and serialization rules for the Weaver Protocol.

## 1. Data Model

### 1.1 Intention
An `Intention` is the unsigned body of an atomic transaction targeting a specific Store.
This is the content that gets hashed and signed. The signature lives in `SignedIntention`.

Field order matches the canonical Borsh serialization order.

```rust
struct Intention {
    // 1. Identity
    author: PubKey,           // Ed25519 public key (32 bytes)

    // 2. Metadata
    timestamp: HLC,           // Hybrid Logical Clock (wall_time_ms, counter)

    // 3. Target
    store_id: Uuid,           // The Store this Intention applies to

    // 4. Linearity (Per-Author Chain)
    // Hash of the previous Intention by this author in this store.
    // Hash::ZERO if this is the author's first write to the store.
    store_prev: Hash,

    // 5. Causal Graph
    condition: Condition,     // Explicit dependencies (DAG links)

    // 6. Payload
    ops: Vec<u8>,             // Opaque operation bytes (interpretation left to the state machine)
}
```

### 1.2 SignedIntention
The wire/storage envelope. Wraps an `Intention` with its cryptographic proof.

```rust
struct SignedIntention {
    intention: Intention,
    signature: Sig,           // Ed25519 signature over blake3(borsh(intention))
}
```

**Signing:** `signature = Ed25519.sign(signing_key, blake3(borsh(intention)))`
**Verification:** `Ed25519.verify(intention.author, blake3(borsh(intention)), signature)`

### 1.3 Condition (The Dependency Graph)
The `Condition` enum defines the causal dependencies (DAG links) required for the Intention to be applied.

```rust
enum Condition {
    // V1: All listed hashes must be applied before this Intention can be applied.
    V1(Vec<Hash>),

    // Future variants (V2+) reserved for programmable logic.
}
```

### 1.4 Operations
The `ops` field is **opaque bytes**. The store's state machine decides how to interpret them.
By convention, operations are wrapped in `UniversalOp`:

```protobuf
message UniversalOp {
    oneof op {
        bytes app_data = 10;   // Application-specific payload (e.g. KV put/delete)
        SystemOp system = 11;  // System operations (hierarchy, peers, invites)
    }
}
```

System operations include hierarchy management (`ChildAdd`, `ChildRemove`), peer management (`SetPeerStatus`), and invite management. Application stores (e.g. KvStore) put their own serialized operations in `app_data`.

---

## 2. Canonical Serialization (Borsh)

Weaver uses **Borsh** (Binary Object Representation Serializer for Hashing) for hashing and signing.
-   **Specification:** [borsh.io](https://borsh.io)
-   **Why:** Strict, portable, deterministic, and designed for consensus.

### 2.1 Hashing Rules
1.  **Format:** Little-endian, integers are fixed width.
2.  **Structs:** Fields are written in declaration order.
3.  **Hash function:** `blake3(borsh(intention))` produces the 32-byte content hash.

### 2.2 Condition Canonicalization
The `Vec<Hash>` in `Condition::V1` MUST be **sorted lexically** (byte-wise) before serialization.
This is enforced by `Condition::v1()` which sorts on construction.

---

## 3. Debug View (S-Expression)

For debugging and inspection, intentions are rendered as structured S-Expressions
via `store debug intention <hash-prefix>`.

The server decodes `ops` using the store's state machine (`Introspectable`)
so both system and application operations are always fully expanded.

### 3.1 Format

```lisp
(intention
  (hash #abcdef01...)
  (author #ed25519-pubkey)
  (store-id #uuid-bytes)
  (store-prev #hash-of-previous-intention)
  (condition (v1 #dep-hash-1 #dep-hash-2))
  (timestamp 1234567890 :counter 0)
  (signature #ed25519-sig)
  (ops
    (system (child-add #uuid "alias"))))
```

Application data example (KvStore):
```lisp
  (ops
    (data (put "key1" "val1")))
```

### 3.2 Pretty Printing

`SExpr::to_pretty()` renders multi-line output with indentation.
Top-level list children are each placed on their own line; nested leaf lists stay inline.

---

## 4. Validation Logic

A Node accepts an Intention `I` if and only if:

1.  **Signature Valid:** `Ed25519.verify(I.author, blake3(borsh(I)), signature)` is TRUE.
2.  **Linearity Valid:** `I.store_prev` matches the current tip of `I.author`'s chain in `I.store_id` (or is `Hash::ZERO` for new authors).
3.  **Dependencies Met (for application):** For every `h` in `I.condition.V1`:
    -   `h` has been applied to the local state.
    -   If dependencies are not yet met, the Intention is stored but **floats** (unapplied) until they arrive.

### 4.1 Floating Intentions
When an Intention passes signature and linearity checks but has unmet causal dependencies, it is stored in the intention store but not applied to the state machine. These are called **floating intentions**. They are automatically applied once their dependencies are met (e.g. after sync delivers the missing intentions).

---

## 5. Witness Log (Total Apply Order)

When an Intention is applied to the state machine, a `WitnessRecord` is appended to the witness log.
The canonical data type is the proto-generated `lattice_proto::weaver::WitnessRecord`:

```protobuf
message WitnessContent {
    bytes store_id = 1;          // UUID of the store
    bytes intention_hash = 2;    // blake3 hash of the applied intention
    uint64 wall_time = 3;        // Wall-clock time when witnessed (Unix ms)
    bytes prev_hash = 4;         // blake3 hash of previous WitnessRecord.content (32 bytes, all-zeros for first)
}

message WitnessRecord {
    bytes content = 1;           // protobuf-encoded WitnessContent
    bytes signature = 2;         // Ed25519 sign(node_key, blake3(content))
}
```

### 5.1 Hash Chain Integrity

The `prev_hash` field creates a tamper-evident chain across the witness log:
- The first witness record has `prev_hash = [0u8; 32]` (genesis sentinel).
- Each subsequent record sets `prev_hash = blake3(previous_record.content)`.
- On startup, `IntentionStore::rebuild_indexes()` verifies the entire chain. Any break is a hard corruption error.

The `IntentionStore` caches `last_witness_hash` in memory for O(1) chain extension.

### 5.2 Signing and Verification

Witness records use the same **content + envelope** pattern as `SignedIntention`.

**Signing:** `signature = Ed25519.sign(node_key, blake3(WitnessContent.encode()))`
**Verification:** `Ed25519.verify(node_pubkey, blake3(content), signature)`

Helpers: `sign_witness()` and `verify_witness()` in `lattice-kernel/src/weaver/witness.rs`.
