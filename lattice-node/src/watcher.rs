use crate::{store_manager::{StoreManager, StoreManagerError}, StoreHandle, Uuid};
use futures_util::StreamExt;
use lattice_model::{store_info::ChildStatus, SystemEvent};
use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use tracing::{debug, info, warn};

/// Watcher that recursively monitors a root store and opens declared child stores.
pub struct RecursiveWatcher {
    store_manager: Arc<StoreManager>,
    root_store_id: Uuid,
    root_store: Arc<dyn StoreHandle>,
    opened_stores: Arc<RwLock<HashSet<Uuid>>>,
    shutdown_tx: tokio::sync::broadcast::Sender<()>,
}

impl RecursiveWatcher {
    /// Create a new RecursiveWatcher for a given root store.
    pub fn new(
        store_manager: Arc<StoreManager>,
        root_store_id: Uuid,
        root_store: Arc<dyn StoreHandle>,
    ) -> Self {
        let (shutdown_tx, _) = tokio::sync::broadcast::channel(1);
        Self {
            store_manager,
            root_store_id,
            root_store,
            opened_stores: Arc::new(RwLock::new(HashSet::new())),
            shutdown_tx,
        }
    }

    /// Start the watcher task.
    pub fn start(&self) {
        let store_manager = self.store_manager.clone();
        let root_store_id = self.root_store_id;
        let root_store = self.root_store.clone();
        let opened_stores = self.opened_stores.clone();
        let mut shutdown_rx = self.shutdown_tx.subscribe();

        tokio::spawn(async move {
            // Get SystemStore capability
            let system = match root_store.clone().as_system() {
                Some(s) => s,
                None => {
                    warn!(store_id = %root_store_id, "Root store does not support SystemStore");
                    return;
                }
            };

            // 1. Subscribe to system events (Log-based + Local Ephemeral)
            let mut stream = system.subscribe_events();

            // 2. Initial reconcile (manual list)
            if let Err(e) = Self::reconcile_stores(
                &store_manager,
                &root_store,
                root_store_id,
                &opened_stores,
            )
            .await
            {
                warn!(error = %e, "Initial reconcile failed");
            }

            debug!("Store watcher started for root {}", root_store_id);

            // 3. Process unified stream
            loop {
                tokio::select! {
                    _ = shutdown_rx.recv() => {
                        debug!("Store watcher shutting down");
                        break;
                    }
                    next = stream.next() => {
                        match next {
                            Some(evt) => {
                                let should_reconcile = matches!(
                                    evt,
                                    SystemEvent::ChildLinkUpdated(_)
                                        | SystemEvent::ChildStatusUpdated(_, _)
                                        | SystemEvent::ChildLinkRemoved(_)
                                        | SystemEvent::BootstrapComplete
                                );

                                if should_reconcile {
                                    if let Err(e) = Self::reconcile_stores(&store_manager, &root_store, root_store_id, &opened_stores).await {
                                        warn!(error = %e, "Reconcile failed");
                                    }
                                }
                            }
                            None => break,
                        }
                    }
                }
            }
        });
    }

    pub async fn shutdown(&self) {
        let _ = self.shutdown_tx.send(());
    }

    /// Manually track a store as "opened by this watcher" (e.g. created manually).
    pub fn track_store(&self, store_id: Uuid) {
        if let Ok(mut guard) = self.opened_stores.write() {
            guard.insert(store_id);
        }
    }

    async fn reconcile_stores(
        store_manager: &Arc<StoreManager>,
        root: &Arc<dyn StoreHandle>,
        root_store_id: Uuid,
        opened_stores: &Arc<RwLock<HashSet<Uuid>>>,
    ) -> Result<(), String> {
        let declarations = Self::list_declarations(root).await?;

        // Get IDs of stores we have opened (from our tracking set)
        let our_stores: HashSet<Uuid> = opened_stores.read().map(|g| g.clone()).unwrap_or_default();

        // Get current store IDs in StoreManager (excluding others?)
        // Actually we just check if it's open in SM
        let current_ids: HashSet<Uuid> = store_manager.store_ids().into_iter().collect();

        let declared_ids: HashSet<Uuid> = declarations
            .iter()
            .filter(|d| !d.archived)
            .map(|d| d.id)
            .collect();

        for decl in &declarations {
            if decl.archived {
                if our_stores.contains(&decl.id) && current_ids.contains(&decl.id) {
                    let _ = store_manager.close(&decl.id);
                    if let Ok(mut guard) = opened_stores.write() {
                        guard.remove(&decl.id);
                    }
                    info!(store_id = %decl.id, "Closed archived store");
                }
            } else {
                if !current_ids.contains(&decl.id) {
                    // Try open (already in meta.db), fallback to create (first discovery)
                    let result = match store_manager.open(decl.id).await {
                        Ok(handle) => Ok(handle),
                        Err(StoreManagerError::NotFound(_)) => {
                            store_manager
                                .create(
                                    decl.id,
                                    Some(root_store_id),
                                    &decl.store_type,
                                    None,
                                    Some(lattice_model::store_info::PeerStrategy::Inherited),
                                )
                                .await
                        }
                        Err(e) => Err(e),
                    };
                    match result {
                        Ok(handle) => {
                            // MIGRATION: backfill genesis for pre-genesis child stores.
                            let st = handle.store_type().to_string();
                            crate::genesis::ensure_genesis(decl.id, &handle, &st).await;
                            if let Ok(mut guard) = opened_stores.write() {
                                guard.insert(decl.id);
                            }
                            debug!(store_id = %decl.id, store_type = %decl.store_type, "Opened child store");
                        }
                        Err(e) => {
                            warn!(store_id = %decl.id, error = ?e, "Failed to open child store");
                        }
                    }
                } else {
                    // Already open. Adopt it into our tracking set if not already tracked.
                    // This allows manual creation (Node::create_store) to be managed by watcher.
                    if !our_stores.contains(&decl.id) {
                        if let Ok(mut guard) = opened_stores.write() {
                            guard.insert(decl.id);
                        }
                        debug!(store_id = %decl.id, "Adopted existing store into watcher");
                    }
                }
            }
        }

        // Close stores that WE opened but are no longer declared
        for store_id in &our_stores {
            if !declared_ids.contains(store_id) && current_ids.contains(store_id) {
                let _ = store_manager.close(store_id);
                if let Ok(mut guard) = opened_stores.write() {
                    guard.remove(store_id);
                }
                info!(store_id = %store_id, "Closed undeclared store");
            }
        }

        Ok(())
    }

    async fn list_declarations(
        root: &Arc<dyn StoreHandle>,
    ) -> Result<Vec<StoreDeclaration>, String> {
        let system = root
            .clone()
            .as_system()
            .ok_or_else(|| "Root store must support SystemStore".to_string())?;

        let children = system.get_children().map_err(|e| e.to_string())?;

        let mut declarations = Vec::new();
        for child in children {
            declarations.push(StoreDeclaration {
                id: child.id,
                store_type: child.store_type.unwrap_or_else(|| "unknown".to_string()),
                name: child.alias,
                archived: match child.status {
                    ChildStatus::Archived => true,
                    _ => false,
                },
            });
        }
        Ok(declarations)
    }
}

pub struct StoreDeclaration {
    pub id: Uuid,
    pub store_type: String,
    pub name: Option<String>,
    pub archived: bool,
}
