//! StoreService gRPC implementation

use crate::backend::Backend;
use crate::proto::{
    store_service_server::StoreService,
    CreateStoreRequest, DeleteStoreRequest, DebugInfo, Empty, SetStoreNameRequest,
    WitnessLogRequest, WitnessLogResponse, StoreId, StoreRef, StoreMeta, StoreList, StoreDetails,
    SystemListResponse, SystemEntry, StoreNameResponse, PeerStrategyResponse,
    FloatingIntentionsResponse, GetIntentionRequest, GetIntentionResponse,
};
use tonic::{Request, Response, Status};
use uuid::Uuid;

pub struct StoreServiceImpl {
    backend: Backend,
}

impl StoreServiceImpl {
    pub fn new(backend: Backend) -> Self {
        Self { backend }
    }
    
    fn parse_uuid(bytes: &[u8]) -> Result<Uuid, Status> {
        Uuid::from_slice(bytes).map_err(|_| Status::invalid_argument("Invalid UUID"))
    }
}

#[tonic::async_trait]
impl StoreService for StoreServiceImpl {
    async fn create(&self, request: Request<CreateStoreRequest>) -> Result<Response<StoreRef>, Status> {
        let req = request.into_inner();
        let parent_id = if let Some(pid) = req.parent_id {
            Some(Self::parse_uuid(&pid)?)
        } else {
            None
        };
        let name = if req.name.is_empty() { None } else { Some(req.name) };
        self.backend.store_create(parent_id, name, &req.store_type).await
            .map(Response::new)
            .map_err(|e| Status::internal(e.to_string()))
    }

    async fn list(&self, request: Request<crate::proto::ListStoreRequest>) -> Result<Response<StoreList>, Status> {
        let req = request.into_inner();
        let parent_id = if let Some(pid) = req.parent_id {
            Some(Self::parse_uuid(&pid)?)
        } else {
            None
        };
        
        self.backend.store_list(parent_id).await
            .map(|stores| Response::new(StoreList { stores }))
            .map_err(|e| Status::internal(e.to_string()))
    }

    async fn get_status(&self, request: Request<StoreId>) -> Result<Response<StoreMeta>, Status> {
        let store_id = Self::parse_uuid(&request.into_inner().id)?;
        self.backend.store_status(store_id).await
            .map(Response::new)
            .map_err(|e| Status::internal(e.to_string()))
    }

    async fn get_details(&self, request: Request<StoreId>) -> Result<Response<StoreDetails>, Status> {
        let store_id = Self::parse_uuid(&request.into_inner().id)?;
        self.backend.store_details(store_id).await
            .map(Response::new)
            .map_err(|e| Status::internal(e.to_string()))
    }

    async fn delete(&self, request: Request<DeleteStoreRequest>) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        let store_id = Self::parse_uuid(&req.store_id)?;
        let child_id = Self::parse_uuid(&req.child_id)?;
        self.backend.store_delete(store_id, child_id).await
            .map(|_| Response::new(Empty {}))
            .map_err(|e| Status::internal(e.to_string()))
    }

    async fn set_name(&self, request: Request<SetStoreNameRequest>) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        let store_id = Self::parse_uuid(&req.store_id)?;
        self.backend.store_set_name(store_id, &req.name).await
            .map(|_| Response::new(Empty {}))
            .map_err(|e| Status::internal(e.to_string()))
    }

    async fn get_name(&self, request: Request<StoreId>) -> Result<Response<StoreNameResponse>, Status> {
        let store_id = Self::parse_uuid(&request.into_inner().id)?;
        self.backend.store_get_name(store_id).await
            .map(|name| Response::new(StoreNameResponse { name }))
            .map_err(|e| Status::internal(e.to_string()))
    }

    async fn sync(&self, request: Request<StoreId>) -> Result<Response<Empty>, Status> {
        let store_id = Self::parse_uuid(&request.into_inner().id)?;
        self.backend.store_sync(store_id).await
            .map(|_| Response::new(Empty {}))
            .map_err(|e| Status::internal(e.to_string()))
    }

    async fn debug(&self, request: Request<StoreId>) -> Result<Response<DebugInfo>, Status> {
        let store_id = Self::parse_uuid(&request.into_inner().id)?;
        self.backend.store_debug(store_id).await
            .map(|authors| Response::new(DebugInfo { authors }))
            .map_err(|e| Status::internal(e.to_string()))
    }

    async fn witness_log(&self, request: Request<WitnessLogRequest>) -> Result<Response<WitnessLogResponse>, Status> {
        let req = request.into_inner();
        let store_id = Self::parse_uuid(&req.store_id)?;
        self.backend.store_witness_log(store_id).await
            .map(|entries| Response::new(WitnessLogResponse { entries }))
            .map_err(|e| Status::internal(e.to_string()))
    }

    async fn floating_intentions(&self, request: Request<StoreId>) -> Result<Response<FloatingIntentionsResponse>, Status> {
        let store_id = Self::parse_uuid(&request.into_inner().id)?;
        self.backend.store_floating(store_id).await
            .map(|intentions| Response::new(FloatingIntentionsResponse { intentions }))
            .map_err(|e| Status::internal(e.to_string()))
    }

    async fn get_intention(&self, request: Request<GetIntentionRequest>) -> Result<Response<GetIntentionResponse>, Status> {
        let req = request.into_inner();
        let store_id = Self::parse_uuid(&req.store_id)?;
        match self.backend.store_get_intention(store_id, &req.hash_prefix).await {
            Ok(mut results) if results.len() == 1 => {
                let detail = results.remove(0);
                Ok(Response::new(GetIntentionResponse {
                    intention: Some(detail.intention),
                    ops: detail.ops.into_iter().map(Into::into).collect(),
                }))
            }
            Ok(results) if results.is_empty() => Err(Status::not_found("intention not found")),
            Ok(results) => {
                let hashes: Vec<String> = results.iter()
                    .map(|d| hex::encode(&d.intention.hash))
                    .collect();
                Err(Status::failed_precondition(format!("ambiguous prefix, matches: {}", hashes.join(", "))))
            }
            Err(e) => Err(Status::internal(e.to_string())),
        }
    }

    async fn system_list(&self, request: Request<StoreId>) -> Result<Response<SystemListResponse>, Status> {
        let store_id = Self::parse_uuid(&request.into_inner().id)?;
        self.backend.store_system_list(store_id).await
            .map(|entries| Response::new(SystemListResponse {
                entries: entries.into_iter().map(|(key, value)| SystemEntry { key, value }).collect()
            }))
            .map_err(|e| Status::internal(e.to_string()))
    }

    async fn list_peers(&self, request: Request<StoreId>) -> Result<Response<crate::proto::PeerList>, Status> {
        let store_id = Self::parse_uuid(&request.into_inner().id)?;
        self.backend.store_peers(store_id).await
            .map(|peers| Response::new(crate::proto::PeerList { peers }))
            .map_err(|e| Status::internal(e.to_string()))
    }

    async fn revoke_peer(&self, request: Request<crate::proto::RevokePeerRequest>) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        let store_id = Self::parse_uuid(&req.store_id)?;
        self.backend.store_peer_revoke(store_id, &req.peer_key).await
             .map(|_| Response::new(Empty {}))
            .map_err(|e| Status::internal(e.to_string()))
    }

    async fn get_peer_strategy(&self, request: Request<StoreId>) -> Result<Response<PeerStrategyResponse>, Status> {
        let store_id = Self::parse_uuid(&request.into_inner().id)?;
        self.backend.store_peer_strategy(store_id).await
            .map(|strategy| Response::new(PeerStrategyResponse { strategy }))
            .map_err(|e| Status::internal(e.to_string()))
    }

    async fn join(&self, request: Request<crate::proto::JoinRequest>) -> Result<Response<crate::proto::JoinResponse>, Status> {
        self.backend.store_join(&request.into_inner().token).await
            .map(|store_id| Response::new(crate::proto::JoinResponse {
                store_id: store_id.as_bytes().to_vec(),
            }))
            .map_err(|e| Status::internal(e.to_string()))
    }

    async fn invite(&self, request: Request<StoreId>) -> Result<Response<crate::proto::InviteToken>, Status> {
        let store_id = Self::parse_uuid(&request.into_inner().id)?;
        self.backend.store_peer_invite(store_id).await
            .map(|token| Response::new(crate::proto::InviteToken { token }))
            .map_err(|e| Status::internal(e.to_string()))
    }
}
