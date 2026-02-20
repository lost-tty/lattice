# Daemon Operation

While Lattice is heavily optimized as an embeddable library via FFI, it is also highly capable as a headless daemon (`latticed`) for home servers, cloud relays, or desktop background services.

## The Daemon Pattern (`latticed`)

The `latticed` binary initializes the `Node`, the `NetworkService`, and the `RpcServer`. It strips out the REPL to ensure pure, headless stability.

```bash
# Start the daemon
latticed --data-dir ~/.local/share/lattice
```

This exposes a gRPC API over Unix Domain Sockets (UDS) by default, located at `/tmp/lattice.sock`.

## The CLI (`lattice`)

The `lattice` interactive CLI is explicitly designed as a *Thin Client*. When you run commands in the CLI, it does not open `state.db` directly (which would lock the database). Instead, it acts as an RPC client communicating with the `latticed` daemon.

```bash
lattice
lattice:no-mesh> store create
lattice:060e> put name "Alice"
lattice:060e> get name
Alice
```

If the daemon is not running, the CLI offers an `--embedded` flag to launch the `Node` and the REPL in the same process footprint.

## Manifest Configuration (System Table)

Lattice embraces the Local-First Manifesto. Therefore, its concept of "Service Management" mirrors Systemd but across a mesh.

Configuration properties for the daemon are written into the `System Table` inside `rootstores`. 
When the `StoreManager` detects configuration updates (like requesting the instantiation of an IRC Gateway), it automatically spawns the background service instance, adhering to the "Consent Firewall" principles.
