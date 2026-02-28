---
title: "Epoch Key Encryption"
---

> **Status**: Future

A shared symmetric key for **O(1) gossip payload encryption**, enabling efficient peer revocation.

## Motivation

Currently, gossip payloads are plaintext protobuf bytes broadcast to all peers, relying entirely on QUIC's transport-layer TLS 1.3. This means:

- **No application-layer confidentiality.** Relay nodes or compromised transport can read intention payloads.
- **Revocation has no cryptographic teeth.** Removing a peer from the SystemTable ACL stops new connections, but any node that previously had access has seen all plaintext. There is no forward secrecy.

Epoch Key Encryption introduces a shared symmetric key per store that makes payload encryption O(1) per message regardless of peer count, and provides cryptographic enforcement of peer revocation.

## Design

### EpochKey Generation

On store creation, the creator generates a **random 32-byte EpochKey**. This key is the shared secret for all payload encryption within the store until the next epoch rotation.

### Payload Encryption

All Intentions encrypt their `ops` payload using the shared EpochKey with an AEAD cipher (AES-GCM or ChaCha20-Poly1305). The cryptographic overhead is ~28 bytes per message (12-byte nonce + 16-byte auth tag), regardless of whether there are 10 or 10,000 peers.

The intention metadata (`author`, `timestamp`, `store_id`, `store_prev`, `condition`) and the Ed25519 signature remain unencrypted — only the application payload is wrapped. This preserves the ability to verify DAG structure and signatures without the epoch key.

### Epoch Rotation (Revocation Only)

Epoch rotation is triggered **only on peer revocation**. Adding a peer does not rotate the epoch.

On revocation:

1. The revoking peer generates a new random 32-byte EpochKey.
2. An **EpochChange intention** is created containing the new key encrypted individually for each remaining authorized peer (O(N) envelope).
3. Per-peer encryption uses **X25519 DH** key agreement derived from existing Ed25519 keys (via `to_montgomery()`).
4. The revoked peer never receives the new key — their copy is simply not included.

### Peer Addition

Adding a peer does **not** rotate the epoch. The new peer receives the current EpochKey encrypted to their public key as part of the join handshake. They then bootstrap/sync normally under the existing epoch.

This avoids disrupting existing peers and eliminates any transition window on peer addition.

## Causal Dependency & Auth

Since system ops and data ops share the same intention DAG (routed to different state machine tables by payload type), EpochChange intentions participate in normal causal ordering.

Each author's **first** data intention after an EpochChange includes its hash in `causal_deps`. Subsequent intentions by that author transitively depend on it via `store_prev`, so no repeated explicit reference is needed.

This serves double duty:

1. **DAG-embedded capability auth.** The causal dep proves the author saw the revocation before writing. A revoked peer cannot forge a dependency on an EpochChange it never received — the hash is only knowable if you have the intention.
2. **Clean epoch transition.** Peers must acknowledge the EpochChange before writing under the new epoch, eliminating the window where stragglers write with the old key after rotation.

## Design Considerations

**Nonce partitioning.** Multiple peers encrypt independently with the same key. To avoid nonce collisions, partition the nonce space: e.g. `author_pubkey_prefix[0..4] || random[0..8]`.

**Periodic rotation.** Optional epoch rotation on a timer (independent of membership changes) provides forward secrecy at epoch granularity — compromise of the current key does not reveal past epochs, assuming old keys are deleted.

**Bootstrap.** New peers receive the current epoch key during join. They do not need the key history — materialized state from snapshot-based bootstrap covers pre-epoch data. Only the DAG tail (post-snapshot) needs decryption, and it's all under the current epoch.

**Old-epoch intentions after revocation.** During the brief window between EpochChange broadcast and all peers acknowledging it, some peers may still send intentions encrypted with the old key. The causal dep requirement bounds this: once a peer's first post-epoch intention lands, all its subsequent writes are under the new key. Intentions encrypted with the old key are still valid — they're ordered before the transition in the DAG.

## Cost Summary

| Operation | Cost | When |
|---|---|---|
| Normal intention encryption | O(1) — ~28 bytes overhead | Every write |
| Epoch rotation (revocation) | O(N) — one encrypted key copy per remaining peer | Peer removal |
| Peer addition | O(1) — encrypt current key for new peer | Peer join |
| Signature verification | Unchanged — Ed25519 on unencrypted metadata | Every ingestion |
