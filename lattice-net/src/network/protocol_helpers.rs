//! Shared parsing and validation helpers for protocol message handling.
//!
//! These are used by both inbound handlers (handlers.rs) and outbound
//! protocol methods (service.rs) to avoid duplicating parsing logic.

use crate::LatticeNetError;
use lattice_model::{
    types::{Hash, PubKey},
    Uuid,
};
use lattice_net_types::{NetworkStore, NetworkStoreRegistry, NodeProviderExt};

/// Look up a store in the registry by ID.
pub fn lookup_store(
    registry: &dyn NetworkStoreRegistry,
    store_id: Uuid,
) -> Result<NetworkStore, LatticeNetError> {
    registry
        .get_network_store(&store_id)
        .ok_or(LatticeNetError::StoreNotRegistered(store_id))
}

/// Parse a UUID from bytes, look up the store, and verify the remote peer is authorized.
///
/// Used by all inbound handlers that operate on a specific store (reconcile, fetch,
/// bootstrap). Join requests are excluded — they use `accept_join` which handles
/// its own authorization.
pub fn get_authorized_store(
    provider: &dyn NodeProviderExt,
    store_id_bytes: &[u8],
    remote_pubkey: &PubKey,
) -> Result<(Uuid, NetworkStore), LatticeNetError> {
    let store_id =
        Uuid::from_slice(store_id_bytes).map_err(|_| LatticeNetError::InvalidField("store_id"))?;

    let store = lookup_store(provider.store_registry().as_ref(), store_id)?;

    if !store.can_connect(remote_pubkey) {
        return Err(LatticeNetError::PeerNotAuthorized {
            peer: *remote_pubkey,
            store_id,
        });
    }

    Ok((store_id, store))
}

/// Parse an optional hash from bytes. Empty or zero-hash bytes map to `None`.
pub fn parse_optional_hash(
    bytes: &[u8],
    field: &'static str,
) -> Result<Option<Hash>, LatticeNetError> {
    if bytes.is_empty() {
        return Ok(None);
    }
    let h = Hash::try_from(bytes).map_err(|_| LatticeNetError::InvalidField(field))?;
    Ok((h != Hash::ZERO).then_some(h))
}
