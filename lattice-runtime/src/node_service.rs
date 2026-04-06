//! NodeService gRPC — implemented directly on InProcessBackend

use crate::backend_inprocess::InProcessBackend;
use lattice_api::proto::node_service_server::NodeService;
use lattice_api::proto::NodeEvent as NodeEventMessage;
use lattice_api::proto::{
    AppBindingList, AppBindingProto, Empty, NodeStatus, SetNameRequest, StoreTypeList,
    ToggleAppRequest,
};
use lattice_api::IntoStatus;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};
use uuid::Uuid;

#[tonic::async_trait]
impl NodeService for InProcessBackend {
    async fn get_status(&self, _request: Request<Empty>) -> Result<Response<NodeStatus>, Status> {
        self.node_status().await.map(Response::new).into_status()
    }

    async fn list_store_types(&self, _request: Request<Empty>) -> Result<Response<StoreTypeList>, Status> {
        self.store_types().await
            .map(|types| Response::new(StoreTypeList { items: types })).into_status()
    }

    async fn set_name(&self, request: Request<SetNameRequest>) -> Result<Response<Empty>, Status> {
        self.node_set_name(&request.into_inner().name).await
            .map(|_| Response::new(Empty {})).into_status()
    }

    type SubscribeStream = ReceiverStream<Result<NodeEventMessage, Status>>;

    async fn subscribe(&self, _request: Request<Empty>) -> Result<Response<Self::SubscribeStream>, Status> {
        let mut rx = InProcessBackend::subscribe(self).into_status()?;
        let (tx, stream_rx) = tokio::sync::mpsc::channel(32);
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                if tx.send(Ok(NodeEventMessage { node_event: Some(event) })).await.is_err() { break; }
            }
        });
        Ok(Response::new(ReceiverStream::new(stream_rx)))
    }

    async fn list_active_apps(&self, _request: Request<Empty>) -> Result<Response<AppBindingList>, Status> {
        self.app_list().await.map(|bindings| {
            Response::new(AppBindingList { items: bindings.into_iter().map(Into::into).collect() })
        }).into_status()
    }

    async fn toggle_app(&self, request: Request<ToggleAppRequest>) -> Result<Response<AppBindingProto>, Status> {
        let req = request.into_inner();
        let reg_id = Uuid::from_slice(&req.registry_store_id)
            .map_err(|_| Status::invalid_argument("Invalid UUID"))?;
        self.app_toggle(reg_id, &req.subdomain, req.enabled).await
            .map(|b| Response::new(b.into())).into_status()
    }
}
