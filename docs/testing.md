# Testing Scenarios (Validation Apps)

These apps test HLC ordering, gossip convergence, and conflict resolution.

## Level 1: Pixel Board (Visual Convergence)

50x50 collaborative grid where users paint pixels.

**Data Model:** `/canvas/{x}_{y}` → `{hex_color}`

**Tests:**
- Visualize sync disagreements immediately
- High write volume (log performance)
- Simultaneous writes (HLC tiebreaker)

**Scenario:** Node A paints all red (offline), Node B paints all blue (offline), connect. Board must be identical on both.

---

## Level 2: Shared Grocery List (LWW Trap)

List with add/check/delete operations.

**Data Model:** `/list/{item_uuid}` → `{ name, status: "needed"|"bought" }`

**Tests:** Exposes LWW weakness (resurrection bug)

**Scenario:**
1. Alice syncs, sees "Milk", goes offline, marks "bought"
2. Bob syncs, sees "Milk", deletes it
3. Reconnect

**Result:** Item either resurrects or vanishes based on timestamp. Forces tombstone pattern.

---

## Level 3: Chat Room (Causal Ordering)

Group chat application.

**Data Model:** `/chat/{channel}/{timestamp}_{node_id}` → `{ msg }`

**Tests:**
- HLC causal ordering
- Prefix queries (redb range scans)
- Gap detection via vector clocks

**Scenario:**
1. Node A sends "Msg 1"
2. Node B sees it, replies "Msg 2"
3. Node C comes online, connects only to B

**Success:** Node C receives "Msg 1" before/with "Msg 2" (transitive sync).

---

## Level 4: Chaos Monkey (Automated Simulation)

Tokio-based simulation harness with in-memory networking.

**Setup:**
- 5 node threads in one process
- In-memory network (tokio channels)
- Chaos monkey randomly: cuts connections, writes random keys, sleeps threads

**Assertion:**
```rust
let state_0 = nodes[0].dump_state_hash();
for i in 1..5 {
    assert_eq!(state_0, nodes[i].dump_state_hash());
}
```

Catches HLC clamping edge cases that manual testing misses.
