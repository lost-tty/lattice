//! MeshService gRPC implementation

use crate::proto::{
    mesh_service_server::MeshService, Empty, InviteToken, JoinRequest, JoinResponse, MeshId,
    MeshInfo, MeshList, PeerInfo, PeerList, RevokeRequest,
};
use lattice_model::types::PubKey;
use lattice_net::MeshService as NetMeshService;
use lattice_node::Node;
use std::sync::Arc;
use tonic::{Request, Response, Status};
use uuid::Uuid;

pub struct MeshServiceImpl {
    node: Arc<Node>,
    mesh_network: Option<Arc<NetMeshService>>,
}

impl MeshServiceImpl {
    pub fn new(node: Arc<Node>, mesh_network: Option<Arc<NetMeshService>>) -> Self {
        Self { node, mesh_network }
    }

    fn get_mesh(&self, mesh_id: &[u8]) -> Result<lattice_node::Mesh, Status> {
        let uuid = Uuid::from_slice(mesh_id).map_err(|_| Status::invalid_argument("Invalid mesh ID"))?;
        self.node
            .mesh_by_id(uuid)
            .ok_or_else(|| Status::not_found("Mesh not found"))
    }
}

#[tonic::async_trait]
impl MeshService for MeshServiceImpl {
    async fn create(&self, _request: Request<Empty>) -> Result<Response<MeshInfo>, Status> {
        let mesh_id = self
            .node
            .create_mesh()
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        let mesh = self.node.mesh_by_id(mesh_id).ok_or_else(|| Status::internal("Mesh not found after creation"))?;
        let peer_count = mesh.list_peers().await.map(|p| p.len() as u32).unwrap_or(0);
        let store_count = mesh.list_stores().map(|s| s.len() as u32 + 1).unwrap_or(1); // +1 for root store

        Ok(Response::new(MeshInfo {
            id: mesh_id.as_bytes().to_vec(),
            alias: String::new(),
            peer_count,
            store_count,
        }))
    }

    async fn list(&self, _request: Request<Empty>) -> Result<Response<MeshList>, Status> {
        let meshes = self
            .node
            .meta()
            .list_meshes()
            .map_err(|e| Status::internal(e.to_string()))?;

        let mut mesh_infos = Vec::new();
        for (id, _info) in meshes {
            let (peer_count, store_count) = if let Some(m) = self.node.mesh_by_id(id) {
                let peers = m.list_peers().await.map(|p| p.len() as u32).unwrap_or(0);
                let stores = m.list_stores().map(|s| s.len() as u32 + 1).unwrap_or(1); // +1 for root
                (peers, stores)
            } else {
                (0, 0)
            };

            mesh_infos.push(MeshInfo {
                id: id.as_bytes().to_vec(),
                alias: String::new(),
                peer_count,
                store_count,
            });
        }

        Ok(Response::new(MeshList { meshes: mesh_infos }))
    }

    async fn get_status(&self, request: Request<MeshId>) -> Result<Response<MeshInfo>, Status> {
        let mesh = self.get_mesh(&request.into_inner().id)?;
        let peer_count = mesh.list_peers().await.map(|p| p.len() as u32).unwrap_or(0);
        let store_count = mesh.list_stores().map(|s| s.len() as u32 + 1).unwrap_or(1); // +1 for root

        Ok(Response::new(MeshInfo {
            id: mesh.id().as_bytes().to_vec(),
            alias: String::new(),
            peer_count,
            store_count,
        }))
    }

    async fn join(&self, request: Request<JoinRequest>) -> Result<Response<JoinResponse>, Status> {
        let token = request.into_inner().token;
        let invite = lattice_node::token::Invite::parse(&token)
            .map_err(|e| Status::invalid_argument(format!("Invalid token: {}", e)))?;

        self.node
            .join(invite.inviter, invite.mesh_id, invite.secret)
            .map_err(|e| Status::internal(e.to_string()))?;

        Ok(Response::new(JoinResponse {
            mesh_id: invite.mesh_id.as_bytes().to_vec(),
            status: "Join request sent".to_string(),
        }))
    }

    async fn invite(&self, request: Request<MeshId>) -> Result<Response<InviteToken>, Status> {
        let mesh = self.get_mesh(&request.into_inner().id)?;
        let token = mesh
            .create_invite(self.node.node_id())
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        Ok(Response::new(InviteToken { token }))
    }

    async fn list_peers(&self, request: Request<MeshId>) -> Result<Response<PeerList>, Status> {
        let mesh = self.get_mesh(&request.into_inner().id)?;
        let peers = mesh
            .list_peers()
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        // Get online status and last_seen from network layer - same as backend_inprocess
        let online_peers: std::collections::HashMap<PubKey, std::time::Instant> = self.mesh_network
            .as_ref()
            .and_then(|m| m.connected_peers().ok())
            .unwrap_or_default();

        let my_pubkey = self.node.node_id();
        let peer_infos = peers
            .into_iter()
            .map(|p| {
                let is_self = p.pubkey == my_pubkey;
                let online = is_self || online_peers.contains_key(&p.pubkey);
                let last_seen_ms = online_peers.get(&p.pubkey)
                    .map(|i| i.elapsed().as_millis() as u64)
                    .unwrap_or(0);
                PeerInfo {
                    public_key: p.pubkey.to_vec(),
                    status: p.status.as_str().to_string(),
                    online,
                    name: p.name.unwrap_or_default(),
                    last_seen_ms,
                }
            })
            .collect();

        Ok(Response::new(PeerList { peers: peer_infos }))
    }

    async fn revoke(&self, request: Request<RevokeRequest>) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        let mesh = self.get_mesh(&req.mesh_id)?;
        let pubkey = PubKey::try_from(req.peer_key.as_slice())
            .map_err(|_| Status::invalid_argument("Invalid public key"))?;

        mesh.revoke_peer(pubkey)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        Ok(Response::new(Empty {}))
    }
}
