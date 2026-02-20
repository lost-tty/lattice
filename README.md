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
# Start the daemon
latticed

# In another terminal
lattice
lattice> store create --root
lattice:a1b2> put hello world
lattice:a1b2> get hello
world
```

Or standalone without the daemon:

```bash
lattice --embedded
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

## License

AGPL-3.0
