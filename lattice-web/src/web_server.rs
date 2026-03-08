//! Axum HTTP + WebSocket server

use crate::tunnel;
use crate::ui::StaticFiles;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use lattice_api::backend::Backend;
use lattice_api::proto::{
    dynamic_store_service_server::DynamicStoreServiceServer,
    node_service_server::NodeServiceServer, store_service_server::StoreServiceServer,
};
use lattice_api::{DynamicStoreServiceImpl, NodeServiceImpl, StoreServiceImpl};
use std::net::SocketAddr;
use std::sync::Arc;
use tonic::service::Routes;

/// Shared state for the web server: tonic gRPC routes + backend for descriptor lookups.
struct AppState {
    routes: Routes,
    backend: Backend,
}

/// The web server: serves UI on `/` and WebSocket tunnel on `/ws`.
pub struct WebServer {
    backend: Backend,
    port: u16,
}

impl WebServer {
    /// Create a new web server bound to the given port.
    pub fn new(backend: Backend, port: u16) -> Self {
        Self { backend, port }
    }

    /// The URL the web UI will be reachable at.
    pub fn url(&self) -> String {
        crate::web_url(self.port)
    }

    /// Run the web server (blocks until shutdown).
    pub async fn run(self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let routes = Routes::new(NodeServiceServer::new(NodeServiceImpl::new(
            self.backend.clone(),
        )))
        .add_service(StoreServiceServer::new(StoreServiceImpl::new(
            self.backend.clone(),
        )))
        .add_service(DynamicStoreServiceServer::new(
            DynamicStoreServiceImpl::new(self.backend.clone()),
        ))
        .prepare();

        let state = Arc::new(AppState {
            routes,
            backend: self.backend.clone(),
        });

        let app = Router::new()
            .route("/", get(serve_index))
            .route("/static/{*path}", get(serve_static))
            .route("/proto/api.bin", get(serve_api_proto))
            .route("/proto/tunnel.bin", get(serve_tunnel_proto))
            .route("/proto/weaver.bin", get(serve_weaver_proto))
            .route("/proto/store/{uuid}", get(serve_store_proto))
            .route("/ws", get(ws_handler))
            .with_state(state);

        let addr = SocketAddr::new(std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST), self.port);
        tracing::info!("Lattice web UI listening on {}", self.url());

        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, app).await?;

        Ok(())
    }
}

async fn serve_index() -> impl IntoResponse {
    match StaticFiles::get("index.html") {
        Some(file) => (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8".to_string())],
            file.data,
        )
            .into_response(),
        None => (StatusCode::INTERNAL_SERVER_ERROR, "index.html missing").into_response(),
    }
}

async fn serve_static(Path(path): Path<String>) -> impl IntoResponse {
    match StaticFiles::get(&path) {
        Some(file) => {
            let mime = mime_guess::from_path(&path)
                .first_or_octet_stream()
                .to_string();
            ([(header::CONTENT_TYPE, mime)], file.data).into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Serve the API FileDescriptorSet as raw binary.
async fn serve_api_proto() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/octet-stream")],
        lattice_api::FILE_DESCRIPTOR_SET,
    )
}

/// Serve the tunnel FileDescriptorSet as raw binary.
async fn serve_tunnel_proto() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/octet-stream")],
        crate::TUNNEL_DESCRIPTOR,
    )
}

/// Serve the weaver FileDescriptorSet as raw binary.
async fn serve_weaver_proto() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/octet-stream")],
        lattice_proto::FILE_DESCRIPTOR_SET,
    )
}

async fn serve_store_proto(
    Path(uuid_str): Path<String>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let store_id = match uuid::Uuid::parse_str(&uuid_str) {
        Ok(id) => id,
        Err(_) => return (StatusCode::BAD_REQUEST, "Invalid UUID".to_string()).into_response(),
    };
    match state.backend.store_get_descriptor(store_id).await {
        Ok((fds_bytes, _service_name)) => (
            [(header::CONTENT_TYPE, "application/octet-stream".to_string())],
            fds_bytes,
        )
            .into_response(),
        Err(e) => (StatusCode::NOT_FOUND, e.to_string()).into_response(),
    }
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> Response {
    let routes = state.routes.clone();
    ws.on_upgrade(|socket| tunnel::handle_ws(socket, routes))
}
