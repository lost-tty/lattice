//! RootState — persistent state for app definitions and bundles.
//!
//! Uses the same on-disk format as KvState (TABLE_DATA, lattice_kvtable::Value).
//! The dual-decoder `apply()` accepts both native `RootPayload` and legacy
//! `KvPayload` intentions, silently dropping KV ops that don't match the
//! `apps/` or `appbundles/` key namespaces.

use crate::proto::{
    self, GetAppRequest, GetAppResponse, GetBundleRequest, GetBundleResponse,
    ListAppsRequest, ListAppsResponse, ListBundlesRequest, ListBundlesResponse,
    RegisterAppOp, RegisterAppResponse, RemoveAppOp, RemoveAppResponse,
    RemoveBundleOp, RemoveBundleResponse, RootPayload,
    UploadBundleOp, UploadBundleRequest, UploadBundleResponse,
};
use crate::RootEvent;
use lattice_kvtable::{KvRead, KvTableError};
use lattice_model::{Hash, Op, Uuid};
use lattice_store_base::{
    dispatch::dispatch_method, event_stream, BoxByteStream, CommandHandler, Introspectable,
    StreamDescriptor, StreamError, StreamHandler, StreamProvider, Subscriber,
};
use lattice_storage::{StateContext, StateDbError, StateLogic};
use prost::Message;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use tracing::debug;

/// Current payload version. Must be > 0 to distinguish from legacy KvPayload.
const PAYLOAD_VERSION: u32 = 1;

/// Maximum bundle zip size (64 MB). Enforced at the command handler level
/// so it applies to both local uploads and replicated intentions.
const MAX_BUNDLE_SIZE: u64 = 64 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Bundle validation
// ---------------------------------------------------------------------------

/// Validate a bundle zip and return the app_id + manifest as a proto.
/// Delegates to `manifest::parse_bundle_manifest` for the actual parsing.
fn validate_bundle_manifest(
    zip_bytes: &[u8],
) -> Result<(String, proto::BundleManifest), Box<dyn std::error::Error + Send + Sync>> {
    let (app_id, manifest) = crate::manifest::parse_bundle_manifest(zip_bytes)?;

    // Convert generic manifest → proto
    let sections = manifest
        .into_iter()
        .map(|(name, entries)| proto::BundleManifestSection {
            name,
            entries: entries
                .into_iter()
                .map(|(key, value)| proto::BundleManifestEntry { key, value })
                .collect(),
        })
        .collect();

    Ok((app_id, proto::BundleManifest { sections }))
}

// ---------------------------------------------------------------------------
// Key namespace helpers
// ---------------------------------------------------------------------------

/// Returns true if the key belongs to the `apps/` or `appbundles/` namespaces.
fn is_root_key(key: &[u8]) -> bool {
    key.starts_with(b"apps/") || key.starts_with(b"appbundles/")
}

// ---------------------------------------------------------------------------
// Error conversion
// ---------------------------------------------------------------------------

trait KvTableResultExt<T> {
    fn into_state_err(self) -> Result<T, StateDbError>;
}

impl<T> KvTableResultExt<T> for Result<T, KvTableError> {
    fn into_state_err(self) -> Result<T, StateDbError> {
        self.map_err(|e| match e {
            KvTableError::Storage(e) => StateDbError::Redb(e.into()),
            KvTableError::Decode(e) => StateDbError::Decode(e),
            KvTableError::Conversion(s) => StateDbError::Conversion(s),
            KvTableError::Dag(e) => StateDbError::Conversion(e.to_string()),
        })
    }
}

// ---------------------------------------------------------------------------
// RootState
// ---------------------------------------------------------------------------

/// Persistent state for app definitions and bundles.
///
/// On-disk format is identical to KvState: TABLE_DATA with
/// `lattice_kvtable::Value` encoding per key. The difference is that
/// RootState only accepts keys in the `apps/` and `appbundles/` namespaces,
/// silently dropping everything else.
pub struct RootState {
    ctx: StateContext<RootEvent>,
}

impl std::fmt::Debug for RootState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RootState").finish_non_exhaustive()
    }
}

impl From<StateContext<RootEvent>> for RootState {
    fn from(ctx: StateContext<RootEvent>) -> Self {
        Self { ctx }
    }
}

impl RootState {
    pub fn new(ctx: StateContext<RootEvent>) -> Self {
        Self { ctx }
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<RootEvent> {
        self.ctx.subscribe()
    }

    // ---- Read helpers (same pattern as KvState) ----

    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StateDbError> {
        let txn = self.ctx.db().begin_read()?;
        let table = match txn.open_table() {
            Ok(Some(t)) => t,
            Ok(None) => return Ok(None),
            Err(e) => return Err(e),
        };
        let ro = lattice_kvtable::ReadOnlyKVTable::new(table);
        ro.get(key).into_state_err()
    }

    pub fn head_hashes(&self, key: &[u8]) -> Result<Vec<Hash>, StateDbError> {
        let txn = self.ctx.db().begin_read()?;
        let table = match txn.open_table() {
            Ok(Some(t)) => t,
            Ok(None) => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let ro = lattice_kvtable::ReadOnlyKVTable::new(table);
        ro.heads(key).into_state_err()
    }

    pub fn scan<F>(&self, prefix: &[u8], mut visitor: F) -> Result<(), StateDbError>
    where
        F: FnMut(Vec<u8>, Option<Vec<u8>>, bool) -> Result<bool, StateDbError>,
    {
        let txn = self.ctx.db().begin_read()?;
        let table = match txn.open_table() {
            Ok(Some(t)) => t,
            Ok(None) => return Ok(()),
            Err(e) => return Err(e),
        };
        let ro = lattice_kvtable::ReadOnlyKVTable::new(table);
        for result in ro.range(prefix..).into_state_err()? {
            let (key, value, _conflicted) = result.into_state_err()?;
            if !key.starts_with(prefix) {
                break;
            }
            if !visitor(key, value, _conflicted)? {
                break;
            }
        }
        Ok(())
    }

    /// Collect head hashes from all keys matching a prefix.
    fn heads_for_prefix(&self, prefix: &[u8]) -> Result<Vec<Hash>, StateDbError> {
        let mut deps = Vec::new();
        let txn = self.ctx.db().begin_read()?;
        let table = match txn.open_table() {
            Ok(Some(t)) => t,
            Ok(None) => return Ok(deps),
            Err(e) => return Err(e),
        };
        let ro = lattice_kvtable::ReadOnlyKVTable::new(table);
        for result in ro.range(prefix..).into_state_err()? {
            let (key, _, _) = result.into_state_err()?;
            if !key.starts_with(prefix) {
                break;
            }
            deps.extend(ro.heads(&key).into_state_err()?);
        }
        Ok(deps)
    }
}

// ===========================================================================
// StateLogic — the dual-decoder apply()
// ===========================================================================

impl StateLogic for RootState {
    type Event = RootEvent;

    fn store_type() -> &'static str {
        lattice_model::STORE_TYPE_ROOTSTORE
    }

    fn apply(
        table: &mut redb::Table<&[u8], &[u8]>,
        op: &Op,
        dag: &dyn lattice_model::DagQueries,
    ) -> Result<Vec<Self::Event>, StateDbError> {
        // Try native RootPayload first.
        // version > 0 distinguishes from KvPayload (which has no version field).
        if let Ok(root_payload) = RootPayload::decode(op.info.payload.as_ref()) {
            if root_payload.version > 0 && !root_payload.ops.is_empty() {
                return apply_root_payload(table, op, dag, &root_payload);
            }
        }

        // Fall back to legacy KvPayload (for stores migrated from core:kvstore)
        if let Ok(kv_payload) =
            lattice_kvstore::KvPayload::decode(op.info.payload.as_ref())
        {
            return apply_legacy_kv(table, op, dag, &kv_payload);
        }

        // Unknown format — silently drop
        debug!("RootState: dropping unknown payload format");
        Ok(vec![])
    }
}

/// Apply a single key write via `apply_head`. Returns `true` if the key was modified.
fn apply_key(
    kvt: &mut lattice_kvtable::KVTable,
    key: &[u8],
    op: &Op,
    value: Vec<u8>,
    tombstone: bool,
    dag: &dyn lattice_model::DagQueries,
) -> Result<bool, StateDbError> {
    Ok(kvt
        .apply_head(key, &op.info, op.causal_deps, value, tombstone, dag)
        .into_state_err()?
        .is_some())
}

/// Apply a native RootPayload.
///
/// Translates each `RootOperation` into the underlying KV key writes.
/// Emits one semantic `RootEvent` per operation (not per key write).
fn apply_root_payload(
    table: &mut redb::Table<&[u8], &[u8]>,
    op: &Op,
    dag: &dyn lattice_model::DagQueries,
    payload: &RootPayload,
) -> Result<Vec<RootEvent>, StateDbError> {
    let mut kvt = lattice_kvtable::KVTable::new(table);
    let mut events = Vec::new();

    // Ops are applied in reverse order to match the KvState convention:
    // when multiple ops in one intention touch the same key, the *first* op
    // in the original list wins because it's applied last (LWW with the same
    // hash/timestamp treats later apply_head calls as no-ops when the head
    // is already set).
    for root_op in payload.ops.iter().rev() {
        match &root_op.op {
            Some(proto::root_operation::Op::RegisterApp(r)) => {
                let sid_key = format!("apps/{}/store-id", r.subdomain);
                let aid_key = format!("apps/{}/app-id", r.subdomain);
                let store_id_str = Uuid::from_slice(&r.store_id)
                    .map(|u| u.to_string())
                    .unwrap_or_default();

                let mut changed = false;
                for (key, value) in [
                    (sid_key, store_id_str.into_bytes()),
                    (aid_key, r.app_id.as_bytes().to_vec()),
                ] {
                    changed |= apply_key(&mut kvt, key.as_bytes(), op, value, false, dag)?;
                }
                if changed {
                    events.push(RootEvent::AppChanged {
                        subdomain: r.subdomain.clone(),
                    });
                }
            }
            Some(proto::root_operation::Op::RemoveApp(r)) => {
                let sid_key = format!("apps/{}/store-id", r.subdomain);
                let aid_key = format!("apps/{}/app-id", r.subdomain);

                let mut changed = false;
                for key in [sid_key, aid_key] {
                    changed |= apply_key(&mut kvt, key.as_bytes(), op, Vec::new(), true, dag)?;
                }
                if changed {
                    events.push(RootEvent::AppRemoved {
                        subdomain: r.subdomain.clone(),
                    });
                }
            }
            Some(proto::root_operation::Op::UploadBundle(r)) => {
                let mut changed = false;

                let zip_key = format!("appbundles/{}/bundle.zip", r.app_id);
                changed |= apply_key(&mut kvt, zip_key.as_bytes(), op, r.data.clone(), false, dag)?;

                if !r.manifest.is_empty() {
                    if let Ok(manifest) = proto::BundleManifest::decode(r.manifest.as_slice()) {
                        for section in &manifest.sections {
                            for entry in &section.entries {
                                let mkey = format!("appbundles/{}/manifest/{}/{}", r.app_id, section.name, entry.key);
                                changed |= apply_key(&mut kvt, mkey.as_bytes(), op, entry.value.as_bytes().to_vec(), false, dag)?;
                            }
                        }
                    }
                }

                if changed {
                    events.push(RootEvent::BundleChanged {
                        app_id: r.app_id.clone(),
                    });
                }
            }
            Some(proto::root_operation::Op::RemoveBundle(r)) => {
                let mut changed = false;

                let zip_key = format!("appbundles/{}/bundle.zip", r.app_id);
                changed |= apply_key(&mut kvt, zip_key.as_bytes(), op, Vec::new(), true, dag)?;

                // Collect manifest keys first (can't mutate while iterating)
                let manifest_prefix = format!("appbundles/{}/manifest/", r.app_id);
                let manifest_keys: Vec<Vec<u8>> = {
                    let mut keys = Vec::new();
                    for result in kvt.range(manifest_prefix.as_bytes()..).into_state_err()? {
                        let (k, _, _) = result.into_state_err()?;
                        if !k.starts_with(manifest_prefix.as_bytes()) {
                            break;
                        }
                        keys.push(k);
                    }
                    keys
                };
                for key in &manifest_keys {
                    changed |= apply_key(&mut kvt, key, op, Vec::new(), true, dag)?;
                }

                if changed {
                    events.push(RootEvent::BundleRemoved {
                        app_id: r.app_id.clone(),
                    });
                }
            }
            None => continue,
        }
    }

    Ok(events)
}

/// Apply a legacy KvPayload, filtering to root-store key namespaces.
///
/// Only `apps/` and `appbundles/` keys are applied; everything else
/// is silently dropped. This allows seamless migration from core:kvstore.
///
/// Coalesces per-key writes into one semantic `RootEvent` per entity.
fn apply_legacy_kv(
    table: &mut redb::Table<&[u8], &[u8]>,
    op: &Op,
    dag: &dyn lattice_model::DagQueries,
    kv_payload: &lattice_kvstore::KvPayload,
) -> Result<Vec<RootEvent>, StateDbError> {
    use lattice_kvstore::proto::operation;
    use std::collections::HashSet;

    let mut kvt = lattice_kvtable::KVTable::new(table);

    // Track which entities changed/were removed to emit one event each.
    let mut apps_changed: HashSet<String> = HashSet::new();
    let mut apps_removed: HashSet<String> = HashSet::new();
    let mut bundles_changed: HashSet<String> = HashSet::new();
    let mut bundles_removed: HashSet<String> = HashSet::new();

    for kv_op in kv_payload.ops.iter().rev() {
        if let Some(op_type) = &kv_op.op_type {
            let (key, value, tombstone) = match op_type {
                operation::OpType::Put(put) => (&put.key, put.value.clone(), false),
                operation::OpType::Delete(del) => (&del.key, Vec::new(), true),
            };

            if !is_root_key(key) {
                continue;
            }

            match kvt.apply_head(key, &op.info, op.causal_deps, value, tombstone, dag) {
                Ok(Some(winner)) => {
                    let key_str = String::from_utf8_lossy(key);
                    if key_str.starts_with("apps/") {
                        let parts: Vec<&str> = key_str.split('/').collect();
                        if parts.len() >= 2 {
                            let subdomain = parts[1].to_string();
                            if winner.is_none() {
                                apps_removed.insert(subdomain);
                            } else {
                                apps_changed.insert(subdomain);
                            }
                        }
                    } else if key_str.starts_with("appbundles/") {
                        if let Some(app_id) = key_str
                            .strip_prefix("appbundles/")
                            .and_then(|s| s.strip_suffix("/bundle.zip"))
                        {
                            if winner.is_none() {
                                bundles_removed.insert(app_id.to_string());
                            } else {
                                bundles_changed.insert(app_id.to_string());
                            }
                        }
                    }
                }
                Ok(None) => {}
                Err(e) => return Err(e).into_state_err(),
            }
        }
    }

    let mut events = Vec::new();
    // Changed takes priority over removed (if both keys in an app were touched)
    for subdomain in apps_changed {
        apps_removed.remove(&subdomain);
        events.push(RootEvent::AppChanged { subdomain });
    }
    for subdomain in apps_removed {
        events.push(RootEvent::AppRemoved { subdomain });
    }
    for app_id in bundles_changed {
        bundles_removed.remove(&app_id);
        events.push(RootEvent::BundleChanged { app_id });
    }
    for app_id in bundles_removed {
        events.push(RootEvent::BundleRemoved { app_id });
    }

    Ok(events)
}

// ===========================================================================
// StreamProvider
// ===========================================================================

struct AppsSubscriber;

impl Subscriber<RootState> for AppsSubscriber {
    fn subscribe<'a>(
        &'a self,
        state: &'a RootState,
        params: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = Result<BoxByteStream, StreamError>> + Send + 'a>> {
        let _ = params;
        Box::pin(async move {
            Ok(event_stream(
                state.subscribe(),
                move |event: RootEvent| {
                    let proto_event = match event {
                        RootEvent::AppChanged { subdomain } => proto::WatchAppsEvent {
                            event: Some(proto::watch_apps_event::Event::Updated(
                                proto::AppEntry {
                                    subdomain,
                                    app_id: String::new(),
                                    store_id: Vec::new(),
                                },
                            )),
                        },
                        RootEvent::AppRemoved { subdomain } => proto::WatchAppsEvent {
                            event: Some(proto::watch_apps_event::Event::Removed(subdomain)),
                        },
                        // Not an app event — skip
                        _ => return None,
                    };
                    let mut buf = Vec::new();
                    proto_event.encode(&mut buf).ok()?;
                    Some(buf)
                },
            ))
        })
    }
}

struct BundlesSubscriber;

impl Subscriber<RootState> for BundlesSubscriber {
    fn subscribe<'a>(
        &'a self,
        state: &'a RootState,
        params: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = Result<BoxByteStream, StreamError>> + Send + 'a>> {
        let _ = params;
        Box::pin(async move {
            Ok(event_stream(
                state.subscribe(),
                move |event: RootEvent| {
                    let proto_event = match event {
                        RootEvent::BundleChanged { app_id } => proto::WatchBundlesEvent {
                            event: Some(proto::watch_bundles_event::Event::UpdatedAppId(app_id)),
                        },
                        RootEvent::BundleRemoved { app_id } => proto::WatchBundlesEvent {
                            event: Some(proto::watch_bundles_event::Event::RemovedAppId(app_id)),
                        },
                        _ => return None,
                    };
                    let mut buf = Vec::new();
                    proto_event.encode(&mut buf).ok()?;
                    Some(buf)
                },
            ))
        })
    }
}

impl StreamProvider for RootState {
    fn stream_handlers(&self) -> Vec<StreamHandler<Self>> {
        vec![
            StreamHandler {
                descriptor: StreamDescriptor {
                    name: "watch-apps".to_string(),
                    description: "Subscribe to app definition changes".to_string(),
                    param_schema: Some("lattice.rootstore.WatchAppsParams".to_string()),
                    event_schema: Some("lattice.rootstore.WatchAppsEvent".to_string()),
                },
                subscriber: Box::new(AppsSubscriber),
            },
            StreamHandler {
                descriptor: StreamDescriptor {
                    name: "watch-bundles".to_string(),
                    description: "Subscribe to bundle changes".to_string(),
                    param_schema: Some("lattice.rootstore.WatchBundlesParams".to_string()),
                    event_schema: Some("lattice.rootstore.WatchBundlesEvent".to_string()),
                },
                subscriber: Box::new(BundlesSubscriber),
            },
        ]
    }
}

// ===========================================================================
// CommandHandler
// ===========================================================================

impl CommandHandler for RootState {
    fn handle_command<'a>(
        &'a self,
        writer: &'a dyn lattice_model::StateWriter,
        method_name: &'a str,
        request: prost_reflect::DynamicMessage,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<
                        prost_reflect::DynamicMessage,
                        Box<dyn std::error::Error + Send + Sync>,
                    >,
                > + Send
                + 'a,
        >,
    > {
        let desc = self.service_descriptor();
        Box::pin(async move {
            match method_name {
                "RegisterApp" => {
                    dispatch_method(method_name, request, desc, |req| {
                        self.handle_register_app(writer, req)
                    })
                    .await
                }
                "RemoveApp" => {
                    dispatch_method(method_name, request, desc, |req| {
                        self.handle_remove_app(writer, req)
                    })
                    .await
                }
                "UploadBundle" => {
                    dispatch_method(method_name, request, desc, |req| {
                        self.handle_upload_bundle(writer, req)
                    })
                    .await
                }
                "RemoveBundle" => {
                    dispatch_method(method_name, request, desc, |req| {
                        self.handle_remove_bundle(writer, req)
                    })
                    .await
                }
                _ => Err(format!("Unknown command: {}", method_name).into()),
            }
        })
    }

    fn handle_query<'a>(
        &'a self,
        _dag: &'a dyn lattice_model::DagQueries,
        method_name: &'a str,
        request: prost_reflect::DynamicMessage,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<
                        prost_reflect::DynamicMessage,
                        Box<dyn std::error::Error + Send + Sync>,
                    >,
                > + Send
                + 'a,
        >,
    > {
        let desc = self.service_descriptor();
        Box::pin(async move {
            match method_name {
                "GetApp" => {
                    dispatch_method(method_name, request, desc, |req| self.handle_get_app(req))
                        .await
                }
                "ListApps" => {
                    dispatch_method(method_name, request, desc, |req| self.handle_list_apps(req))
                        .await
                }
                "GetBundle" => {
                    dispatch_method(method_name, request, desc, |req| {
                        self.handle_get_bundle(req)
                    })
                    .await
                }
                "ListBundles" => {
                    dispatch_method(method_name, request, desc, |req| {
                        self.handle_list_bundles(req)
                    })
                    .await
                }
                _ => Err(format!("Unknown query: {}", method_name).into()),
            }
        })
    }
}

// ===========================================================================
// Command / query handlers
// ===========================================================================

impl RootState {
    async fn handle_register_app(
        &self,
        writer: &dyn lattice_model::StateWriter,
        req: RegisterAppOp,
    ) -> Result<RegisterAppResponse, Box<dyn std::error::Error + Send + Sync>> {
        if req.subdomain.is_empty() {
            return Err("subdomain cannot be empty".into());
        }
        if req.app_id.is_empty() {
            return Err("app_id cannot be empty".into());
        }

        // Collect causal deps from both keys
        let sid_key = format!("apps/{}/store-id", req.subdomain);
        let aid_key = format!("apps/{}/app-id", req.subdomain);
        let mut causal_deps = self.head_hashes(sid_key.as_bytes())?;
        causal_deps.extend(self.head_hashes(aid_key.as_bytes())?);

        let payload = RootPayload {
            version: PAYLOAD_VERSION,
            ops: vec![proto::RootOperation {
                op: Some(proto::root_operation::Op::RegisterApp(req)),
            }],
        };
        writer.submit(payload.encode_to_vec(), causal_deps).await?;
        Ok(RegisterAppResponse {})
    }

    async fn handle_remove_app(
        &self,
        writer: &dyn lattice_model::StateWriter,
        req: RemoveAppOp,
    ) -> Result<RemoveAppResponse, Box<dyn std::error::Error + Send + Sync>> {
        if req.subdomain.is_empty() {
            return Err("subdomain cannot be empty".into());
        }

        let sid_key = format!("apps/{}/store-id", req.subdomain);
        let aid_key = format!("apps/{}/app-id", req.subdomain);
        let mut causal_deps = self.head_hashes(sid_key.as_bytes())?;
        causal_deps.extend(self.head_hashes(aid_key.as_bytes())?);

        let payload = RootPayload {
            version: PAYLOAD_VERSION,
            ops: vec![proto::RootOperation {
                op: Some(proto::root_operation::Op::RemoveApp(req)),
            }],
        };
        writer.submit(payload.encode_to_vec(), causal_deps).await?;
        Ok(RemoveAppResponse {})
    }

    async fn handle_upload_bundle(
        &self,
        writer: &dyn lattice_model::StateWriter,
        req: UploadBundleRequest,
    ) -> Result<UploadBundleResponse, Box<dyn std::error::Error + Send + Sync>> {
        if req.data.is_empty() {
            return Err("bundle data cannot be empty".into());
        }
        if req.data.len() as u64 > MAX_BUNDLE_SIZE {
            return Err(format!(
                "bundle exceeds size limit ({} > {} bytes)",
                req.data.len(),
                MAX_BUNDLE_SIZE
            )
            .into());
        }

        // Validate the zip and extract app_id + manifest proto
        let (manifest_app_id, manifest) = validate_bundle_manifest(&req.data)?;

        let app_id = if req.app_id.is_empty() {
            manifest_app_id
        } else if req.app_id != manifest_app_id {
            return Err(format!(
                "manifest.toml app.id '{}' does not match request app_id '{}'",
                manifest_app_id, req.app_id
            )
            .into());
        } else {
            req.app_id
        };

        // Gather causal deps from bundle.zip + all manifest keys
        let zip_key = format!("appbundles/{}/bundle.zip", app_id);
        let manifest_prefix = format!("appbundles/{}/manifest/", app_id);
        let mut causal_deps = self.head_hashes(zip_key.as_bytes())?;
        causal_deps.extend(self.heads_for_prefix(manifest_prefix.as_bytes())?);

        // Build the internal op with the parsed manifest
        let op = UploadBundleOp {
            app_id: app_id.clone(),
            data: req.data,
            manifest: manifest.encode_to_vec(),
        };
        let payload = RootPayload {
            version: PAYLOAD_VERSION,
            ops: vec![proto::RootOperation {
                op: Some(proto::root_operation::Op::UploadBundle(op)),
            }],
        };
        writer.submit(payload.encode_to_vec(), causal_deps).await?;
        Ok(UploadBundleResponse { app_id })
    }

    async fn handle_remove_bundle(
        &self,
        writer: &dyn lattice_model::StateWriter,
        req: RemoveBundleOp,
    ) -> Result<RemoveBundleResponse, Box<dyn std::error::Error + Send + Sync>> {
        if req.app_id.is_empty() {
            return Err("app_id cannot be empty".into());
        }

        // Gather causal deps from bundle.zip + all manifest keys
        let zip_key = format!("appbundles/{}/bundle.zip", req.app_id);
        let manifest_prefix = format!("appbundles/{}/manifest/", req.app_id);
        let mut causal_deps = self.head_hashes(zip_key.as_bytes())?;
        causal_deps.extend(self.heads_for_prefix(manifest_prefix.as_bytes())?);


        let payload = RootPayload {
            version: PAYLOAD_VERSION,
            ops: vec![proto::RootOperation {
                op: Some(proto::root_operation::Op::RemoveBundle(req)),
            }],
        };
        writer.submit(payload.encode_to_vec(), causal_deps).await?;
        Ok(RemoveBundleResponse {})
    }

    async fn handle_get_app(
        &self,
        req: GetAppRequest,
    ) -> Result<GetAppResponse, Box<dyn std::error::Error + Send + Sync>> {
        let sid_key = format!("apps/{}/store-id", req.subdomain);
        let aid_key = format!("apps/{}/app-id", req.subdomain);

        let store_id = self.get(sid_key.as_bytes())?;
        let app_id = self.get(aid_key.as_bytes())?;

        match (store_id, app_id) {
            (Some(sid), Some(aid)) => {
                let uuid = Uuid::parse_str(&String::from_utf8_lossy(&sid))
                    .unwrap_or_default();
                Ok(GetAppResponse {
                    found: true,
                    subdomain: req.subdomain,
                    app_id: String::from_utf8_lossy(&aid).to_string(),
                    store_id: uuid.as_bytes().to_vec(),
                })
            }
            _ => Ok(GetAppResponse {
                found: false,
                subdomain: req.subdomain,
                app_id: String::new(),
                store_id: Vec::new(),
            }),
        }
    }

    async fn handle_get_bundle(
        &self,
        req: GetBundleRequest,
    ) -> Result<GetBundleResponse, Box<dyn std::error::Error + Send + Sync>> {
        let key = format!("appbundles/{}/bundle.zip", req.app_id);
        match self.get(key.as_bytes())? {
            Some(data) => Ok(GetBundleResponse {
                found: true,
                app_id: req.app_id,
                data,
            }),
            None => Ok(GetBundleResponse {
                found: false,
                app_id: req.app_id,
                data: Vec::new(),
            }),
        }
    }

    async fn handle_list_apps(
        &self,
        _req: ListAppsRequest,
    ) -> Result<ListAppsResponse, Box<dyn std::error::Error + Send + Sync>> {
        let mut apps: HashMap<String, proto::AppEntry> = HashMap::new();

        self.scan(b"apps/", |key, value, _| {
            if let Some(val) = value {
                let key_str = String::from_utf8_lossy(&key);
                let parts: Vec<&str> = key_str.split('/').collect();
                if parts.len() == 3 && parts[0] == "apps" {
                    let subdomain = parts[1].to_string();
                    let field = parts[2];
                    let entry = apps.entry(subdomain.clone()).or_insert_with(|| {
                        proto::AppEntry {
                            subdomain,
                            app_id: String::new(),
                            store_id: Vec::new(),
                        }
                    });
                    match field {
                        "store-id" => {
                            if let Ok(uuid) =
                                Uuid::parse_str(&String::from_utf8_lossy(&val))
                            {
                                entry.store_id = uuid.as_bytes().to_vec();
                            }
                        }
                        "app-id" => {
                            entry.app_id = String::from_utf8_lossy(&val).to_string();
                        }
                        _ => {}
                    }
                }
            }
            Ok(true)
        })?;

        Ok(ListAppsResponse {
            items: apps.into_values().collect(),
        })
    }

    async fn handle_list_bundles(
        &self,
        _req: ListBundlesRequest,
    ) -> Result<ListBundlesResponse, Box<dyn std::error::Error + Send + Sync>> {
        // Scan all appbundles/ keys, group by app_id, reconstruct manifest.
        // Keys: appbundles/{app_id}/bundle.zip  (presence = bundle exists)
        //       appbundles/{app_id}/manifest/{section}/{key}  (manifest data)
        let mut entries: HashMap<String, proto::BundleEntry> = HashMap::new();

        self.scan(b"appbundles/", |key, value, _| {
            if let Some(val) = value {
                let key_str = String::from_utf8_lossy(&key);
                let rest = match key_str.strip_prefix("appbundles/") {
                    Some(r) => r,
                    None => return Ok(true),
                };

                // Split into app_id and the rest: "{app_id}/{remainder}"
                let (app_id, remainder) = match rest.split_once('/') {
                    Some(pair) => pair,
                    None => return Ok(true),
                };

                let entry = entries.entry(app_id.to_string()).or_insert_with(|| {
                    proto::BundleEntry {
                        app_id: app_id.to_string(),
                        manifest: None,
                    }
                });

                // Parse manifest keys: "manifest/{section}/{key}"
                if let Some(manifest_rest) = remainder.strip_prefix("manifest/") {
                    if let Some((section_name, field)) = manifest_rest.split_once('/') {
                        let manifest = entry.manifest.get_or_insert_with(|| {
                            proto::BundleManifest {
                                sections: Vec::new(),
                            }
                        });
                        let section = match manifest.sections.iter_mut().find(|s| s.name == section_name) {
                            Some(s) => s,
                            None => {
                                manifest.sections.push(proto::BundleManifestSection {
                                    name: section_name.to_string(),
                                    entries: Vec::new(),
                                });
                                manifest.sections.last_mut().unwrap()
                            }
                        };
                        section.entries.push(proto::BundleManifestEntry {
                            key: field.to_string(),
                            value: String::from_utf8_lossy(&val).to_string(),
                        });
                    }
                }
                // bundle.zip presence is implicit (the entry exists)
            }
            Ok(true)
        })?;

        Ok(ListBundlesResponse {
            items: entries.into_values().collect(),
        })
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_kvstore::proto::Operation as KvOperation;
    use lattice_model::dag_queries::NullDag;
    use lattice_model::hlc::HLC;
    use lattice_model::{PubKey, Uuid};
    use lattice_mockkernel::TestHarness;

    type RootHarness = TestHarness<RootState>;

    static NULL_DAG: NullDag = NullDag;

    /// Build a BundleManifest proto for testing.
    fn make_test_manifest(app_id: &str, name: &str, version: &str, store_type: &str) -> proto::BundleManifest {
        proto::BundleManifest {
            sections: vec![proto::BundleManifestSection {
                name: "app".to_string(),
                entries: vec![
                    proto::BundleManifestEntry { key: "id".to_string(), value: app_id.to_string() },
                    proto::BundleManifestEntry { key: "name".to_string(), value: name.to_string() },
                    proto::BundleManifestEntry { key: "version".to_string(), value: version.to_string() },
                    proto::BundleManifestEntry { key: "store_type".to_string(), value: store_type.to_string() },
                ],
            }],
        }
    }

    fn make_op(payload_bytes: Vec<u8>, hash: Hash) -> Op<'static> {
        make_op_with_deps(payload_bytes, hash, &[])
    }

    fn make_op_with_deps(payload_bytes: Vec<u8>, hash: Hash, deps: &[Hash]) -> Op<'static> {
        let causal_deps: &'static [Hash] = Vec::leak(deps.to_vec());
        Op {
            info: lattice_model::IntentionInfo {
                hash,
                payload: std::borrow::Cow::Owned(payload_bytes),
                timestamp: HLC::now(),
                author: PubKey::from([1u8; 32]),
            },
            causal_deps,
            prev_hash: Hash::ZERO,
        }
    }

    // ---- Native RootPayload tests ----

    #[test]
    fn native_register_app() {
        let h = RootHarness::new();
        let store_id = Uuid::new_v4();

        let payload = RootPayload {
            version: PAYLOAD_VERSION,
            ops: vec![proto::RootOperation {
                op: Some(proto::root_operation::Op::RegisterApp(RegisterAppOp {
                    subdomain: "inventory".into(),
                    app_id: "inv-app".into(),
                    store_id: store_id.as_bytes().to_vec(),
                })),
            }],
        };
        let op = make_op(payload.encode_to_vec(), Hash::from([2u8; 32]));
        h.apply(&op, &NULL_DAG).unwrap();

        // Verify keys written
        let sid = h.store.get(b"apps/inventory/store-id").unwrap().unwrap();
        assert_eq!(String::from_utf8_lossy(&sid), store_id.to_string());
        let aid = h.store.get(b"apps/inventory/app-id").unwrap().unwrap();
        assert_eq!(String::from_utf8_lossy(&aid), "inv-app");
    }

    #[test]
    fn native_remove_app() {
        let h = RootHarness::new();

        // Test that RemoveApp creates tombstones
        let rm_payload = RootPayload {
            version: PAYLOAD_VERSION,
            ops: vec![proto::RootOperation {
                op: Some(proto::root_operation::Op::RemoveApp(RemoveAppOp {
                    subdomain: "gone".into(),
                })),
            }],
        };
        h.apply(&make_op(rm_payload.encode_to_vec(), Hash::from([3u8; 32])), &NULL_DAG)
            .unwrap();
        // Tombstoned keys should return None
        assert!(h.store.get(b"apps/gone/store-id").unwrap().is_none());
        assert!(h.store.get(b"apps/gone/app-id").unwrap().is_none());
    }

    #[test]
    fn native_upload_bundle() {
        let h = RootHarness::new();

        let manifest = make_test_manifest("myapp", "My App", "1.0.0", "core:kvstore");
        let payload = RootPayload {
            version: PAYLOAD_VERSION,
            ops: vec![proto::RootOperation {
                op: Some(proto::root_operation::Op::UploadBundle(UploadBundleOp {
                    app_id: "myapp".into(),
                    data: b"fake-zip-data".to_vec(),
                    manifest: manifest.encode_to_vec(),
                })),
            }],
        };
        h.apply(&make_op(payload.encode_to_vec(), Hash::from([1u8; 32])), &NULL_DAG)
            .unwrap();

        // Verify zip data
        assert_eq!(
            h.store.get(b"appbundles/myapp/bundle.zip").unwrap(),
            Some(b"fake-zip-data".to_vec())
        );

        // Verify manifest keys
        assert_eq!(
            h.store.get(b"appbundles/myapp/manifest/app/id").unwrap(),
            Some(b"myapp".to_vec())
        );
        assert_eq!(
            h.store.get(b"appbundles/myapp/manifest/app/name").unwrap(),
            Some(b"My App".to_vec())
        );
        assert_eq!(
            h.store.get(b"appbundles/myapp/manifest/app/version").unwrap(),
            Some(b"1.0.0".to_vec())
        );
        assert_eq!(
            h.store.get(b"appbundles/myapp/manifest/app/store_type").unwrap(),
            Some(b"core:kvstore".to_vec())
        );
    }

    #[test]
    fn native_remove_bundle() {
        let h = RootHarness::new();

        // First upload a bundle with manifest
        let upload_hash = Hash::from([1u8; 32]);
        let manifest = make_test_manifest("gone", "Gone App", "0.1.0", "core:kvstore");
        let upload = RootPayload {
            version: PAYLOAD_VERSION,
            ops: vec![proto::RootOperation {
                op: Some(proto::root_operation::Op::UploadBundle(UploadBundleOp {
                    app_id: "gone".into(),
                    data: b"zip-data".to_vec(),
                    manifest: manifest.encode_to_vec(),
                })),
            }],
        };
        h.apply(&make_op(upload.encode_to_vec(), upload_hash), &NULL_DAG)
            .unwrap();
        // Verify it exists
        assert!(h.store.get(b"appbundles/gone/bundle.zip").unwrap().is_some());
        assert!(h.store.get(b"appbundles/gone/manifest/app/id").unwrap().is_some());

        // Now remove it (causally depends on the upload)
        let remove = RootPayload {
            version: PAYLOAD_VERSION,
            ops: vec![proto::RootOperation {
                op: Some(proto::root_operation::Op::RemoveBundle(RemoveBundleOp {
                    app_id: "gone".into(),
                })),
            }],
        };
        h.apply(
            &make_op_with_deps(remove.encode_to_vec(), Hash::from([2u8; 32]), &[upload_hash]),
            &NULL_DAG,
        )
        .unwrap();

        // Both zip and manifest keys should be tombstoned
        assert!(h.store.get(b"appbundles/gone/bundle.zip").unwrap().is_none());
        assert!(h.store.get(b"appbundles/gone/manifest/app/id").unwrap().is_none());
        assert!(h.store.get(b"appbundles/gone/manifest/app/name").unwrap().is_none());
        assert!(h.store.get(b"appbundles/gone/manifest/app/version").unwrap().is_none());
        assert!(h.store.get(b"appbundles/gone/manifest/app/store_type").unwrap().is_none());
    }

    // ---- Legacy KvPayload compat tests ----

    #[test]
    fn legacy_kv_app_keys_applied() {
        let h = RootHarness::new();
        let store_id = Uuid::new_v4();

        let kv_payload = lattice_kvstore::KvPayload {
            ops: vec![
                KvOperation::put(b"apps/crm/store-id", store_id.to_string().as_bytes()),
                KvOperation::put(b"apps/crm/app-id", b"crm-app"),
            ],
        };
        let op = make_op(kv_payload.encode_to_vec(), Hash::from([3u8; 32]));
        h.apply(&op, &NULL_DAG).unwrap();

        let sid = h.store.get(b"apps/crm/store-id").unwrap().unwrap();
        assert_eq!(String::from_utf8_lossy(&sid), store_id.to_string());
    }

    #[test]
    fn legacy_kv_bundle_key_applied() {
        let h = RootHarness::new();

        let kv_payload = lattice_kvstore::KvPayload {
            ops: vec![KvOperation::put(
                b"appbundles/myapp/bundle.zip",
                b"zip-bytes",
            )],
        };
        let op = make_op(kv_payload.encode_to_vec(), Hash::from([4u8; 32]));
        h.apply(&op, &NULL_DAG).unwrap();
        assert_eq!(
            h.store.get(b"appbundles/myapp/bundle.zip").unwrap(),
            Some(b"zip-bytes".to_vec())
        );
    }

    #[test]
    fn legacy_kv_non_root_keys_silently_dropped() {
        let h = RootHarness::new();

        let kv_payload = lattice_kvstore::KvPayload {
            ops: vec![
                KvOperation::put(b"user-data/key1", b"value1"),
                KvOperation::put(b"some/random/key", b"value2"),
                KvOperation::put(b"apps/inv/app-id", b"inventory"), // this one stays
            ],
        };
        let op = make_op(kv_payload.encode_to_vec(), Hash::from([5u8; 32]));
        h.apply(&op, &NULL_DAG).unwrap();

        // Non-root keys should not exist
        assert!(h.store.get(b"user-data/key1").unwrap().is_none());
        assert!(h.store.get(b"some/random/key").unwrap().is_none());

        // Root key should exist
        assert!(h.store.get(b"apps/inv/app-id").unwrap().is_some());
    }

    #[test]
    fn unknown_payload_silently_dropped() {
        let h = RootHarness::new();

        let op = make_op(b"garbage-bytes-not-protobuf".to_vec(), Hash::from([6u8; 32]));
        // Should not panic — silently dropped
        h.apply(&op, &NULL_DAG).unwrap();
    }
}
