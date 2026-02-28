//! RPC Server with UDS listener

use crate::backend::Backend;
use crate::dynamic_store_service::DynamicStoreServiceImpl;
use crate::node_service::NodeServiceImpl;
use crate::proto::{
    dynamic_store_service_server::DynamicStoreServiceServer,
    node_service_server::NodeServiceServer, store_service_server::StoreServiceServer,
};
use crate::store_service::StoreServiceImpl;
use std::path::PathBuf;
use tokio::net::UnixListener;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::transport::Server;

/// RPC Server for latticed daemon
pub struct RpcServer {
    backend: Backend,
    socket_path: PathBuf,
}

impl RpcServer {
    pub fn new(backend: Backend, socket_path: PathBuf) -> Self {
        Self {
            backend,
            socket_path,
        }
    }

    /// Default socket path using platform-specific data directory
    pub fn default_socket_path() -> PathBuf {
        let base = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("./data"))
            .join("lattice");
        base.join("latticed.sock")
    }

    /// Run the RPC server
    pub async fn run(self) -> Result<(), Box<dyn std::error::Error>> {
        // Remove stale socket if exists
        if self.socket_path.exists() {
            std::fs::remove_file(&self.socket_path)?;
        }

        // Ensure parent directory exists
        if let Some(parent) = self.socket_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let uds = UnixListener::bind(&self.socket_path)?;

        // Secure the socket (RW for owner only)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(metadata) = std::fs::metadata(&self.socket_path) {
                let mut perms = metadata.permissions();
                perms.set_mode(0o600);
                let _ = std::fs::set_permissions(&self.socket_path, perms);
            }
        }

        let uds_stream = UnixListenerStream::new(uds);

        tracing::info!("RPC server listening on {:?}", self.socket_path);

        // Create all service implementations - all wrap the same backend
        let node_service = NodeServiceImpl::new(self.backend.clone());
        let store_service = StoreServiceImpl::new(self.backend.clone());
        let dynamic_store_service = DynamicStoreServiceImpl::new(self.backend.clone());

        Server::builder()
            .add_service(NodeServiceServer::new(node_service))
            .add_service(StoreServiceServer::new(store_service))
            .add_service(DynamicStoreServiceServer::new(dynamic_store_service))
            .serve_with_incoming(uds_stream)
            .await?;

        Ok(())
    }
}

impl Drop for RpcServer {
    fn drop(&mut self) {
        // Cleanup socket on shutdown
        let _ = std::fs::remove_file(&self.socket_path);
    }
}
