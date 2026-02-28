//! NodeService gRPC implementation - thin wrapper around LatticeBackend

use crate::backend::Backend;
use crate::proto::node_service_server::NodeService;
use crate::proto::NodeEvent as NodeEventMessage;
use crate::proto::{Empty, NodeStatus, SetNameRequest};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

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
            .map_err(|e| Status::internal(e.to_string()))
    }

    async fn set_name(&self, request: Request<SetNameRequest>) -> Result<Response<Empty>, Status> {
        self.backend
            .node_set_name(&request.into_inner().name)
            .await
            .map(|_| Response::new(Empty {}))
            .map_err(|e| Status::internal(e.to_string()))
    }

    type SubscribeStream = ReceiverStream<Result<NodeEventMessage, Status>>;

    async fn subscribe(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<Self::SubscribeStream>, Status> {
        let mut rx = self
            .backend
            .subscribe()
            .map_err(|e| Status::internal(e.to_string()))?;

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
}
