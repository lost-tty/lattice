# Test Cases

## Timestamp / HLC

### Time-traveling node applies own intention
- Node X has clock at year 2050
- Node X creates intention, signs, broadcasts
- All nodes (including X) should clamp to parent.hlc + 1
- Verify: X's state.db matches other nodes' state.db
- Failure mode: X applies using 2050, others use 101 → divergence

### Clamping with no parent (genesis intention)
- Node X creates first-ever intention with future timestamp
- All nodes should DROP the intention (no parent to anchor to)
- Verify: intention is not applied anywhere

### Out-of-order intention arrival
- Intention B (hlc=91) arrives after Intention A (hlc=100)
- Both write to same key
- Verify: A's value wins (LWW with timestamp tracking)
- Verify: no rollback needed, just comparison on apply

### Clock drift detection
- Node consistently sees its intentions clamped
- Verify: UI alerts user about clock being ahead

### Clock in past (Pi without RTC, boots at 1970)
- Node X has clock at 1970
- Node X receives intentions from peers with HLC around 2024
- Standard HLC: X uses max(1970, peer_hlc + 1) = peer_hlc + 1
- Verify: X's intentions slot in correctly (no special handling needed)

### Pre-flight peer sanity check (future clock)
- Node X has clock at 2050
- Before creating intention, X compares local_clock to max_peer_hlc
- If local_clock > max_peer_hlc + MAX_DRIFT, use max_peer_hlc + 1
- Verify: X's intention uses sane timestamp, all nodes agree
