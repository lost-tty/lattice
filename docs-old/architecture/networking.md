# Networking & Sync

Lattice nodes use a fluid topology network stack, built on `iroh-net`. Nodes do not require static IP addresses, dedicated hosting, or standard routing ports. 

They establish connections using the QUIC protocol. With QUIC and Iroh, nodes aggressively punch holes through NATs, routing traffic directly P2P from laptops, to phones, to cloud servers.

## Sync Mechanisms

Lattice employs a tiered sync mechanism to balance bandwidth efficiency with immediate responsiveness.

### 1. Negentropy (O(1) Set Reconciliation)
When a device connects to a known peer after being offline for weeks, exchanging traditional vector clocks can be highly inefficient if the intention IDs drift out of alignment. 

Lattice sync relies heavily on **Negentropy**, a range-based set reconciliation protocol. 
It operates by computing the `XOR` of subsets of cryptographic hashes representing the node's intention state. By iteratively dividing the range of operations, it quickly zeroes in on exactly which operations a node is missing, allowing it to efficiently fetch just those bytes.

### 2. Gossip and Subscriptions
Synchronizing entire databases requires bandwidth. However, sometimes users just need immediate pings when changes happen (like receiving a chat message).

The **`GossipManager`** allows nodes to subscribe to specific topics. When a node commits an Intention, it broadcasts a lightweight ping out to connected peers via Gossip. Those peers can immediately trigger a direct Negentropy pull sync in the background to seamlessly integrate the new data.

## Bootstrap Protocol

When a node joins a store for the very first time using an invite token:
1.  **Handshake:** The new node (Joiner) connects to the Inviter and presents the token.
2.  **Initial Clone:** Because Negentropy thrives on small diffs, the Inviter initially streams its entire linear **Witness Log** to the Joiner to quickly bootstrap the database.
3.  **Active Sync:** Once the log finishes, the Node transitions into `Active` state and begins the normal Negentropy synchronization process with all online peers it discovers in the Root Store's `meta.db`. 

Because tokens provide access to Root Stores, the `RecursiveWatcher` instantly initializes background bootstrap processes for any discovered Sub-stores, walking down the hierarchy recursively.
