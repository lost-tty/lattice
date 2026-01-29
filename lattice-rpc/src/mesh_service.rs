//! MeshService gRPC implementation
//!
//! Thin wrapper - parses bytes to Uuid at the boundary, then delegates to backend.

use crate::backend::Backend;
use crate::proto::{
    mesh_service_server::MeshService, Empty, InviteToken, JoinRequest, JoinResponse, MeshId,
    MeshInfo, MeshList, PeerList, RevokeRequest,
};
use tonic::{Request, Response, Status};
use uuid::Uuid;

pub struct MeshServiceImpl {
    backend: Backend,
}

impl MeshServiceImpl {
    pub fn new(backend: Backend) -> Self {
        Self { backend }
    }
    
    fn parse_uuid(bytes: &[u8]) -> Result<Uuid, Status> {
        Uuid::from_slice(bytes).map_err(|_| Status::invalid_argument("Invalid UUID"))
    }
}

#[tonic::async_trait]
impl MeshService for MeshServiceImpl {
    async fn create(&self, _request: Request<Empty>) -> Result<Response<MeshInfo>, Status> {
        self.backend.mesh_create().await
            .map(Response::new)
            .map_err(|e| Status::internal(e.to_string()))
    }

    async fn list(&self, _request: Request<Empty>) -> Result<Response<MeshList>, Status> {
        self.backend.mesh_list().await
            .map(|meshes| Response::new(MeshList { meshes }))
            .map_err(|e| Status::internal(e.to_string()))
    }

    async fn get_status(&self, request: Request<MeshId>) -> Result<Response<MeshInfo>, Status> {
        let mesh_id = Self::parse_uuid(&request.into_inner().id)?;
        self.backend.mesh_status(mesh_id).await
            .map(Response::new)
            .map_err(|e| Status::internal(e.to_string()))
    }

    async fn join(&self, request: Request<JoinRequest>) -> Result<Response<JoinResponse>, Status> {
        self.backend.mesh_join(&request.into_inner().token).await
            .map(|mesh_id| Response::new(JoinResponse {
                mesh_id: mesh_id.as_bytes().to_vec(),
            }))
            .map_err(|e| Status::internal(e.to_string()))
    }

    async fn invite(&self, request: Request<MeshId>) -> Result<Response<InviteToken>, Status> {
        let mesh_id = Self::parse_uuid(&request.into_inner().id)?;
        self.backend.mesh_invite(mesh_id).await
            .map(|token| Response::new(InviteToken { token }))
            .map_err(|e| Status::internal(e.to_string()))
    }

    async fn list_peers(&self, request: Request<MeshId>) -> Result<Response<PeerList>, Status> {
        let mesh_id = Self::parse_uuid(&request.into_inner().id)?;
        self.backend.mesh_peers(mesh_id).await
            .map(|peers| Response::new(PeerList { peers }))
            .map_err(|e| Status::internal(e.to_string()))
    }

    async fn revoke(&self, request: Request<RevokeRequest>) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        let mesh_id = Self::parse_uuid(&req.mesh_id)?;
        self.backend.mesh_revoke(mesh_id, &req.peer_key).await
            .map(|_| Response::new(Empty {}))
            .map_err(|e| Status::internal(e.to_string()))
    }
}
