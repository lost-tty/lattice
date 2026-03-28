//! App manager — watches root stores, notifies consumers of app/bundle changes.
//!
//! All app CRUD (register, remove, upload/remove bundle) is done directly
//! through the root store's typed commands. AppManager's role is:
//! 1. Watch root stores and emit `AppEvent`s for the web server
//! 2. Manage the local enabled/disabled toggle (MetaStore)

use crate::meta_store::MetaStore;
use crate::node::NodeEvent;
use crate::store_handle::StoreHandle;
use crate::store_manager::StoreManager;
use futures_util::StreamExt;
use lattice_model::AppBinding;
use lattice_rootstore::proto::{
    GetAppRequest, GetAppResponse, GetBundleRequest, GetBundleResponse, ListAppsRequest,
    ListAppsResponse, ListBundlesRequest, ListBundlesResponse,
};
use lattice_store_base::{invoke_command, StreamReflectable};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Events emitted by AppManager to notify consumers of state changes.
#[derive(Clone, Debug)]
pub enum AppEvent {
    /// An app is available to serve (new or updated).
    AppAvailable(AppBinding),
    /// An app should stop being served (removed from root store or disabled).
    AppRemoved { subdomain: String },
    /// Bundle zip bytes updated for an app. Uses `Bytes` for O(1) clone
    /// across broadcast subscribers.
    BundleUpdated { app_id: String, data: bytes::Bytes },
    /// Bundle removed for an app.
    BundleRemoved { app_id: String },
}

// ============================================================================
// Helpers
// ============================================================================

/// Return handles for root stores (core:rootstore only).
fn root_handles(
    store_manager: &StoreManager,
    meta: &MetaStore,
) -> Vec<(Uuid, Arc<dyn StoreHandle>)> {
    let root_ids: HashSet<Uuid> = meta
        .list_rootstores()
        .unwrap_or_default()
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    store_manager
        .list()
        .into_iter()
        .filter(|(id, t)| t == lattice_model::STORE_TYPE_ROOTSTORE && root_ids.contains(id))
        .filter_map(|(id, _)| store_manager.get_handle(&id).map(|h| (id, h)))
        .collect()
}

/// Build subdomain→enabled map from MetaStore.
fn local_state(meta: &MetaStore) -> HashMap<String, bool> {
    meta.list_app_bindings()
        .unwrap_or_default()
        .into_iter()
        .map(|b| (b.subdomain, b.enabled))
        .collect()
}

/// Convert RootStore `ListAppsResponse` into AppBinding map, enriched with local state.
fn apps_from_response(
    resp: &ListAppsResponse,
    registry_store_id: Uuid,
    local: &HashMap<String, bool>,
    apps: &mut HashMap<String, AppBinding>,
) {
    for entry in &resp.apps {
        let enabled = local.get(&entry.subdomain).copied().unwrap_or(false);
        let store_id = Uuid::from_slice(&entry.store_id).unwrap_or(Uuid::nil());
        apps.insert(
            entry.subdomain.clone(),
            AppBinding {
                subdomain: entry.subdomain.clone(),
                app_id: entry.app_id.clone(),
                store_id,
                registry_store_id,
                enabled,
            },
        );
    }
}

/// Scan a single root store and collect all app + bundle events.
async fn collect_store_events(
    dispatcher: &dyn lattice_store_base::CommandDispatcher,
    store_id: Uuid,
    meta: &MetaStore,
) -> Vec<AppEvent> {
    let local = local_state(meta);
    let mut events = Vec::new();

    // App events
    if let Ok(resp) = invoke_command::<ListAppsRequest, ListAppsResponse>(
        dispatcher,
        "ListApps",
        ListAppsRequest {},
    )
    .await
    {
        let mut apps: HashMap<String, AppBinding> = HashMap::new();
        apps_from_response(&resp, store_id, &local, &mut apps);
        for app in apps.into_values() {
            if app.enabled {
                events.push(AppEvent::AppAvailable(app));
            }
        }
    }

    // Bundle events
    if let Ok(resp) = invoke_command::<ListBundlesRequest, ListBundlesResponse>(
        dispatcher,
        "ListBundles",
        ListBundlesRequest {},
    )
    .await
    {
        for bundle in &resp.bundles {
            if let Ok(get_resp) = invoke_command::<GetBundleRequest, GetBundleResponse>(
                dispatcher,
                "GetBundle",
                GetBundleRequest {
                    app_id: bundle.app_id.clone(),
                },
            )
            .await
            {
                if get_resp.found {
                    info!(app_id = %bundle.app_id, store = %store_id, "Loaded bundle");
                    events.push(AppEvent::BundleUpdated {
                        app_id: bundle.app_id.clone(),
                        data: get_resp.data.into(),
                    });
                }
            }
        }
    }

    events
}

/// Load all apps and bundles from a single store and emit events.
async fn load_and_emit(
    store_manager: &StoreManager,
    meta: &MetaStore,
    store_id: Uuid,
    app_tx: &broadcast::Sender<AppEvent>,
) {
    let handle = match store_manager.get_handle(&store_id) {
        Some(h) => h,
        None => return,
    };
    let dispatcher = handle.as_dispatcher();
    for event in collect_store_events(dispatcher.as_ref(), store_id, meta).await {
        let _ = app_tx.send(event);
    }
}

/// Subscribe to a store stream and process each event with a callback.
/// Handles the get-handle → subscribe → decode loop boilerplate.
async fn watch_stream<T, F, Fut>(
    store_manager: &StoreManager,
    store_id: Uuid,
    stream_name: &str,
    mut on_event: F,
) where
    T: prost::Message + Default,
    F: FnMut(T) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let handle = match store_manager.get_handle(&store_id) {
        Some(h) => h,
        None => return,
    };
    let stream = match handle.subscribe(stream_name, &[]).await {
        Ok(s) => s,
        Err(e) => {
            warn!(store = %store_id, stream = stream_name, "Failed to subscribe: {:?}", e);
            return;
        }
    };
    tokio::pin!(stream);

    while let Some(event_bytes) = stream.next().await {
        if let Ok(event) = T::decode(event_bytes.as_ref()) {
            on_event(event).await;
        }
    }
    debug!(store = %store_id, stream = stream_name, "Watch stream ended");
}

/// Spawn watch tasks for apps and bundles on a single store.
fn spawn_watchers(
    store_manager: Arc<StoreManager>,
    meta: Arc<MetaStore>,
    store_id: Uuid,
    app_tx: broadcast::Sender<AppEvent>,
) {
    // Watch apps
    {
        let store_manager = store_manager.clone();
        let meta = meta.clone();
        let app_tx = app_tx.clone();
        tokio::spawn(async move {
            watch_stream(
                &store_manager,
                store_id,
                "watch-apps",
                |event: lattice_rootstore::proto::WatchAppsEvent| {
                    let store_manager = store_manager.clone();
                    let meta = meta.clone();
                    let app_tx = app_tx.clone();
                    async move {
                        match event.event {
                            Some(lattice_rootstore::proto::watch_apps_event::Event::Updated(entry)) => {
                                let subdomain = entry.subdomain;
                                let local = local_state(&meta);
                                let enabled = local.get(&subdomain).copied().unwrap_or(false);

                                let handle = match store_manager.get_handle(&store_id) {
                                    Some(h) => h,
                                    None => return,
                                };
                                let dispatcher = handle.as_dispatcher();
                                if let Ok(resp) = invoke_command::<GetAppRequest, GetAppResponse>(
                                    dispatcher.as_ref(),
                                    "GetApp",
                                    GetAppRequest { subdomain: subdomain.clone() },
                                )
                                .await
                                {
                                    if resp.found {
                                        let app = AppBinding {
                                            subdomain: subdomain.clone(),
                                            app_id: resp.app_id,
                                            store_id: Uuid::from_slice(&resp.store_id)
                                                .unwrap_or(Uuid::nil()),
                                            registry_store_id: store_id,
                                            enabled,
                                        };
                                        info!(subdomain = %subdomain, "App updated via replication");
                                        if enabled {
                                            let _ = app_tx.send(AppEvent::AppAvailable(app));
                                        }
                                    }
                                }
                            }
                            Some(lattice_rootstore::proto::watch_apps_event::Event::Removed(subdomain)) => {
                                info!(subdomain = %subdomain, "App removed via replication");
                                let _ = app_tx.send(AppEvent::AppRemoved { subdomain });
                            }
                            None => {}
                        }
                    }
                },
            )
            .await;
        });
    }

    // Watch bundles
    {
        tokio::spawn(async move {
            watch_stream(
                &store_manager,
                store_id,
                "watch-bundles",
                |event: lattice_rootstore::proto::WatchBundlesEvent| {
                    let store_manager = store_manager.clone();
                    let app_tx = app_tx.clone();
                    async move {
                        match event.event {
                            Some(lattice_rootstore::proto::watch_bundles_event::Event::UpdatedAppId(app_id)) => {
                                let handle = match store_manager.get_handle(&store_id) {
                                    Some(h) => h,
                                    None => return,
                                };
                                let dispatcher = handle.as_dispatcher();
                                match invoke_command::<GetBundleRequest, GetBundleResponse>(
                                    dispatcher.as_ref(),
                                    "GetBundle",
                                    GetBundleRequest { app_id: app_id.clone() },
                                )
                                .await
                                {
                                    Ok(resp) if resp.found => {
                                        info!(app_id = %app_id, store = %store_id, bytes = resp.data.len(), "Bundle updated via replication");
                                        let _ = app_tx.send(AppEvent::BundleUpdated { app_id, data: resp.data.into() });
                                    }
                                    Ok(_) => {
                                        warn!(app_id = %app_id, store = %store_id, "Bundle watch fired but GetBundle returned not found");
                                    }
                                    Err(e) => {
                                        warn!(app_id = %app_id, store = %store_id, error = %e, "Failed to fetch bundle data");
                                    }
                                }
                            }
                            Some(lattice_rootstore::proto::watch_bundles_event::Event::RemovedAppId(app_id)) => {
                                info!(app_id = %app_id, store = %store_id, "Bundle removed via replication");
                                let _ = app_tx.send(AppEvent::BundleRemoved { app_id });
                            }
                            None => {}
                        }
                    }
                },
            )
            .await;
        });
    }
}

// ============================================================================
// AppManager
// ============================================================================

pub struct AppManager {
    store_manager: Arc<StoreManager>,
    meta: Arc<MetaStore>,
    node_events: broadcast::Sender<NodeEvent>,
    app_tx: broadcast::Sender<AppEvent>,
    /// Serializes set_enabled calls to prevent TOCTOU races on the
    /// conflict check (read binding → check → write binding).
    enable_lock: tokio::sync::Mutex<()>,
}

impl AppManager {
    pub fn new(
        store_manager: Arc<StoreManager>,
        meta: Arc<MetaStore>,
        node_events: broadcast::Sender<NodeEvent>,
    ) -> Self {
        let (app_tx, _) = broadcast::channel(256);
        let enable_lock = tokio::sync::Mutex::new(());
        Self {
            store_manager,
            meta,
            node_events,
            app_tx,
            enable_lock,
        }
    }

    /// Attach a consumer: returns current state as events + a receiver for future changes.
    pub async fn attach(&self) -> (Vec<AppEvent>, broadcast::Receiver<AppEvent>) {
        let rx = self.app_tx.subscribe();
        let mut events = Vec::new();

        for (store_id, handle) in root_handles(&self.store_manager, &self.meta) {
            let dispatcher = handle.as_dispatcher();
            events.extend(
                collect_store_events(dispatcher.as_ref(), store_id, &self.meta).await,
            );
        }

        (events, rx)
    }

    /// List apps enabled on this node.
    pub async fn list_active(&self) -> Vec<AppBinding> {
        self.list()
            .await
            .into_iter()
            .filter(|a| a.enabled)
            .collect()
    }

    /// List all apps across root stores, enriched with local enabled/disabled state.
    pub async fn list(&self) -> Vec<AppBinding> {
        let local = local_state(&self.meta);
        let mut apps: HashMap<String, AppBinding> = HashMap::new();

        for (store_id, handle) in root_handles(&self.store_manager, &self.meta) {
            let dispatcher = handle.as_dispatcher();
            if let Ok(resp) = invoke_command::<ListAppsRequest, ListAppsResponse>(
                dispatcher.as_ref(),
                "ListApps",
                ListAppsRequest {},
            )
            .await
            {
                apps_from_response(&resp, store_id, &local, &mut apps);
            }
        }

        apps.into_values().collect()
    }

    /// Get a single app by subdomain.
    pub async fn get(&self, subdomain: &str) -> Option<AppBinding> {
        let local = local_state(&self.meta);

        for (store_id, handle) in root_handles(&self.store_manager, &self.meta) {
            let dispatcher = handle.as_dispatcher();
            let resp = match invoke_command::<GetAppRequest, GetAppResponse>(
                dispatcher.as_ref(),
                "GetApp",
                GetAppRequest {
                    subdomain: subdomain.to_string(),
                },
            )
            .await
            {
                Ok(r) => r,
                Err(_) => continue,
            };

            if resp.found {
                let enabled = local.get(subdomain).copied().unwrap_or(false);
                return Some(AppBinding {
                    subdomain: subdomain.to_string(),
                    app_id: resp.app_id,
                    store_id: Uuid::from_slice(&resp.store_id).unwrap_or(Uuid::nil()),
                    registry_store_id: store_id,
                    enabled,
                });
            }
        }
        None
    }

    /// Enable or disable an app locally. Updates MetaStore and emits event.
    ///
    /// `registry_store_id` identifies which root store the app belongs to,
    /// disambiguating when the same subdomain exists in multiple stores.
    ///
    /// Enabling fails if another app with the same subdomain is already
    /// enabled (from a different root store).
    pub async fn set_enabled(
        &self,
        registry_store_id: Uuid,
        subdomain: &str,
        enabled: bool,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Serialize to prevent TOCTOU races on the conflict check.
        let _guard = self.enable_lock.lock().await;

        // Look up the app in the specified root store.
        let handle = self
            .store_manager
            .get_handle(&registry_store_id)
            .ok_or_else(|| format!("Root store {} not available", registry_store_id))?;

        let dispatcher = handle.as_dispatcher();
        let resp = invoke_command::<GetAppRequest, GetAppResponse>(
            dispatcher.as_ref(),
            "GetApp",
            GetAppRequest {
                subdomain: subdomain.to_string(),
            },
        )
        .await?;

        if !resp.found {
            return Err(format!(
                "No app '{}' in root store {}",
                subdomain,
                registry_store_id
            )
            .into());
        }

        // Check for subdomain conflict when enabling.
        if enabled {
            if let Some(existing) = self.meta.get_app_binding(subdomain)? {
                if existing.enabled && existing.registry_store_id != registry_store_id {
                    return Err(format!(
                        "Subdomain '{}' is already enabled from root store {}",
                        subdomain, existing.registry_store_id
                    )
                    .into());
                }
            }
        }

        let binding = AppBinding {
            subdomain: subdomain.to_string(),
            app_id: resp.app_id,
            store_id: Uuid::from_slice(&resp.store_id).unwrap_or(Uuid::nil()),
            registry_store_id,
            enabled,
        };
        self.meta.set_app_binding(&binding)?;

        if enabled {
            let _ = self.app_tx.send(AppEvent::AppAvailable(binding));
        } else {
            let _ = self.app_tx.send(AppEvent::AppRemoved {
                subdomain: subdomain.to_string(),
            });
        }

        Ok(())
    }

    /// Initial load + start watching. Awaits initial scan, spawns background watchers.
    pub async fn start(&self) {
        let mut rx = self.node_events.subscribe();
        let mut watched: HashSet<Uuid> = HashSet::new();

        for (store_id, _) in root_handles(&self.store_manager, &self.meta) {
            if !watched.insert(store_id) {
                continue;
            }
            load_and_emit(&self.store_manager, &self.meta, store_id, &self.app_tx).await;
            spawn_watchers(
                self.store_manager.clone(),
                self.meta.clone(),
                store_id,
                self.app_tx.clone(),
            );
        }

        info!("AppManager: initial scan complete");

        let store_manager = self.store_manager.clone();
        let meta = self.meta.clone();
        let app_tx = self.app_tx.clone();
        tokio::spawn(async move {
            loop {
                let event = match rx.recv().await {
                    Ok(e) => e,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(lagged = n, "App manager node-event listener lagged, missed {} events", n);
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                };
                let store_id = match event {
                    NodeEvent::StoreReady { store_id } => store_id,
                    _ => continue,
                };
                if !watched.insert(store_id) {
                    continue;
                }
                let info = match store_manager.get_info(&store_id) {
                    Some(i) => i,
                    None => continue,
                };
                if info.store_type != lattice_model::STORE_TYPE_ROOTSTORE {
                    continue;
                }
                info!(store = %store_id, "AppManager: new root store, loading");
                load_and_emit(&store_manager, &meta, store_id, &app_tx).await;
                spawn_watchers(store_manager.clone(), meta.clone(), store_id, app_tx.clone());
            }
            debug!("AppManager: event loop ended");
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replicated_app_defaults_to_disabled() {
        let reg_id = Uuid::new_v4();
        let store_id = Uuid::new_v4();
        let resp = ListAppsResponse {
            apps: vec![lattice_rootstore::proto::AppEntry {
                subdomain: "inventory".into(),
                app_id: "inventory".into(),
                store_id: store_id.as_bytes().to_vec(),
            }],
        };
        let local: HashMap<String, bool> = HashMap::new();
        let mut apps = HashMap::new();
        apps_from_response(&resp, reg_id, &local, &mut apps);
        assert!(!apps["inventory"].enabled);
    }

    #[test]
    fn locally_enabled_app_is_enabled() {
        let reg_id = Uuid::new_v4();
        let resp = ListAppsResponse {
            apps: vec![lattice_rootstore::proto::AppEntry {
                subdomain: "myapp".into(),
                app_id: "myapp".into(),
                store_id: Uuid::new_v4().as_bytes().to_vec(),
            }],
        };
        let mut local = HashMap::new();
        local.insert("myapp".to_string(), true);
        let mut apps = HashMap::new();
        apps_from_response(&resp, reg_id, &local, &mut apps);
        assert!(apps["myapp"].enabled);
    }

    #[test]
    fn locally_disabled_app_stays_disabled() {
        let reg_id = Uuid::new_v4();
        let resp = ListAppsResponse {
            apps: vec![lattice_rootstore::proto::AppEntry {
                subdomain: "myapp".into(),
                app_id: "myapp".into(),
                store_id: Uuid::new_v4().as_bytes().to_vec(),
            }],
        };
        let mut local = HashMap::new();
        local.insert("myapp".to_string(), false);
        let mut apps = HashMap::new();
        apps_from_response(&resp, reg_id, &local, &mut apps);
        assert!(!apps["myapp"].enabled);
    }

    #[test]
    fn multiple_apps_mixed_local_state() {
        let reg_id = Uuid::new_v4();
        let resp = ListAppsResponse {
            apps: vec![
                lattice_rootstore::proto::AppEntry {
                    subdomain: "enabled-app".into(),
                    app_id: "app1".into(),
                    store_id: Uuid::new_v4().as_bytes().to_vec(),
                },
                lattice_rootstore::proto::AppEntry {
                    subdomain: "disabled-app".into(),
                    app_id: "app2".into(),
                    store_id: Uuid::new_v4().as_bytes().to_vec(),
                },
                lattice_rootstore::proto::AppEntry {
                    subdomain: "replicated-app".into(),
                    app_id: "app3".into(),
                    store_id: Uuid::new_v4().as_bytes().to_vec(),
                },
            ],
        };
        let mut local = HashMap::new();
        local.insert("enabled-app".to_string(), true);
        local.insert("disabled-app".to_string(), false);
        let mut apps = HashMap::new();
        apps_from_response(&resp, reg_id, &local, &mut apps);
        assert!(apps["enabled-app"].enabled);
        assert!(!apps["disabled-app"].enabled);
        assert!(!apps["replicated-app"].enabled);
    }

    #[test]
    fn local_state_from_metastore() {
        let dir = tempfile::tempdir().unwrap();
        let meta = MetaStore::open(&dir.path().join("meta.db")).unwrap();
        assert!(local_state(&meta).is_empty());

        meta.set_app_binding(&AppBinding {
            subdomain: "inv".into(),
            app_id: "inventory".into(),
            store_id: Uuid::nil(),
            registry_store_id: Uuid::nil(),
            enabled: true,
        })
        .unwrap();
        meta.set_app_binding(&AppBinding {
            subdomain: "crm".into(),
            app_id: "crm".into(),
            store_id: Uuid::nil(),
            registry_store_id: Uuid::nil(),
            enabled: false,
        })
        .unwrap();

        let state = local_state(&meta);
        assert_eq!(state.get("inv"), Some(&true));
        assert_eq!(state.get("crm"), Some(&false));
        assert_eq!(state.get("unknown"), None);
    }
}
