# One-Time Join Tokens

## Overview

Currently, the system requires the inviter to know the guest's Public Key before the join (via `peer invite <pubkey>`). This feature reverses the flow: the inviter generates a token upfront, and the guest presents it to join.

**Benefits:**
- Simpler onboarding: share a token string instead of exchanging public keys
- One-time use: tokens are consumed on successful join (security)
- Auditable: track which peers joined via token

## Token Format

```
lattice:<node_pubkey_hex>:<secret_hex>
```

Example: `lattice:a1b2c3...def0:1234abcd...9876`

## Storage Model

Tokens stored in root store under `/invites/<hash_of_secret>`:
- Only the **hash** is stored, never the raw secret
- Value: `"valid"` (presence indicates unused token)
- Deleted on successful use (one-time)

Optional audit trail: `/nodes/<pubkey>/joined_via` → `"token"`

## Implementation Plan

### 1. Protobuf (proto/network.proto)

Add optional secret field to `JoinRequest`:

```protobuf
message JoinRequest {
  bytes node_pubkey = 1; 
  optional bytes invite_secret = 2; // NEW: The secret token
}
```

### 2. Node Logic (lattice-core/src/node.rs)

#### `Node::create_invite() -> Result<String, NodeError>`

1. Generate random 32-byte secret
2. Hash secret with blake3
3. Store in KV: `/invites/<hash>` → `"valid"`
4. Return formatted token string

```rust
pub async fn create_invite(&self) -> Result<String, NodeError> {
    let store = self.root_store()?;
    
    // Generate random secret
    use rand::RngCore;
    let mut secret = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut secret);
    
    // Store hash only
    let hash = blake3::hash(&secret);
    let key = format!("/invites/{}", hex::encode(hash.as_bytes()));
    store.put(key.as_bytes(), b"valid").await?;
    
    // Format token
    let token = format!("lattice:{}:{}", 
        hex::encode(self.node.public_key()), 
        hex::encode(secret)
    );
    
    Ok(token)
}
```

#### `Node::accept_join(pubkey, secret) -> Result<JoinAcceptance, NodeError>`

Modified to check **either** explicit peer list **or** valid token:

```rust
pub async fn accept_join(&self, pubkey: PubKey, secret: Option<&[u8]>) -> Result<JoinAcceptance, NodeError> {
    let store = self.root_store()?;
    
    // Check explicit invite (existing method)
    let is_explicitly_invited = PeerProvider::can_join(self, &pubkey);
    
    // Check token if not explicitly invited
    let mut valid_token = false;
    if !is_explicitly_invited {
        if let Some(secret_bytes) = secret {
            let hash = blake3::hash(secret_bytes);
            let key = format!("/invites/{}", hex::encode(hash.as_bytes()));
            
            if let Ok(heads) = store.get(key.as_bytes()).await {
                if heads.lww_head().is_some() {
                    valid_token = true;
                    // Consume token (one-time use)
                    store.delete(key.as_bytes()).await?; 
                }
            }
        }
    }

    if !is_explicitly_invited && !valid_token {
        return Err(NodeError::Store(StateError::Unauthorized(...)));
    }

    // Add peer and return acceptance
    self.set_peer_status(pubkey, PeerStatus::Active).await?;
    
    // Audit trail
    if valid_token {
        let note_key = format!("/nodes/{:x}/joined_via", pubkey);
        store.put(note_key.as_bytes(), b"token").await?;
    }

    Ok(JoinAcceptance { store_id })
}
```

### 3. Network Handler (lattice-net/src/mesh/server.rs)

Extract and pass secret to node logic:

```rust
async fn handle_join_request(..., req: JoinRequest, ...) {
    let secret_slice = req.invite_secret.as_deref();
    let acceptance = node.accept_join(*remote_pubkey, secret_slice).await?;
    // ... rest unchanged
}
```

### 4. CLI Commands

#### New: `node create-invite`

```rust
pub async fn cmd_create_invite(node: &Node, ...) -> CommandResult {
    match node.create_invite().await {
        Ok(token) => {
            writeln!(w, "Invite Token (one-time use):");
            writeln!(w, "{}", token);
        }
        Err(e) => wout!(writer, "Error: {}", e),
    }
    CommandResult::Ok
}
```

#### Updated: `node join <target>`

Parse token format vs plain node ID:

```rust
let (peer_id, secret) = if input.starts_with("lattice:") {
    // Parse: lattice:<pubkey>:<secret>
    let parts: Vec<&str> = input.split(':').collect();
    // ... validation ...
    (PubKey::from_hex(parts[1])?, Some(hex::decode(parts[2])?))
} else {
    (PubKey::from_hex(input)?, None)
};

node.join(peer_id, secret)?;
```

### 5. Mesh Engine (lattice-net/src/mesh/engine.rs)

Update `handle_join_request_event` to include secret in outbound request:

```rust
pub async fn handle_join_request_event(
    &self, 
    peer_id: iroh::PublicKey, 
    secret: Option<Vec<u8>>
) -> Result<(), NodeError> {
    // ...
    let req = PeerMessage {
        message: Some(peer_message::Message::JoinRequest(JoinRequest {
            node_pubkey: self.node.node_id().to_vec(),
            invite_secret: secret,
        })),
    };
    // ...
}
```

### 6. NodeEvent (lattice-core/src/node.rs)

Update event to carry optional secret:

```rust
pub enum NodeEvent {
    // ...
    JoinRequested(PubKey, Option<Vec<u8>>),
    // ...
}
```

## Flow Summary

1. **User A** runs `node create-invite`
   - Node A generates secret, stores hash, returns `lattice:<A_ID>:<SECRET>`

2. **User B** runs `node join lattice:<A_ID>:<SECRET>`
   - Node B parses token, extracts ID and secret
   - Emits `JoinRequested(ID, Some(Secret))`

3. **Node B → Node A**: `JoinRequest { pubkey: B, invite_secret: Secret }`

4. **Node A** receives request:
   - Doesn't know B yet
   - Hashes incoming secret, finds `/invites/<hash>` in store
   - Deletes invite key (consumed)
   - Adds B as `Active` peer

5. **Node A → Node B**: `JoinResponse`

6. **Node B** initializes store and starts syncing

## Security Considerations

- **Hash storage**: Only blake3 hash stored, not raw secret
- **One-time use**: Token deleted immediately after successful verification
- **No replay**: Consumed tokens cannot be reused
- **Audit trail**: Optional tracking of token-based joins
