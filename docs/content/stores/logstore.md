---
title: "Log Store"
weight: 2
---

An append-only log. Entries are immutable, chronologically ordered, and never conflict. Store type: `core:logstore`.

## Data Model

Each entry is stored with a composite key that ensures chronological ordering regardless of insertion order:

```
Key (40 bytes):  [HLC wall_time (8 bytes BE)] [Author PubKey (32 bytes)]
Value:           [content_length (u64 LE, 8 bytes)] [content bytes]
```

Big-endian wall_time gives correct B-tree sort order. The author PubKey breaks ties when two entries share a timestamp. Duplicate `(wall_time, Author)` pairs are rejected as `DuplicateTimestamp` — the HLC `counter` field is not encoded in the key.

Entries have **no causal dependencies** on each other — each append submits with an empty `causal_deps` vector. Concurrent appends from different authors simply coexist as separate entries.

## Commands and Queries

| Method | Kind | Description |
|--------|------|-------------|
| **Append** | Command | Append content bytes. Returns the intention hash. |
| **Read** | Query | Read entries. Optional `tail` parameter returns only the last N. |

```protobuf
message AppendRequest { bytes content = 1; }
message AppendResponse { bytes hash = 1; }

message ReadRequest { optional uint64 tail = 1; }
message ReadResponse { repeated bytes entries = 1; }
```

`Read` returns content bytes only (not timestamps or authors). Entries are always in chronological order. `tail: None` returns all entries; `tail: Some(N)` returns the last N.

## Follow Stream

The log store provides a `Follow` stream that emits new entries as they are appended. Unlike the KV store's prefix-filtered watch, `Follow` takes no parameters — all new entries are delivered.

```protobuf
message FollowParams {}
message LogEvent { bytes content = 1; }
```
