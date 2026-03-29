//! Genesis intention helpers.
//!
//! Every store has a genesis intention as its very first entry. For new stores,
//! the creating node submits it using its own identity. For existing stores
//! (migration), a deterministic synthetic genesis is constructed so that all
//! nodes derive the same hash.

use lattice_model::hlc::HLC;
use lattice_model::types::{Hash, PubKey};
use lattice_model::weaver::{Condition, Intention, SignedIntention};
use lattice_proto::storage::{universal_op, GenesisOp, UniversalOp};
use prost::Message;
use std::sync::Arc;
use uuid::Uuid;

/// Build the genesis payload bytes (a serialized `UniversalOp::Genesis`).
pub fn build_genesis_payload(store_type: &str, nonce: u32) -> Vec<u8> {
    let genesis_op = GenesisOp {
        store_type: store_type.to_string(),
        nonce,
    };
    let universal = UniversalOp {
        op: Some(universal_op::Op::Genesis(genesis_op)),
    };
    universal.encode_to_vec()
}

/// Build a deterministic synthetic genesis intention for an existing store.
///
/// All nodes must produce the identical intention so that the genesis hash
/// is consistent across the mesh. The keypair is derived deterministically
/// from the store UUID, the timestamp is fixed at zero, and the nonce is
/// derived from the UUID bytes.
///
/// MIGRATION: Only used by `Node::ensure_genesis` to backfill pre-genesis
/// stores. Delete together with that method once all dev stores are migrated.
pub fn build_synthetic_genesis(store_id: Uuid, store_type: &str) -> SignedIntention {
    let uuid_bytes = store_id.as_bytes();

    // Deterministic nonce from the first 4 bytes of the UUID.
    let nonce = u32::from_le_bytes(uuid_bytes[0..4].try_into().unwrap());

    // Deterministic Ed25519 keypair: seed = blake3("lattice-synthetic-genesis" || uuid).
    let seed = blake3::hash(&[b"lattice-synthetic-genesis".as_slice(), uuid_bytes].concat());
    let signing_key = ed25519_dalek::SigningKey::from_bytes(seed.as_bytes());

    let author = PubKey::from(signing_key.verifying_key().to_bytes());
    let payload = build_genesis_payload(store_type, nonce);

    let intention = Intention {
        author,
        timestamp: HLC {
            wall_time: 0,
            counter: 0,
        },
        store_id,
        store_prev: Hash::ZERO,
        condition: Condition::v1(vec![]),
        ops: payload,
    };

    SignedIntention::sign(intention, &signing_key)
}

/// Ensure a store has a genesis intention. If TABLE_META lacks a genesis
/// hash, ingest a deterministic synthetic genesis so that all nodes in the
/// mesh converge on the same root hash.
///
/// MIGRATION: This exists only to backfill genesis intentions into stores
/// created before GenesisOp was introduced. Once all development stores
/// have been migrated (or wiped), delete this function and
/// `build_synthetic_genesis`.
pub async fn ensure_genesis(
    store_id: Uuid,
    handle: &Arc<dyn crate::StoreHandle>,
    store_type: &str,
) {
    let inspector = handle.as_inspector();

    // Any actor command serializes after the startup projection that
    // runs before the actor's event loop.  witness_count() acts as a
    // fence: by the time it returns, the actor has finished replaying
    // the witness log onto state.db (including writing genesis_hash to
    // TABLE_META if a GenesisOp was already present).
    let count = inspector.witness_count().await;

    let meta = inspector.store_meta().await;
    if meta.genesis_hash.is_some() {
        return;
    }

    // Empty stores either just got created (genesis submitted via
    // StoreManager::create) or just got joined (genesis arrives via
    // sync).  Only inject a synthetic genesis into stores that have
    // existing intentions but no genesis -- i.e. pre-genesis stores.
    if count == 0 {
        return;
    }

    // No genesis -- synthesize and ingest a deterministic one.
    let signed = build_synthetic_genesis(store_id, store_type);
    let genesis_hash = signed.intention.hash();
    tracing::info!(
        store_id = %store_id,
        genesis_hash = %genesis_hash,
        "Migrating store: injecting synthetic genesis"
    );

    let sync = handle.as_sync_provider();
    match sync.ingest_intention(signed).await {
        Ok(_) => {
            tracing::debug!(store_id = %store_id, "Synthetic genesis ingested");
        }
        Err(e) => {
            tracing::warn!(
                store_id = %store_id,
                error = %e,
                "Failed to ingest synthetic genesis"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_genesis_is_deterministic() {
        let store_id = Uuid::from_bytes([0xAA; 16]);
        let a = build_synthetic_genesis(store_id, "core:kvstore");
        let b = build_synthetic_genesis(store_id, "core:kvstore");
        assert_eq!(a.intention.hash(), b.intention.hash());
        assert_eq!(a.signature, b.signature);
    }

    #[test]
    fn synthetic_genesis_signature_verifies() {
        let store_id = Uuid::new_v4();
        let signed = build_synthetic_genesis(store_id, "core:rootstore");
        signed
            .verify()
            .expect("synthetic genesis signature must verify");
    }

    #[test]
    fn different_stores_produce_different_hashes() {
        let a = build_synthetic_genesis(Uuid::from_bytes([1; 16]), "core:kvstore");
        let b = build_synthetic_genesis(Uuid::from_bytes([2; 16]), "core:kvstore");
        assert_ne!(a.intention.hash(), b.intention.hash());
    }

    #[test]
    fn genesis_payload_round_trips() {
        let payload = build_genesis_payload("core:rootstore", 42);
        let decoded = UniversalOp::decode(payload.as_slice()).unwrap();
        match decoded.op {
            Some(universal_op::Op::Genesis(g)) => {
                assert_eq!(g.store_type, "core:rootstore");
                assert_eq!(g.nonce, 42);
            }
            other => panic!("expected Genesis, got {:?}", other),
        }
    }
}
