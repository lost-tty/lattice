//! Axum HTTP + WebSocket server
//!
//! Supports two modes depending on the `Host` header:
//!
//! - **Management UI** (bare host, e.g. `localhost:8080`): serves the
//!   existing Lattice admin SPA from embedded static files.
//! - **App mode** (subdomain, e.g. `inventory.localhost:8080`): looks up
//!   the subdomain in the app routing table and serves the corresponding
//!   app bundle.

use crate::apps;
use crate::tunnel;
use crate::ui::StaticFiles;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use lattice_api::backend::Backend;
use lattice_api::proto::{
    dynamic_store_service_server::DynamicStoreServiceServer,
    node_service_server::NodeServiceServer, store_service_server::StoreServiceServer,
};
use lattice_api::{DynamicStoreServiceImpl, NodeServiceImpl, StoreServiceImpl};
use lattice_model::AppBinding;
use lattice_node::{AppEvent, AppManager};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::RwLock;
use tonic::service::Routes;

/// Shared state for the web server.
struct AppState {
    routes: Routes,
    backend: Backend,
    /// subdomain → binding (routing)
    apps: Arc<RwLock<HashMap<String, AppBinding>>>,
    /// app_id → parsed bundle (serving)
    bundles: Arc<RwLock<HashMap<String, apps::AppBundle>>>,
}

/// The web server: serves UI on `/` and WebSocket tunnel on `/ws`.
pub struct WebServer {
    backend: Backend,
    app_manager: Arc<AppManager>,
    port: u16,
}

impl WebServer {
    pub fn new(
        backend: Backend,
        app_manager: Arc<AppManager>,
        port: u16,
    ) -> Self {
        Self {
            backend,
            app_manager,
            port,
        }
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

        let apps: Arc<RwLock<HashMap<String, AppBinding>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let bundles: Arc<RwLock<HashMap<String, apps::AppBundle>>> =
            Arc::new(RwLock::new(HashMap::new()));

        // Attach to the app manager: get current state + subscribe to changes
        let (initial_events, mut rx) = self.app_manager.attach().await;
        for event in initial_events {
            apply_app_event(&apps, &bundles, event).await;
        }

        let app_count = apps.read().await.len();
        let bundle_count = bundles.read().await.len();
        tracing::debug!(apps = app_count, bundles = bundle_count, "Web server: ready");

        let apps_ref = apps.clone();
        let bundles_ref = bundles.clone();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => apply_app_event(&apps_ref, &bundles_ref, event).await,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(missed = n, "App event receiver lagged, some events were dropped");
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        let state = Arc::new(AppState {
            routes,
            backend: self.backend.clone(),
            apps: apps.clone(),
            bundles: bundles.clone(),
        });

        let app = Router::new()
            .route("/", get(serve_root))
            .route("/static/{*path}", get(serve_static))
            .route("/sdk/lattice-sdk.js", get(serve_sdk_js))
            .route("/sdk/vendor/{*path}", get(serve_sdk_vendor))
            .route("/sdk/store-id", get(serve_store_id))
            .route("/proto/api.bin", get(serve_api_proto))
            .route("/proto/tunnel.bin", get(serve_tunnel_proto))
            .route("/proto/weaver.bin", get(serve_weaver_proto))
            .route("/proto/store/{uuid}", get(serve_store_proto))
            .route("/ws", get(ws_handler))
            .route("/{*path}", get(serve_catchall))
            .with_state(state);

        // Bind to both IPv4 and IPv6 localhost for dual-stack.
        // Either may fail (e.g. IPv6 disabled), but at least one must succeed.
        let v4 = tokio::net::TcpListener::bind(
            SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), self.port),
        ).await;
        let v6 = tokio::net::TcpListener::bind(
            SocketAddr::new(std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST), self.port),
        ).await;

        match (v4, v6) {
            (Ok(v4), Ok(v6)) => {
                let app_clone = app.clone();
                tokio::select! {
                    r = axum::serve(v4, app_clone) => { r?; }
                    r = axum::serve(v6, app) => { r?; }
                }
            }
            (Ok(v4), Err(_)) => {
                axum::serve(v4, app).await?;
            }
            (Err(_), Ok(v6)) => {
                axum::serve(v6, app).await?;
            }
            (Err(e4), Err(e6)) => {
                return Err(format!("Failed to bind web server — v4: {e4}, v6: {e6}").into());
            }
        }

        Ok(())
    }
}

// ============================================================================
// Request handlers
// ============================================================================

fn get_host(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

async fn serve_root(headers: HeaderMap, State(state): State<Arc<AppState>>) -> Response {
    if is_app_subdomain(&headers) {
        serve_app_index(&headers, &state).await
    } else {
        serve_management_index()
    }
}

/// Render a styled error page from the embedded `error.html` template.
/// `back_url` controls where the "Back" link points (e.g. "/" for management UI,
/// or the full management URL for app subdomain errors).
fn error_page(status: StatusCode, message: &str, back_url: &str) -> Response {
    let body = match StaticFiles::get("error.html") {
        Some(file) => {
            let template = String::from_utf8_lossy(&file.data);
            let code = status.as_u16().to_string();
            template
                .replace("{{CODE}}", &code)
                .replace("{{MESSAGE}}", message)
                .replace("{{BACK_URL}}", back_url)
        }
        None => format!("{} {}", status.as_u16(), message),
    };
    (
        status,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        body,
    )
        .into_response()
}

fn serve_management_index() -> Response {
    match StaticFiles::get("index.html") {
        Some(file) => (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8".to_string())],
            file.data,
        )
            .into_response(),
        None => error_page(StatusCode::INTERNAL_SERVER_ERROR, "index.html missing", "/"),
    }
}

async fn serve_app_index(headers: &HeaderMap, state: &AppState) -> Response {
    let binding = match get_active_app_binding(headers, state).await {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let bundles = state.bundles.read().await;
    match bundles.get(&binding.app_id).and_then(|b| b.get("index.html")) {
        Some((data, _)) => (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8".to_string())],
            data.to_vec(),
        ).into_response(),
        None => error_page(StatusCode::NOT_FOUND, "App bundle has no index.html", &management_url(headers)),
    }
}

/// Returns true if the request is coming from an app subdomain.
fn is_app_subdomain(headers: &HeaderMap) -> bool {
    get_host(headers)
        .as_deref()
        .and_then(apps::registry::extract_subdomain)
        .is_some()
}

/// Build the management UI URL from the Host header by stripping the subdomain.
fn management_url(headers: &HeaderMap) -> String {
    get_host(headers)
        .as_deref()
        .and_then(apps::registry::strip_subdomain)
        .map(|host| format!("http://{host}"))
        .unwrap_or_else(|| "/".to_string())
}

/// Resolves the host header to an enabled AppBinding, or returns an HTTP error.
async fn get_active_app_binding(
    headers: &HeaderMap,
    state: &AppState,
) -> Result<AppBinding, Response> {
    let host = get_host(headers).unwrap_or_default();
    let back = management_url(headers);
    let subdomain = apps::registry::extract_subdomain(&host)
        .ok_or_else(|| error_page(StatusCode::BAD_REQUEST, "No subdomain", &back))?;

    match state.apps.read().await.get(subdomain).cloned() {
        Some(b) if b.enabled => Ok(b),
        Some(_) => Err(error_page(StatusCode::SERVICE_UNAVAILABLE, "App is disabled", &back)),
        None => Err(error_page(StatusCode::NOT_FOUND, "No app registered at this subdomain", &back)),
    }
}

async fn serve_static(
    headers: HeaderMap,
    Path(path): Path<String>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    // On app subdomains, only serve management static files if no app is bound
    // (so error pages can load style.css). If an app is registered, its bundle
    // handles all paths.
    if is_app_subdomain(&headers) {
        let has_app = get_active_app_binding(&headers, &state).await.is_ok();
        if has_app {
            return StatusCode::NOT_FOUND.into_response();
        }
    }
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

async fn serve_sdk_js() -> impl IntoResponse {
    match StaticFiles::get("sdk/lattice-sdk.js") {
        Some(file) => (
            [(header::CONTENT_TYPE, "application/javascript".to_string())],
            file.data,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn serve_sdk_vendor(Path(path): Path<String>) -> impl IntoResponse {
    let full_path = format!("vendor/{}", path);
    match StaticFiles::get(&full_path) {
        Some(file) => {
            let mime = mime_guess::from_path(&path)
                .first_or_octet_stream()
                .to_string();
            ([(header::CONTENT_TYPE, mime)], file.data).into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn serve_store_id(headers: HeaderMap, State(state): State<Arc<AppState>>) -> Response {
    match get_active_app_binding(&headers, &state).await {
        Ok(binding) => (
            [(header::CONTENT_TYPE, "text/plain".to_string())],
            binding.store_id.to_string(),
        ).into_response(),
        Err(resp) => resp,
    }
}

async fn serve_catchall(
    headers: HeaderMap,
    Path(path): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Response {
    if is_app_subdomain(&headers) {
        // App mode: serve file from app bundle
        let binding = match get_active_app_binding(&headers, &state).await {
            Ok(b) => b,
            Err(resp) => return resp,
        };
        let bundles = state.bundles.read().await;
        match bundles.get(&binding.app_id).and_then(|b| b.get(&path)) {
            Some((data, mime)) => {
                ([(header::CONTENT_TYPE, mime.to_string())], data.to_vec()).into_response()
            }
            None => error_page(StatusCode::NOT_FOUND, "File not found in app bundle", &management_url(&headers)),
        }
    } else {
        // Management UI: SPA fallback — serve index.html for client-side routing
        serve_management_index()
    }
}

async fn serve_api_proto() -> Response {
    (
        [(header::CONTENT_TYPE, "application/octet-stream")],
        lattice_api::FILE_DESCRIPTOR_SET,
    ).into_response()
}

async fn serve_tunnel_proto() -> Response {
    (
        [(header::CONTENT_TYPE, "application/octet-stream")],
        crate::TUNNEL_DESCRIPTOR,
    ).into_response()
}

async fn serve_weaver_proto(headers: HeaderMap) -> Response {
    if is_app_subdomain(&headers) {
        return StatusCode::NOT_FOUND.into_response();
    }
    (
        [(header::CONTENT_TYPE, "application/octet-stream")],
        lattice_proto::FILE_DESCRIPTOR_SET,
    ).into_response()
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
    headers: HeaderMap,
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> Response {
    let routes = state.routes.clone();

    let allowed_store = if is_app_subdomain(&headers) {
        match get_active_app_binding(&headers, &state).await {
            Ok(b) => Some(b.store_id),
            Err(resp) => return resp,
        }
    } else {
        None
    };

    ws.on_upgrade(move |socket| tunnel::handle_ws(socket, routes, allowed_store))
}

// ============================================================================
// App event handling
// ============================================================================

async fn apply_app_event(
    apps: &RwLock<HashMap<String, AppBinding>>,
    bundles: &RwLock<HashMap<String, apps::AppBundle>>,
    event: AppEvent,
) {
    match event {
        AppEvent::AppAvailable(binding) => {
            apps.write().await.insert(binding.subdomain.clone(), binding);
        }
        AppEvent::AppRemoved { subdomain } => {
            apps.write().await.remove(&subdomain);
        }
        AppEvent::BundleUpdated { app_id, data } => {
            match tokio::task::spawn_blocking(move || apps::AppBundle::from_zip(&data)).await {
                Ok(Ok(bundle)) => { bundles.write().await.insert(app_id, bundle); }
                Ok(Err(e)) => {
                    tracing::warn!(app_id = %app_id, "Invalid bundle zip: {e}");
                    bundles.write().await.remove(&app_id);
                }
                Err(e) => {
                    tracing::error!(app_id = %app_id, "Bundle parse task panicked: {e}");
                }
            }
        }
        AppEvent::BundleRemoved { app_id } => {
            bundles.write().await.remove(&app_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn make_binding(subdomain: &str, app_id: &str) -> AppBinding {
        AppBinding {
            subdomain: subdomain.to_string(),
            app_id: app_id.to_string(),
            store_id: Uuid::nil(),
            registry_store_id: Uuid::nil(),
            enabled: true,
        }
    }

    fn make_zip(id: &str) -> Vec<u8> {
        crate::test_utils::make_test_zip(id, "1.0")
    }

    fn empty_maps() -> (RwLock<HashMap<String, AppBinding>>, RwLock<HashMap<String, apps::AppBundle>>) {
        (RwLock::new(HashMap::new()), RwLock::new(HashMap::new()))
    }

    #[tokio::test]
    async fn app_available_adds_to_map() {
        let (apps, bundles) = empty_maps();
        apply_app_event(&apps, &bundles, AppEvent::AppAvailable(make_binding("inv", "inventory"))).await;
        assert!(apps.read().await.contains_key("inv"));
    }

    #[tokio::test]
    async fn app_removed_cleans_up() {
        let (apps, bundles) = empty_maps();
        apply_app_event(&apps, &bundles, AppEvent::AppAvailable(make_binding("inv", "inventory"))).await;
        apply_app_event(&apps, &bundles, AppEvent::AppRemoved { subdomain: "inv".into() }).await;
        assert!(apps.read().await.is_empty());
    }

    #[tokio::test]
    async fn bundle_updated_adds_to_cache() {
        let (apps, bundles) = empty_maps();
        apply_app_event(&apps, &bundles, AppEvent::BundleUpdated {
            app_id: "inventory".into(), data: make_zip("inventory").into(),
        }).await;
        assert!(bundles.read().await.contains_key("inventory"));
    }

    #[tokio::test]
    async fn bundle_removed_cleans_up() {
        let (apps, bundles) = empty_maps();
        apply_app_event(&apps, &bundles, AppEvent::BundleUpdated {
            app_id: "inventory".into(), data: make_zip("inventory").into(),
        }).await;
        apply_app_event(&apps, &bundles, AppEvent::BundleRemoved { app_id: "inventory".into() }).await;
        assert!(bundles.read().await.is_empty());
    }

    #[tokio::test]
    async fn invalid_bundle_evicts_stale() {
        let (apps, bundles) = empty_maps();
        apply_app_event(&apps, &bundles, AppEvent::BundleUpdated {
            app_id: "inventory".into(), data: make_zip("inventory").into(),
        }).await;
        assert!(bundles.read().await.contains_key("inventory"));

        apply_app_event(&apps, &bundles, AppEvent::BundleUpdated {
            app_id: "inventory".into(), data: bytes::Bytes::from_static(b"garbage"),
        }).await;
        assert!(!bundles.read().await.contains_key("inventory"));
    }

    #[tokio::test]
    async fn bundle_before_app_then_app_serves() {
        let (apps, bundles) = empty_maps();

        // Bundle uploaded first
        apply_app_event(&apps, &bundles, AppEvent::BundleUpdated {
            app_id: "inventory".into(), data: make_zip("inventory").into(),
        }).await;

        // App registered later
        apply_app_event(&apps, &bundles, AppEvent::AppAvailable(make_binding("inv", "inventory"))).await;

        // Serve lookup: app exists, bundle exists, file found
        let app_id = apps.read().await.get("inv").unwrap().app_id.clone();
        let b = bundles.read().await;
        assert!(b.get(&app_id).unwrap().get("index.html").is_some());
    }

    #[tokio::test]
    async fn app_before_bundle_then_bundle_serves() {
        let (apps, bundles) = empty_maps();

        // App registered first
        apply_app_event(&apps, &bundles, AppEvent::AppAvailable(make_binding("inv", "inventory"))).await;

        // No bundle yet
        assert!(bundles.read().await.get("inventory").is_none());

        // Bundle uploaded later
        apply_app_event(&apps, &bundles, AppEvent::BundleUpdated {
            app_id: "inventory".into(), data: make_zip("inventory").into(),
        }).await;

        // Now it serves
        let app_id = apps.read().await.get("inv").unwrap().app_id.clone();
        let b = bundles.read().await;
        assert!(b.get(&app_id).unwrap().get("index.html").is_some());
    }

    #[tokio::test]
    async fn multiple_subdomains_share_bundle() {
        let (apps, bundles) = empty_maps();

        apply_app_event(&apps, &bundles, AppEvent::BundleUpdated {
            app_id: "inventory".into(), data: make_zip("inventory").into(),
        }).await;
        apply_app_event(&apps, &bundles, AppEvent::AppAvailable(make_binding("inv1", "inventory"))).await;
        apply_app_event(&apps, &bundles, AppEvent::AppAvailable(make_binding("inv2", "inventory"))).await;

        let a = apps.read().await;
        let b = bundles.read().await;
        assert_eq!(a.get("inv1").unwrap().app_id, "inventory");
        assert_eq!(a.get("inv2").unwrap().app_id, "inventory");
        assert!(b.get("inventory").is_some());
        // One bundle, two apps
        assert_eq!(a.len(), 2);
        assert_eq!(b.len(), 1);
    }

    #[tokio::test]
    async fn bundle_update_replaces_old() {
        let (apps, bundles) = empty_maps();

        apply_app_event(&apps, &bundles, AppEvent::BundleUpdated {
            app_id: "myapp".into(), data: make_zip("myapp").into(),
        }).await;
        let v1 = bundles.read().await.get("myapp").unwrap().version.clone();

        // Upload new version
        let manifest = "[app]\nid = \"myapp\"\nname = \"Test\"\nversion = \"2.0\"\nstore_type = \"core:kvstore\"\n";
        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts = zip::write::SimpleFileOptions::default();
            zip.start_file("manifest.toml", opts).unwrap();
            std::io::Write::write_all(&mut zip, manifest.as_bytes()).unwrap();
            zip.start_file("index.html", opts).unwrap();
            std::io::Write::write_all(&mut zip, b"<h1>v2</h1>").unwrap();
            zip.finish().unwrap();
        }
        apply_app_event(&apps, &bundles, AppEvent::BundleUpdated {
            app_id: "myapp".into(), data: buf.into(),
        }).await;

        let v2 = bundles.read().await.get("myapp").unwrap().version.clone();
        assert_eq!(v1, "1.0");
        assert_eq!(v2, "2.0");
    }
}
