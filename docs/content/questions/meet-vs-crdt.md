---
title: "Conflict Resolution Architecture"
status: resolved
resolution: "Approach A, slimmed down. State machines keep per-conflict-domain head tracking, but store only intention hashes — not duplicated values, HLC, or author metadata. Values are resolved at apply time (LWW). Conflict details (payloads, authors, timestamps) are read from the DAG on demand. Each store type defines its own conflict domains and resolution semantics. The kernel provides DAG query primitives (LCA, paths, ancestry) but does not own conflict tracking — that remains store-specific."
---

# Conflict Resolution Architecture

## Definitions

Let <span class="math">(H, &lt;)</span> be a DAG of intentions where <span class="math">h₁ &lt; h₂</span> iff <span class="math">h₁</span> is in <span class="math">deps(h₂)</span> (transitive causal dependency). Each intention <span class="math">h ∈ H</span> carries:

- <span class="math">ops(h)</span>: an opaque payload interpreted by a state machine <span class="math">M</span>
- <span class="math">deps(h) ⊂ H</span>: the set of intention hashes the author observed at write time (the `condition.V1` field)

Two intentions <span class="math">a, b ∈ H</span> are **concurrent** iff <span class="math">a ≮ b</span> and <span class="math">b ≮ a</span> — neither author had seen the other's intention at write time.

A **state machine** <span class="math">M</span> is a tuple <span class="math">(S, s₀, apply)</span> where <span class="math">S</span> is the state space, <span class="math">s₀</span> the initial state, and <span class="math">apply: S × Op → S</span>. The platform delivers all intentions to every replica; <span class="math">apply</span> receives the full <span class="math">deps(h)</span> with each operation.

<span class="math">M</span> is a **CRDT** if for all pairs of concurrent intentions <span class="math">a, b</span> and all reachable states <span class="math">s</span>:

<div class="math-block">apply(apply(s, a), b) = apply(apply(s, b), a)</div>

i.e., concurrent operations commute. This guarantees all replicas converge to the same state regardless of delivery order.

The **meet** (lowest common ancestor) of two intentions <span class="math">a, b</span> in <span class="math">(H, &lt;)</span> is:

<div class="math-block">lca(a, b) = max{ m ∈ H | m ≤ a ∧ m ≤ b }</div>

## System Description

The platform replicates intentions across nodes. Each node applies them to a local state machine. One concrete state machine (`KvState`) maintains a **headlist** per key <span class="math">k</span>:

<div class="math-block">heads(k) = { h ∈ H | h wrote to k ∧ ∄ h' &gt; h that wrote to k }</div>

New writes subsume heads listed in their <span class="math">deps</span>. Reads project from <span class="math">heads(k)</span> via a strategy <span class="math">σ: P(S) → S</span> (e.g., LWW by timestamp, first-write-wins, return all).

## Two Approaches

**Approach A (CRDT-internal):** The state machine <span class="math">M</span> embeds sufficient metadata in <span class="math">S</span> to merge concurrent operations at apply-time, using <span class="math">deps(h)</span> to determine causal relationships. No external infrastructure needed.

**Approach B (External Meet):** An operator outside <span class="math">M</span> computes <span class="math">m = lca(a, b)</span>, extracts the two branches <span class="math">P(m, a)</span> and <span class="math">P(m, b)</span> (the paths from <span class="math">m</span> to each head), reconstructs <span class="math">M</span>'s state at <span class="math">m</span>, replays each branch independently, and performs a 3-way diff <span class="math">(S_m, S_a, S_b)</span>.

Approach B requires either snapshot infrastructure to recover <span class="math">S_m</span>, or replay from <span class="math">s₀</span> through all <span class="math">h ≤ m</span> — which is <span class="math">O(|H|)</span> in the worst case.

## Questions

**(a)** Given that <span class="math">apply</span> receives full <span class="math">deps(h)</span> context, is there a class of conflict between concurrent intentions that a CRDT-style state machine fundamentally cannot resolve internally? Or is Approach A theoretically complete — i.e., for any desired merge semantics, does there exist a state space <span class="math">S</span> and <span class="math">apply</span> function that achieves it using only the information available at apply-time?

**(b)** Approach B requires reconstructing <span class="math">S_m</span> at arbitrary points in <span class="math">(H, &lt;)</span>. No snapshot infrastructure exists. Evaluate: is it more cost-effective to build replay/snapshot infrastructure for Approach B, or to invest in abstractions that make Approach A easier to implement correctly (reusable headlist primitives, commutativity helpers, causal context utilities)?

**(c)** Is there value in implementing <span class="math">lca(a, b)</span> and <span class="math">P(m, a)</span> as kernel primitives even if the primary resolution strategy is Approach A? What are the concrete use cases beyond debugging/visualization?

**(d)** Some conflicts require human judgment. A multi-value register CRDT preserves all concurrent values and defers resolution to the reader — but this only works when <span class="math">S</span> can represent "unresolved" states. Is this sufficient for all human-in-the-loop scenarios, or are there cases where Approach B is required to present meaningful choices to a user?

**(e)** Currently <span class="math">apply</span> only sees the current state <span class="math">S</span> and the incoming operation (including <span class="math">deps(h)</span>). For Approach A to work in the general case, would state machines need the ability to traverse the DAG backwards along <span class="math">deps</span> edges — i.e., access the full causal history, not just the current state? If so, what does that imply for the <span class="math">apply</span> interface — should it receive a handle to the intention store, or should the necessary causal context be pre-computed and passed in?

**(f)** What if the operation space were required to carry algebraic structure — inverses, commutativity, composition — such that convergence and historical state reconstruction are consequences of the algebra alone? What is the minimal structure on <span class="math">Op</span> that provides both properties, and what class of state machines can be expressed within it?

**(g)** The headlist pattern localizes conflicts to individual keys. But some data models have **cross-key invariants** — uniqueness constraints, referential integrity, structural acyclicity — where two intentions that are individually valid produce an invalid combined state. Neither intention "conflicts" with the other at any single key, yet the system is in a broken state. Can Approach A handle this class of conflict, or does it require fundamentally different machinery?

---

## Analysis: Algebraic Constraints on Operations

### Abelian Group Attempt

A natural candidate is to require <span class="math">(Op, ∘, ⁻¹, e)</span> to form an **abelian group**. This would give:

- **Convergence** from commutativity: concurrent operations can be applied in any order
- **Free rollback** from inverses: <span class="math">down(o) = o⁻¹</span>
- **State at any DAG point** by composing inverses of all operations after the target

### The Idempotency Contradiction

Abelian groups are fundamentally incompatible with idempotency. In any group, if <span class="math">a ∘ a = a</span>, then multiplying both sides by <span class="math">a⁻¹</span> yields <span class="math">a = e</span>. The only idempotent element in a group is the identity.

Most real-world data structures require idempotent operations — applying `put("color", "blue")` twice must equal applying it once. This rules out the abelian group approach for KV registers, sets, flags, and any structure where repeated application is a no-op.

### The Semilattice Alternative

CRDTs resolve this by using **join-semilattices** <span class="math">(S, ⊔)</span> — structures that are idempotent, commutative, and associative:

<div class="math-block">a ⊔ a = a &nbsp;&nbsp;&nbsp; a ⊔ b = b ⊔ a &nbsp;&nbsp;&nbsp; (a ⊔ b) ⊔ c = a ⊔ (b ⊔ c)</div>

This gives convergence and idempotency but **no inverses** — and therefore no free rollback, no cheap historical state reconstruction.

### The Three-Way Trade-Off

The properties form an impossible triangle for non-trivial structures:

| Property | Abelian Group | Join-Semilattice |
|----------|:---:|:---:|
| Commutativity (convergence) | ✓ | ✓ |
| Idempotency (at-least-once safe) | ✗ | ✓ |
| Invertibility (rollback) | ✓ | ✗ |

No single algebraic structure provides all three for non-trivial operations.

### Open Sub-Questions

**(f.i)** Is there a weaker structure that provides *partial* invertibility alongside idempotency? For instance, can operations be partitioned into an invertible subgroup (counters, accumulators) and an idempotent semilattice (registers, flags), composed in a product structure <span class="math">Op = G × L</span>? What are the practical implications of such a decomposition?

**(f.ii)** If no purely algebraic path to free rollback exists for idempotent operations, does this settle the architecture question — Approach A (CRDT metadata) is the only general mechanism, with Approach B (Meet/replay) reserved for cases where the state machine cannot or does not maintain sufficient merge metadata?

---

## Analysis: Human-in-the-Loop Without Meet

Question **(d)** asks whether Approach B is required for human-in-the-loop conflict resolution. The short answer: **no**. Approach A provides sufficient machinery if the system is designed around a few principles.

### The Information Available at Read Time

When a key <span class="math">k</span> has multiple heads, the state machine already knows:

- The **set of concurrent values** in <span class="math">heads(k)</span>
- The **causal deps** of each head — which intentions each author had observed
- The **operation payloads** of each head intention (put, delete, etc.)

This is strictly more information than a traditional 3-way merge tool receives. A 3-way merge sees <span class="math">(S_m, S_a, S_b)</span> — three opaque snapshots. A headlist-based system sees the *operations themselves*, their causal relationships, and optionally the full chain of prior operations via <span class="math">deps</span> traversal.

### Four Mechanisms

**Operation-aware presentation.** Conflicts are shown to users as *what each party did* (e.g., "Alice set price to €12, Bob set price to €15") rather than as raw diverged state. The intentions carry semantic meaning — they are the user's atomic units of work. This requires no replay infrastructure; the intention payloads are stored in the DAG.

**Transactional intention boundaries.** Each intention is an atomic, coherent unit of work. When presenting a conflict, the system can show the full intention — all keys it touched, its timestamp, its author — giving the human resolver complete context about *why* the conflicting write was made.

**Client-side baseline.** The writing client knows the state it observed before issuing a write (it read it). If the client caches this baseline (or the system records it as part of the intention metadata), the diff between baseline and new value is available without replaying the DAG. The 3-way view <span class="math">(baseline, mine, theirs)</span> can be reconstructed from local information.

**Conflict markers as state.** The multi-value register pattern — returning all concurrent values and deferring resolution — is a first-class CRDT. The state <span class="math">S</span> explicitly represents "unresolved" as <span class="math">|heads(k)| > 1</span>. The resolution write is just another intention that causally depends on all current heads, collapsing them to a single value. No special infrastructure is needed; this is a normal write.

### When Approach B Adds Value

Approach B becomes relevant in narrow scenarios:

- **Structural merging** of complex values (e.g., merging two concurrent edits to a JSON document at the field level) where the state machine does not decompose the value into individually-trackable sub-keys
- **Automated resolution policies** that require computing a semantic diff between branches — though even here, the operation log often contains more useful information than state snapshots

For the KV use case, where values are opaque blobs and keys are the unit of granularity, the headlist mechanism is sufficient for both automated and human-in-the-loop resolution.

### Implication for Architecture

For data models with **local conflicts** (conflicts confined to a single key), Approach A is theoretically complete for HITL scenarios. The intention DAG itself *is* the conflict record — it preserves not just the diverged states but the operations and causal context that produced them. Approach B reconstructs this information from state snapshots, which is strictly less informative.

However, this conclusion depends on the conflict being *detectable at the key level*. Data models with cross-key invariants (see the next section) introduce conflicts that no single key's headlist can detect, which complicates the picture.

---

## Analysis: Beyond Key-Value — Conflict Taxonomy by Data Model

The analyses above implicitly assume the KV case, where conflicts are local to individual keys. Different data models introduce fundamentally different classes of conflict. This section examines what changes when the platform hosts state machines beyond KV.

### The Locality Spectrum

A conflict is **local** if it is detectable and resolvable by examining a single key (or a single unit of state). A conflict is **global** if it involves an invariant spanning multiple keys, where each individual key's state is internally consistent but the combined state violates a system-wide property.

<div class="math-block">local: ∃k. |heads(k)| > 1 &nbsp;&nbsp;&nbsp;&nbsp; global: ∀k. |heads(k)| = 1 ∧ invariant(S) = false</div>

KV stores only have local conflicts. As data models grow richer, global conflicts emerge — and these are the hard cases.

### Key-Value Store

**Conflict surface:** Local only. Two concurrent writes to key <span class="math">k</span> produce <span class="math">|heads(k)| > 1</span>. Writes to different keys never conflict.

**Approach A fitness:** Complete. The headlist per key is the natural CRDT. Merge strategies (LWW, FWW, multi-value) operate independently per key. No cross-key reasoning needed.

**Approach B value:** Marginal. The operation payloads contain all information a 3-way diff would reconstruct.

### Document Store (Sub-Document Merging)

**Conflict surface:** Local if the state machine decomposes documents into per-field headlists. Consider a document:

<div class="math-block">{ "title": "Report", "status": "draft", "body": "..." }</div>

Alice updates `status` to `"review"`. Bob updates `body`. These are concurrent but non-conflicting — as long as the state machine tracks heads per field path rather than per document.

**Approach A fitness:** Good, with caveats. The state machine must maintain <span class="math">heads(doc_id, field_path)</span> rather than <span class="math">heads(doc_id)</span>. This is a CRDT map — each field is an independent register. The state space grows (one headlist per field per document), but the pattern is identical to KV.

The hard sub-case is **text fields**. Concurrent edits to the same text field (e.g., two users editing a paragraph) cannot be decomposed into independent sub-keys without a text CRDT (Automerge, Yjs-style sequence CRDT). This is Approach A taken to its logical extreme — the state machine embeds a rich CRDT for the text data type — but it requires the platform to support arbitrarily complex state machine internals.

**Approach B value:** Meaningful for text. Showing a human a 3-way character-level diff of a text field is a well-understood UX (Git merge conflicts). Reconstructing this from operation payloads alone requires the state machine to log character-level ops, which it may or may not do.

### Filesystem

**Conflict surface:** Both local and global. File *contents* produce local conflicts (same as KV — each file is a key). But the namespace is a tree with structural invariants:

- **Unique names per directory:** Two users concurrently create `/docs/report.txt` — neither sees the other's file, but the combined state has a name collision.
- **No orphans:** Alice deletes `/docs/` while Bob creates `/docs/notes.txt`. Bob's file now references a parent that doesn't exist.
- **No cycles:** Alice moves `/a/` into `/b/`. Bob moves `/b/` into `/a/`. The combined state has a cycle.
- **Referential integrity of moves:** Alice moves `/docs/report.txt` to `/archive/`. Bob renames it to `/docs/final.txt`. The file now has two locations, or the rename targets a path that no longer contains the file.

These are all **global conflicts** — no single key's headlist is in a conflicted state, but the tree as a whole is invalid.

**Approach A fitness:** Possible but expensive. The state machine must treat the *entire tree structure* as the CRDT, not individual files. Existing research (the INRIA filesystem CRDT) models this as a set of triples <span class="math">(parent, name, inode)</span> with causality-aware conflict resolution rules: last-writer-wins on name collisions, "move wins over delete" policies for orphans, cycle detection at apply time. The state machine needs:

- Per-name headlists within each directory (for name collisions)
- A parent pointer per inode with its own headlist (for concurrent moves)
- Cycle detection on every move operation
- Orphan resurrection or tombstoning policy

This is a significantly more complex state machine than KV, but it *is* an Approach A solution — all metadata lives in <span class="math">S</span>, all resolution happens at apply time.

**Approach B value:** High for UX. Presenting a human with "Alice moved X to /archive/, Bob renamed X to final.txt" as a structured tree diff — showing the base tree, Alice's tree, Bob's tree side by side — is much more legible than presenting raw concurrent operation payloads. The tree structure gives spatial context that a flat operation log does not.

### SQL (Relational)

**Conflict surface:** Dominated by global conflicts. The relational model is defined by constraints that span rows, columns, and tables:

- **Uniqueness:** Two concurrent inserts each satisfy `UNIQUE(email)` individually but violate it together. Neither row's headlist is conflicted — the conflict exists only in the index.
- **Referential integrity:** Alice deletes a `department` row. Bob inserts an `employee` referencing that department. Each operation is valid against the state its author observed.
- **Schema evolution:** Alice adds column `score INTEGER NOT NULL DEFAULT 0`. Bob adds column `score TEXT`. Concurrent schema changes produce a state where the column exists with contradictory definitions.
- **Check constraints:** `CHECK(quantity >= 0)`. Alice sets `quantity = 3`. Bob concurrently sets `quantity = 2`. Neither conflicts — but if a third concurrent operation subtracts 4, the constraint is violated in one branch but not the other.

**Approach A fitness:** Severely limited. The fundamental problem is that relational constraints are *predicates over global state*, not properties of individual rows. A headlist per row handles concurrent updates to the same row, but says nothing about cross-row invariants.

To make Approach A work, the state machine would need to:

1. Maintain headlists per row (for local conflicts)
2. Defer constraint checking to read time (treating the constraint violation itself as a "conflict marker")
3. Track which constraints are violated and which concurrent intentions contributed to the violation
4. Provide a resolution mechanism (human or automated) that can atomically resolve the constraint violation

This is essentially building a CRDT relational database. Research systems exist (Antidote DB, Electric SQL) but they impose significant restrictions: no arbitrary CHECK constraints, limited schema evolution, eventual consistency semantics that differ from SQL's expected isolation levels.

**Approach B value:** Also limited, but for different reasons. A 3-way merge of relational state must contend with schema differences between the base and the branches. If Alice altered the schema on her branch and Bob altered data on his, the base state's structure doesn't match either branch. Traditional 3-way merge assumes structural compatibility.

**Honest assessment:** SQL on a causal DAG requires either (1) restricting the SQL surface area to the point where cross-row constraints are absent — at which point it degrades to a document store with a query language — or (2) accepting that some operations must be serialized rather than merged, with a coordination protocol for constraint-violating regions of the state space.

### Summary

| Data Model | Local Conflicts | Global Conflicts | Approach A | Approach B | Key Challenge |
|---|---|---|---|---|---|
| KV | Per-key concurrency | None | Complete | Marginal | — |
| Document (opaque) | Per-document concurrency | None | Complete | Marginal | Same as KV |
| Document (sub-field) | Per-field concurrency | None | Complete | Text fields | Field-level headlists |
| Filesystem | File content concurrency | Name collisions, orphans, cycles, move conflicts | Possible (complex) | High UX value | Tree-wide structural CRDT |
| SQL | Per-row concurrency | Uniqueness, referential integrity, schema evolution | Severely limited | Also hard | Global constraint predicates |

### The Dividing Line

The critical variable is whether conflicts are **detectable at the unit of replication**. In KV, the unit of replication is the key, and all conflicts manifest as multiple heads on a single key. In a filesystem, the unit of replication is the file, but conflicts can manifest in the *relationships between files* — the directory tree structure. In SQL, the unit of replication is the row, but conflicts can manifest in *predicates over sets of rows* — constraints.

Approach A works cleanly when the state machine's conflict surface aligns with the headlist granularity. As the gap between replication unit and conflict surface widens, the state machine must embed increasingly sophisticated metadata to detect and resolve conflicts that no single headlist can see.

Approach B does not solve this problem either — it merely shifts the detection to a different phase (post-hoc diff rather than apply-time merge). The fundamental difficulty of global conflicts is that they require *global reasoning*, which neither approach escapes.

### Implications for Lattice

If the platform intends to support only KV and document stores, Approach A with per-key headlists is sufficient. Approach B infrastructure (LCA, path extraction, replay) has marginal value.

If the platform intends to support filesystems, investing in tree-structured CRDTs under Approach A is the primary cost, but Approach B primitives (LCA, path extraction) become valuable for presenting structural conflicts to humans.

If the platform intends to support relational semantics, neither approach alone suffices. The architecture would need a coordination layer for constraint-violating regions — a fundamentally different mechanism from both A and B.
