//! StoreService gRPC — implemented directly on InProcessBackend

use crate::backend_inprocess::{parse_uuid, InProcessBackend};
use lattice_api::proto::{
    store_service_server::StoreService, BranchPath, CreateStoreRequest, DebugInfo,
    DeleteStoreRequest, Empty, FloatingIntentionsResponse, GetIntentionRequest,
    GetIntentionResponse, InspectBranchRequest, InspectBranchResponse, PeerStrategyResponse,
    SetStoreNameRequest, StoreDetails, StoreId, StoreList, StoreMeta, StoreNameResponse, StoreRef,
    SystemEntry, SystemListResponse, WitnessLogRequest, WitnessLogResponse,
};
use lattice_api::IntoStatus;
use tonic::{Request, Response, Status};

#[tonic::async_trait]
impl StoreService for InProcessBackend {
    async fn create(&self, request: Request<CreateStoreRequest>) -> Result<Response<StoreRef>, Status> {
        let req = request.into_inner();
        let parent_id = req.parent_id.as_ref().map(|pid| parse_uuid(pid)).transpose()?;
        let name = if req.name.is_empty() { None } else { Some(req.name) };
        self.store_create(parent_id, name, &req.store_type).await.map(Response::new).into_status()
    }

    async fn list(&self, request: Request<lattice_api::proto::ListStoreRequest>) -> Result<Response<StoreList>, Status> {
        let req = request.into_inner();
        let parent_id = req.parent_id.as_ref().map(|pid| parse_uuid(pid)).transpose()?;
        self.store_list(parent_id).await.map(|s| Response::new(StoreList { items: s })).into_status()
    }

    async fn get_status(&self, request: Request<StoreId>) -> Result<Response<StoreMeta>, Status> {
        let id = parse_uuid(&request.into_inner().id)?;
        self.store_status(id).await.map(Response::new).into_status()
    }

    async fn get_details(&self, request: Request<StoreId>) -> Result<Response<StoreDetails>, Status> {
        let id = parse_uuid(&request.into_inner().id)?;
        self.store_details(id).await.map(Response::new).into_status()
    }

    async fn delete(&self, request: Request<DeleteStoreRequest>) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        let store_id = parse_uuid(&req.store_id)?;
        let child_id = parse_uuid(&req.child_id)?;
        self.store_delete(store_id, child_id).await.map(|_| Response::new(Empty {})).into_status()
    }

    async fn rebuild(&self, request: Request<StoreId>) -> Result<Response<Empty>, Status> {
        let id = parse_uuid(&request.into_inner().id)?;
        self.store_rebuild(id).await.map(|_| Response::new(Empty {})).into_status()
    }

    async fn set_name(&self, request: Request<SetStoreNameRequest>) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        let id = parse_uuid(&req.store_id)?;
        self.store_set_name(id, &req.name).await.map(|_| Response::new(Empty {})).into_status()
    }

    async fn get_name(&self, request: Request<StoreId>) -> Result<Response<StoreNameResponse>, Status> {
        let id = parse_uuid(&request.into_inner().id)?;
        self.store_get_name(id).await.map(|name| Response::new(StoreNameResponse { name })).into_status()
    }

    async fn sync(&self, request: Request<StoreId>) -> Result<Response<Empty>, Status> {
        let id = parse_uuid(&request.into_inner().id)?;
        self.store_sync(id).await.map(|_| Response::new(Empty {})).into_status()
    }

    async fn debug(&self, request: Request<StoreId>) -> Result<Response<DebugInfo>, Status> {
        let id = parse_uuid(&request.into_inner().id)?;
        self.store_debug(id).await.map(|a| Response::new(DebugInfo { items: a })).into_status()
    }

    async fn witness_log(&self, request: Request<WitnessLogRequest>) -> Result<Response<WitnessLogResponse>, Status> {
        let id = parse_uuid(&request.into_inner().store_id)?;
        self.store_witness_log(id).await
            .map(|e| Response::new(WitnessLogResponse { items: e.into_iter().map(Into::into).collect() }))
            .into_status()
    }

    async fn floating_intentions(&self, request: Request<StoreId>) -> Result<Response<FloatingIntentionsResponse>, Status> {
        let id = parse_uuid(&request.into_inner().id)?;
        self.store_floating(id).await
            .map(|i| Response::new(FloatingIntentionsResponse { items: i.into_iter().map(Into::into).collect() }))
            .into_status()
    }

    async fn get_intention(&self, request: Request<GetIntentionRequest>) -> Result<Response<GetIntentionResponse>, Status> {
        let req = request.into_inner();
        let id = parse_uuid(&req.store_id)?;
        match self.store_get_intention(id, &req.hash_prefix).await {
            Ok(mut results) if results.len() == 1 => {
                let d = results.remove(0);
                Ok(Response::new(GetIntentionResponse {
                    intention: Some(d.intention), ops: d.ops.into_iter().map(Into::into).collect(),
                }))
            }
            Ok(results) if results.is_empty() => Err(Status::not_found("intention not found")),
            Ok(results) => {
                let hashes: Vec<String> = results.iter().map(|d| hex::encode(&d.intention.hash)).collect();
                Err(Status::failed_precondition(format!("ambiguous prefix, matches: {}", hashes.join(", "))))
            }
            Err(e) => Err(Status::internal(e.to_string())),
        }
    }

    async fn system_list(&self, request: Request<StoreId>) -> Result<Response<SystemListResponse>, Status> {
        let id = parse_uuid(&request.into_inner().id)?;
        self.store_system_list(id).await
            .map(|e| Response::new(SystemListResponse {
                items: e.into_iter().map(|(key, value)| SystemEntry { key, value }).collect(),
            })).into_status()
    }

    async fn list_peers(&self, request: Request<StoreId>) -> Result<Response<lattice_api::proto::PeerList>, Status> {
        let id = parse_uuid(&request.into_inner().id)?;
        self.store_peers(id).await.map(|p| Response::new(lattice_api::proto::PeerList { items: p })).into_status()
    }

    async fn revoke_peer(&self, request: Request<lattice_api::proto::RevokePeerRequest>) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        let id = parse_uuid(&req.store_id)?;
        self.store_peer_revoke(id, &req.peer_key).await.map(|_| Response::new(Empty {})).into_status()
    }

    async fn get_peer_strategy(&self, request: Request<StoreId>) -> Result<Response<PeerStrategyResponse>, Status> {
        let id = parse_uuid(&request.into_inner().id)?;
        self.store_peer_strategy(id).await.map(|s| Response::new(PeerStrategyResponse { strategy: s })).into_status()
    }

    async fn join(&self, request: Request<lattice_api::proto::JoinRequest>) -> Result<Response<lattice_api::proto::JoinResponse>, Status> {
        self.store_join(&request.into_inner().token).await
            .map(|id| Response::new(lattice_api::proto::JoinResponse { store_id: id.as_bytes().to_vec() }))
            .into_status()
    }

    async fn invite(&self, request: Request<StoreId>) -> Result<Response<lattice_api::proto::InviteToken>, Status> {
        let id = parse_uuid(&request.into_inner().id)?;
        self.store_peer_invite(id).await.map(|t| Response::new(lattice_api::proto::InviteToken { token: t })).into_status()
    }

    async fn inspect_branch(&self, request: Request<InspectBranchRequest>) -> Result<Response<InspectBranchResponse>, Status> {
        let req = request.into_inner();
        let id = parse_uuid(&req.store_id)?;
        self.store_inspect_branch(id, req.heads).await.map(|insp| {
            Response::new(InspectBranchResponse {
                lca: insp.lca.as_bytes().to_vec(),
                branches: insp.branches.into_iter().map(|bp| BranchPath {
                    head: bp.head.as_bytes().to_vec(),
                    hashes: bp.hashes.into_iter().map(|h| h.as_bytes().to_vec()).collect(),
                }).collect(),
            })
        }).into_status()
    }
}

