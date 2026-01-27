//! RPC Server with UDS listener

use crate::dynamic_store_service::DynamicStoreServiceImpl;
use crate::mesh_service::MeshServiceImpl;
use crate::node_service::NodeServiceImpl;
use crate::proto::{
    dynamic_store_service_server::DynamicStoreServiceServer,
    mesh_service_server::MeshServiceServer, node_service_server::NodeServiceServer,
    store_service_server::StoreServiceServer,
};
use crate::store_service::StoreServiceImpl;
use lattice_net::MeshService as NetMeshService;
use lattice_node::Node;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::UnixListener;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::transport::Server;

/// RPC Server for latticed daemon
pub struct RpcServer {
    node: Arc<Node>,
    mesh_network: Option<Arc<NetMeshService>>,
    socket_path: PathBuf,
}

impl RpcServer {
    pub fn new(node: Arc<Node>, socket_path: PathBuf) -> Self {
        Self { node, mesh_network: None, socket_path }
    }
    
    /// Set the network MeshService for peer info
    pub fn with_mesh_service(mut self, mesh_network: Arc<NetMeshService>) -> Self {
        self.mesh_network = Some(mesh_network);
        self
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

        // Create all service implementations
        let node_service = NodeServiceImpl::new(self.node.clone());
        let mesh_service = MeshServiceImpl::new(self.node.clone(), self.mesh_network.clone());
        let store_service = StoreServiceImpl::new(self.node.clone());
        let dynamic_store_service = DynamicStoreServiceImpl::new(self.node.clone());

        Server::builder()
            .add_service(NodeServiceServer::new(node_service))
            .add_service(MeshServiceServer::new(mesh_service))
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

