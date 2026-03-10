---
title: "Multi-App Device Model"
weight: 8
---

> **Status**: Proposal

## Problem

When multiple applications embed Lattice on the same physical device, each gets an independent node identity. From the mesh's perspective, a single device with five apps looks like five unrelated peers. This causes two problems:

1. **Data safety misjudgment.** The mesh cannot distinguish "data replicated across 3 devices" from "data replicated across 3 apps on one device." Three peers on one phone is one failure domain, not three.
2. **Peer list noise.** Users see N entries per device instead of one, making the peer list harder to reason about.

## Design

### Device Identity Root

One application per device acts as the **identity authority** (the "Lattice app"). It generates and holds:

- A **device root key** (`K_device`) — an Ed25519 signing key.
- A **device ID** — a stable random identifier for the physical device.

Other applications do not generate their own identity. They request a derived identity from the authority app.

### Derived App Keys

When an application needs an identity, it requests one from the authority app via a platform-specific mechanism (URL scheme, intent, local service). The authority derives a deterministic child key:

```
K_app = KDF(K_device_secret, "lattice/app/" || app_identifier)
```

Properties:

- **Deterministic.** The same app identifier always produces the same key from the same device root. Reinstalling the app and re-deriving yields the same identity — no orphaned peers.
- **Independent.** Each app gets a distinct signing key. Compromise of one app key does not compromise the root or other apps.

### Device Certificate

The symmetric KDF means there is no public mathematical relationship between `K_device_public` and `K_app_public`. To make device grouping verifiable, the authority app signs a certificate during provisioning:

```
cert = Sign(K_device_secret, K_app_public || app_identifier)
```

The child app includes this certificate in its peer info. Any peer on the mesh can verify it against `K_device_public` — proving the child key was issued by that device without knowing any secret keys.

### Peer Info

Every peer announcement carries the device certificate:

```
PeerInfo {
    pubkey: PubKey,
    status: PeerStatus,
    name: Option<String>,
    device_pubkey: Option<PubKey>,
    device_cert: Option<Signature>,
    ...
}
```

Peers verify `device_cert` against `device_pubkey` to confirm co-location. The mesh groups verified peers by `device_pubkey` and counts **physical failure domains** rather than peer count.

### Shared Root Store, Partitioned Children

All apps on all devices join a single **root store**. This is where the peer list, device IDs, and permissions live — the complete picture of the mesh.

Each app creates **child stores** under that root for its own data. Apps only open and sync the children relevant to them. The `RecursiveWatcher` already handles child store discovery; it needs a subscription filter so each app only opens its own children.

```
Root Store (all apps, all devices)
├── Child: "notes"    (Notes app subscribes)
├── Child: "tasks"    (Tasks app subscribes)
├── Child: "photos"   (Photos app subscribes)
└── Child: "settings" (Authority app subscribes)
```

The root store's peer list is authoritative. Child stores inherit peers from the root via `PeerStrategy::Inherited`, so authorization flows down naturally.

### Identity Provisioning Flow

1. App generates an ephemeral X25519 keypair (`eph_pk`, `eph_sk`).
2. App opens a request to the authority app (e.g., URL: `lattice://derive?app=com.example.notes&eph=<eph_pk>&callback=...`).
3. Authority app generates its own ephemeral X25519 keypair (`auth_pk`, `auth_sk`).
4. Authority app computes `shared = X25519(auth_sk, eph_pk)`.
5. Authority app derives `K_app`, signs the device certificate: `cert = Sign(K_device_secret, K_app_public || app_identifier)`.
6. Authority app encrypts the payload: `encrypted = ChaCha20Poly1305(shared, K_app || K_device_public || cert)`.
7. Authority app returns `auth_pk` and `encrypted` via callback URL.
8. App computes `shared = X25519(eph_sk, auth_pk)`, decrypts to obtain `K_app`, `K_device_public`, and `cert`.
8. App stores the key locally and uses it as its `NodeIdentity`.

This is a one-time setup per app installation. The derived key is stored in the app's own sandbox and never needs to be re-derived unless the app is reinstalled.

### App Extensions

Platform-specific app extensions (widgets, share extensions, notification handlers) share storage with their parent app via platform mechanisms (e.g., App Group containers). The parent app runs the full Lattice runtime. Extensions either:

- **Read** store state in read-only mode (for display).
- **Queue** writes for the parent app to process when it next runs.
- **Briefly start** a minimal runtime for time-limited sync (e.g., on push notification), knowing the parent app is suspended and won't contend for database locks.

## Consequences

### Positive

- **Accurate failure domain counting.** The mesh knows how many physical devices hold a copy of any given store's data and can make informed replication decisions.
- **Clean peer lists.** Users see one device entry (possibly with sub-entries per app) instead of N unrelated peers.
- **No cross-app filesystem sharing required.** Each app has its own sandbox, its own `DataDir`, its own Redb databases. Sync between apps on the same device uses the normal Lattice peer-to-peer protocol — no special case.
- **Reinstall-safe identity.** Derived keys are deterministic, so reinstalling an app doesn't create a new peer.
- **Works cross-platform.** The model is platform-agnostic. The provisioning mechanism differs (URL schemes, intents, local sockets), but the key derivation and device grouping logic is universal.

### Negative / Challenges

- **Authority app dependency.** The authority app must be installed before any third-party app can operate. Apps that have not completed the identity handshake prompt the user to install it.
- **Key transit during provisioning.** The identity handshake uses an ephemeral X25519 key exchange. The requesting app sends an ephemeral public key in its request; the authority app performs ECDH to derive a shared secret and returns the derived signing key encrypted under that secret. The plaintext key never appears in any URL or IPC payload.
- **Device loss.** If a device is lost or wiped, all derived identities on that device are gone. The user's other devices revoke the lost device's keys from the mesh and the new device provisions fresh identities through the normal flow.


