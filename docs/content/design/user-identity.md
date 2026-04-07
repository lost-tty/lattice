---
title: "User Identity"
status: design
weight: 5
---

> **Status**: Design

Lattice separates node identity from user identity. Nodes are transport infrastructure — they move intentions through the mesh. Users are the entities that authorize operations within stores. Both layers use Ed25519 keypairs, but they serve different purposes and are verified independently.

## Problem

Currently, node keys serve double duty as both transport identity and authorization identity. A node's public key is the `author` on intentions, the peer identity in the peer table, and the basis for store-level access control. This conflation has consequences:

1. **No multi-device users.** Each device is a separate peer. A user with three devices needs three invites and appears as three peers.
2. **No multi-user nodes.** A shared device can only write as one identity. There is no way for multiple users to operate through the same node.
3. **Authorization is coarse.** The peer table controls who can write to a store, but all operations by a given node are equally authorized. There is no way to restrict specific operations to specific users.

## Design

### Two Signature Layers

Every intention carries two signatures:

1. **Node signature** — the node's Ed25519 key signs the intention (as today). This establishes DAG integrity: the intention is structurally valid, `store_prev` chains correctly, the node vouches for having produced this entry. Verified at the DAG layer before witnessing.

2. **User signature** — a user's Ed25519 key signs the `ops` payload. This establishes authorization: a specific user produced these operations. Verified deterministically at apply time.

The `Intention` struct gains a `user` public key field. The `SignedIntention` (or a new wrapper) gains a `user_signature` field.

### Verification Split

**DAG layer (kernel):** Verifies the node signature. If valid, the intention is witnessed, enters the log, and propagates through the mesh. The kernel also verifies the user signature is cryptographically valid — the `user` pubkey did sign the `ops`. But the kernel does not interpret permissions.

**State machine layer:** Receives the op with the verified user pubkey. Decides whether that user is authorized to perform the operation. Authorization logic is store-specific — one store may be permissionless, another may have roles, another may restrict certain operations to certain users.

An intention with a valid node signature but an invalid or unauthorized user signature still exists in the DAG. It propagates. But every node's state machine deterministically rejects it at apply time, so the mesh converges on the same state.

### Node as Courier

A node does not need per-store authorization. It is transport. Any node can carry any user's operations. The flow:

1. User holds a private key (on device, in keychain, on hardware token).
2. Application asks the user to sign the `ops` payload.
3. Node wraps the signed ops in an intention, signs the intention with the node key, commits.
4. Other nodes replay: verify node signature (DAG integrity), verify user signature (cryptographic authenticity), apply to state machine (authorization).

### Genesis and Initial Permissions

The genesis intention grants permission to one or more initial users. The set of authorized users and their permissions lives in the store's state — managed by the state machine, not by the kernel.

Subsequent permission changes (adding users, changing roles, revoking access) are ordinary operations within the store, subject to the same user authorization rules.

### What the System Provides to State Machines

The minimal contract: the verified user pubkey that signed the ops. The state machine receives this alongside the ops payload and decides what to allow. The kernel guarantees the pubkey is authentic (signature was verified). Everything else — permission models, role hierarchies, access control lists — is application-level.

## Open Questions

- **Key distribution.** How does a user get their private key onto multiple devices? QR code, encrypted backup, manual transfer? Out of scope for the core protocol but affects UX.
- **Key rotation.** What happens when a user rotates their key? The old pubkey is in historical intentions. The store needs a mechanism to associate old and new keys with the same user.
- **Revocation propagation.** When a user's access is revoked, nodes that haven't seen the revocation yet will still accept and propagate their intentions. The state machine rejects them at apply time. Is this sufficient, or do we need kernel-level rejection to avoid unnecessary propagation?
- **Anonymous stores.** Some stores might not need user identity at all. Should user signature be optional (e.g. `user` is `PubKey::ZERO` and `user_signature` is empty)?
