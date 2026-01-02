use lattice_core::{NodeBuilder, PeerStatus, PubKey};
use std::sync::Arc;

#[tokio::test]
async fn test_node_to_mesh_delegation() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let node = Arc::new(NodeBuilder::new().with_data_dir(temp_dir.path()).build()?);
    let peer_pubkey = PubKey::from([2u8; 32]);

    // 1. Before init, get_mesh should fail
    assert!(node.get_mesh().is_err());

    // 2. Init
    let _ = node.init().await?;
    let mesh = node.get_mesh()?;

    // 3. Invite Peer via Mesh
    mesh.invite_peer(peer_pubkey).await?;

    // 4. List Peers via Mesh
    let peers = mesh.list_peers().await?;
    let invited = peers.iter().find(|p| p.pubkey == peer_pubkey).expect("peer not found");
    assert_eq!(invited.status, PeerStatus::Invited);

    // 5. Accept Join via Node Facade
    // Note: Node::accept_join requires the correct mesh_id
    let mesh_id = node.root_store_id()?.unwrap();
    let acceptance = node.accept_join(peer_pubkey, mesh_id).await?;
    assert_eq!(acceptance.store_id, mesh_id);

    // Verify status is Active
    let peers = mesh.list_peers().await?;
    let active = peers.iter().find(|p| p.pubkey == peer_pubkey).expect("peer not found");
    assert_eq!(active.status, PeerStatus::Active);

    // 6. Revoke Peer via Mesh
    mesh.revoke_peer(peer_pubkey).await?;
    let peers = mesh.list_peers().await?;
    let revoked = peers.iter().find(|p| p.pubkey == peer_pubkey).expect("peer not found");
    assert_eq!(revoked.status, PeerStatus::Revoked);

    Ok(())
}
