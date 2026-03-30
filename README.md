# Lattice

Lattice is a decentralized syncing engine. Data lives on disk locally, syncs between peers over QUIC ([Iroh](https://iroh.computer)), and works offline by default. Each write produces a signed, append-only log entry. No central server required.

> **Research software.** The protocol and on-disk format are not stable. Expect breaking changes.

## Building

Rust 1.75+, Protobuf compiler (`protoc`).

```bash
cargo build --release
```

## Quick Start

```bash
# Start the daemon (web UI at http://localhost:8123)
lattice --daemon

# Use the CLI in another terminal
lattice
lattice> store create --root
lattice:a1b2> put hello world
lattice:a1b2> get hello
world
```

Or standalone with an embedded node:

```bash
lattice --embedded
lattice --embedded --web 8123   # with web UI
```

## Web Interface

Lattice includes a browser-based UI served directly from the node. No external dependencies, no build step — the SPA is bundled into the binary. In daemon mode, the web UI is enabled by default on port 8123.

```bash
lattice --daemon                # web UI on http://localhost:8123
lattice --daemon --web 9000     # custom port
lattice --daemon --no-web       # disable web UI
```

The web UI connects to the node over WebSocket and provides a dashboard, app management, store browsing, peer operations, method execution, live subscriptions, and a DAG history graph.

Requires the `web` feature (enabled by default):

```bash
cargo build --release --features web
```

## Connecting Nodes

```bash
# Node A: generate invite
lattice:a1b2> peer invite

# Node B: join
lattice> store join <token>
```

## Documentation

Run the docs site locally:

```bash
cd docs && hugo server
```

## Community

Join the Matrix room: [#latticesync:matrix.org](https://matrix.to/#/%23latticesync:matrix.org)

## License

MPL-2.0
