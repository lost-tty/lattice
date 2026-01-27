//! RPC Client for connecting to latticed daemon
//!
//! Provides a unified client interface for CLI and other clients to
//! communicate with the daemon over UDS.

use crate::proto::{
    dynamic_store_service_client::DynamicStoreServiceClient,
    mesh_service_client::MeshServiceClient, node_service_client::NodeServiceClient,
    store_service_client::StoreServiceClient,
};
use hyper_util::rt::TokioIo;
use std::path::Path;
use tokio::net::UnixStream;
use tonic::transport::{Channel, Endpoint, Uri};
use tower::service_fn;

/// RPC Client connected to latticed daemon
#[derive(Clone)]
pub struct RpcClient {
    pub node: NodeServiceClient<Channel>,
    pub mesh: MeshServiceClient<Channel>,
    pub store: StoreServiceClient<Channel>,
    pub dynamic: DynamicStoreServiceClient<Channel>,
}

impl RpcClient {
    /// Connect to the daemon at the given socket path
    pub async fn connect(socket_path: impl AsRef<Path>) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let socket_path = socket_path.as_ref().to_path_buf();
        
        // Dummy URI required by tonic's Endpoint API - actual connection uses Unix socket below
        let channel = Endpoint::from_static("http://[::]:0")
            .connect_with_connector(service_fn(move |_: Uri| {
                let path = socket_path.clone();
                async move {
                    let stream = UnixStream::connect(path).await?;
                    Ok::<_, std::io::Error>(TokioIo::new(stream))
                }
            }))
            .await?;

        Ok(Self {
            node: NodeServiceClient::new(channel.clone()),
            mesh: MeshServiceClient::new(channel.clone()),
            store: StoreServiceClient::new(channel.clone()),
            dynamic: DynamicStoreServiceClient::new(channel),
        })
    }

    /// Connect to the default socket path
    pub async fn connect_default() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let path = crate::RpcServer::default_socket_path();
        Self::connect(path).await
    }
}
