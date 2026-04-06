//! RPC Server with UDS listener

use std::path::PathBuf;
use tokio::net::UnixListener;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::service::Routes;
use tonic::transport::Server;

/// RPC Server for latticed daemon
pub struct RpcServer {
    routes: Option<Routes>,
    socket_path: PathBuf,
}

impl RpcServer {
    pub fn new(routes: Routes, socket_path: PathBuf) -> Self {
        Self { routes: Some(routes), socket_path }
    }

    /// Default socket path using platform-specific data directory
    pub fn default_socket_path() -> PathBuf {
        let base = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("./data"))
            .join("lattice");
        base.join("latticed.sock")
    }

    /// Run the RPC server
    pub async fn run(mut self) -> Result<(), Box<dyn std::error::Error>> {
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

        tracing::debug!("RPC listening on {:?}", self.socket_path);

        let routes = self.routes.take()
            .ok_or("RpcServer already consumed")?;
        Server::builder()
            .add_routes(routes)
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
