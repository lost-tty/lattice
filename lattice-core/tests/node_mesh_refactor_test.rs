use lattice_core::{NodeBuilder, PeerStatus, PubKey, Invite};
use std::sync::Arc;

#[tokio::test]
async fn test_node_to_mesh_delegation() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let node = Arc::new(NodeBuilder::new().with_data_dir(temp_dir.path()).build()?);
    let peer_pubkey = PubKey::from([2u8; 32]);

    // 1. Before init, no meshes should exist
    assert!(node.list_mesh_ids().is_empty());

    // 2. Init creates a mesh
    let mesh_id = node.create_mesh().await?;
    let mesh = node.mesh_by_id(mesh_id).expect("mesh should exist");

    // 3. Create invite token
    let token_string = mesh.create_invite(node.node_id()).await?;
    let invite = Invite::parse(&token_string)?;

    // 4. Accept Join via Node Facade with secret from token
    let acceptance = node.accept_join(peer_pubkey, mesh_id, &invite.secret).await?;
    assert_eq!(acceptance.store_id, mesh_id);

    // Verify status is Active
    let peers = mesh.list_peers().await?;
    let active = peers.iter().find(|p| p.pubkey == peer_pubkey).expect("peer not found");
    assert_eq!(active.status, PeerStatus::Active);

    // 5. Revoke Peer via Mesh
    mesh.revoke_peer(peer_pubkey).await?;
    let peers = mesh.list_peers().await?;
    let revoked = peers.iter().find(|p| p.pubkey == peer_pubkey).expect("peer not found");
    assert_eq!(revoked.status, PeerStatus::Revoked);

    Ok(())
}
