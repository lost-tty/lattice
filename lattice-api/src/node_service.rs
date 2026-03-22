//! NodeService gRPC implementation - thin wrapper around LatticeBackend

use crate::backend::Backend;
use crate::proto::node_service_server::NodeService;
use crate::proto::NodeEvent as NodeEventMessage;
use crate::proto::{
    AppBindingList, AppBindingProto, Empty, NodeStatus, SetNameRequest, StoreTypeList,
    ToggleAppRequest,
};
use crate::IntoStatus;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};
use uuid::Uuid;

pub struct NodeServiceImpl {
    backend: Backend,
}

impl NodeServiceImpl {
    pub fn new(backend: Backend) -> Self {
        Self { backend }
    }
}

#[tonic::async_trait]
impl NodeService for NodeServiceImpl {
    async fn get_status(&self, _request: Request<Empty>) -> Result<Response<NodeStatus>, Status> {
        self.backend
            .node_status()
            .await
            .map(Response::new)
            .into_status()
    }

    async fn list_store_types(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<StoreTypeList>, Status> {
        let types = self.backend.store_types().await.into_status()?;
        Ok(Response::new(StoreTypeList {
            store_types: types,
        }))
    }

    async fn set_name(&self, request: Request<SetNameRequest>) -> Result<Response<Empty>, Status> {
        self.backend
            .node_set_name(&request.into_inner().name)
            .await
            .map(|_| Response::new(Empty {}))
            .into_status()
    }

    type SubscribeStream = ReceiverStream<Result<NodeEventMessage, Status>>;

    async fn subscribe(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<Self::SubscribeStream>, Status> {
        let mut rx = self
            .backend
            .subscribe()
            .into_status()?;

        let (tx, stream_rx) = tokio::sync::mpsc::channel(32);

        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                let proto_msg = NodeEventMessage {
                    node_event: Some(event),
                };
                if tx.send(Ok(proto_msg)).await.is_err() {
                    break;
                }
            }
        });

        Ok(Response::new(ReceiverStream::new(stream_rx)))
    }

    // ---- App management ----

    async fn list_active_apps(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<AppBindingList>, Status> {
        self.backend
            .app_list()
            .await
            .map(|bindings| {
                Response::new(AppBindingList {
                    bindings: bindings.into_iter().map(Into::into).collect(),
                })
            })
            .into_status()
    }

    async fn toggle_app(
        &self,
        request: Request<ToggleAppRequest>,
    ) -> Result<Response<AppBindingProto>, Status> {
        let req = request.into_inner();
        let reg_id = Uuid::from_slice(&req.registry_store_id)
            .map_err(|_| Status::invalid_argument("Invalid registry_store_id UUID"))?;
        self.backend
            .app_toggle(reg_id, &req.subdomain, req.enabled)
            .await
            .map(|b| Response::new(b.into()))
            .into_status()
    }
}
