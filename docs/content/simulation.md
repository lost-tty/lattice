---
title: "Simulation Harness"
---

`lattice-net-sim` provides in-memory replacements for the production Iroh networking stack so that multi-node integration tests run entirely inside a single process, with no sockets, no QUIC, and deterministic message delivery.

## Components

### `ChannelNetwork`

Shared broker that routes connections between simulated nodes. Internally a `HashMap<PubKey, mpsc::Sender<ChannelConnection>>` — when node A calls `connect(&B)`, the broker looks up B's accept channel and delivers a `ChannelConnection` backed by a `tokio::io::DuplexStream`.

```rust
let network = ChannelNetwork::new();
let transport_a = ChannelTransport::new(pubkey_a, &network).await;
let transport_b = ChannelTransport::new(pubkey_b, &network).await;
```

### `ChannelTransport`

Implements the `Transport` trait. Each instance registers itself with a `ChannelNetwork` on creation. `connect()` creates a `DuplexStream` pair (64 KiB buffer) and hands one end to the peer via the broker; `accept()` blocks until an inbound connection arrives. Streams are split into `ReadHalf` / `WriteHalf` via `ChannelBiStream`, matching the QUIC bidirectional stream interface exactly.

### `GossipNetwork`

Shared broker for gossip, analogous to `ChannelNetwork` for streams. Maintains one `broadcast::channel<(PubKey, Vec<u8>)>` per store UUID. All subscribed nodes for a given store share the same channel, giving all-to-all delivery.

```rust
let gossip_net = GossipNetwork::new();
let gossip_a = BroadcastGossip::new(pubkey_a, &gossip_net);
let gossip_b = BroadcastGossip::new(pubkey_b, &gossip_net);
```

### `BroadcastGossip`

Implements the `GossipLayer` trait. On `subscribe(store_id)`, it taps into the shared `GossipNetwork` channel for that store and spawns a receive task that filters out the node's own messages. On `broadcast(store_id, data)`, it sends `(my_pubkey, data)` into the shared channel.

For testing gap recovery, `drop_next_incoming_message()` sets a flag that causes exactly one inbound gossip message to be silently dropped, triggering the `MissingDep` → `FetchChain` path.

`join_peers()` is a no-op — broadcast gossip is inherently all-to-all.

### `SimBackend`

Wires everything together into a `NetworkBackend<ChannelTransport>`, the same generic struct used by the production `IrohBackend`. Spawns an accept loop that dispatches incoming connections through `dispatch_stream`, mirroring iroh's Router. Used as:

```rust
let backend = SimBackend::new(transport, provider, Some(gossip));
let service = NetworkService::from_backend(backend, event_rx).await;
```

## Usage in Tests

Integration tests create a `ChannelNetwork` + `GossipNetwork` pair, then instantiate N nodes with `ChannelTransport` / `BroadcastGossip` / `SimBackend`. All sync, gossip, gap-fill, and bootstrap protocols exercise the same `NetworkService` code paths as production — only the transport and gossip layers are swapped.
