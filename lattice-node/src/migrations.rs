//! One-off data migrations.
//!
//! Each migration runs once during `Node::start()` and is idempotent.
//! Remove individual migrations once all deployed nodes have upgraded past them.

use crate::data_dir::DataDir;
use crate::meta_store::MetaStore;
use lattice_model::{STORE_TYPE_KVSTORE, STORE_TYPE_ROOTSTORE};
use lattice_storage::state_db::{KEY_STORE_TYPE, TABLE_META};
use tracing::{info, warn};
use uuid::Uuid;

/// Run all pending migrations. Called once at the start of `Node::start()`,
/// before any stores are opened.
pub fn run_all(meta: &MetaStore, data_dir: &DataDir) {
    migrate_rootstore_types(meta, data_dir);
}

// ============================================================================
// Migration: core:kvstore → core:rootstore for root stores
//
// Context: Root stores were originally created as core:kvstore. The new
// RootStore type (core:rootstore) uses typed commands and the same on-disk
// format but a different state machine. This migration updates the store
// type in both meta.db and each store's state.db so they open with the
// correct opener.
//
// Safe to remove once all nodes have been upgraded.
// ============================================================================

fn migrate_rootstore_types(meta: &MetaStore, data_dir: &DataDir) {
    let root_ids: Vec<Uuid> = match meta.list_rootstores() {
        Ok(stores) => stores.into_iter().map(|(id, _)| id).collect(),
        Err(_) => return,
    };

    for store_id in root_ids {
        let record = match meta.get_store(store_id) {
            Ok(Some(r)) => r,
            _ => continue,
        };

        if record.store_type != STORE_TYPE_KVSTORE {
            continue;
        }

        info!(
            store = %store_id,
            "Migrating root store type: {} → {}",
            STORE_TYPE_KVSTORE,
            STORE_TYPE_ROOTSTORE
        );

        // 1. Update meta.db (STORES_TABLE)
        if let Err(e) = migrate_meta_store_type(meta, store_id, &record) {
            warn!(store = %store_id, error = %e, "Failed to migrate store type in meta.db");
            continue;
        }

        // 2. Update the store's own state.db (TABLE_META)
        let state_dir = data_dir.store_state_dir(store_id);
        if let Err(e) = migrate_state_db_store_type(&state_dir) {
            warn!(store = %store_id, error = %e, "Failed to migrate store type in state.db");
            // Revert meta.db to avoid mismatch
            let _ = revert_meta_store_type(meta, store_id, &record);
        }
    }
}

/// Update the store type in meta.db's STORES_TABLE.
fn migrate_meta_store_type(
    meta: &MetaStore,
    store_id: Uuid,
    old_record: &lattice_kernel::proto::storage::StoreRecord,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut updated = old_record.clone();
    updated.store_type = STORE_TYPE_ROOTSTORE.to_string();

    // Re-insert with the new type. add_store overwrites.
    meta.add_store(
        store_id,
        Uuid::from_slice(&old_record.parent_id).unwrap_or(Uuid::nil()),
        &updated.store_type,
    )?;
    Ok(())
}

/// Revert meta.db on failure (best-effort).
fn revert_meta_store_type(
    meta: &MetaStore,
    store_id: Uuid,
    old_record: &lattice_kernel::proto::storage::StoreRecord,
) {
    let _ = meta.add_store(
        store_id,
        Uuid::from_slice(&old_record.parent_id).unwrap_or(Uuid::nil()),
        &old_record.store_type,
    );
}

/// Update the store type in the store's own state.db (TABLE_META).
fn migrate_state_db_store_type(
    state_dir: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let state_db_path = state_dir.join("state.db");
    if !state_db_path.exists() {
        // No state.db yet — it will be created with the correct type on first open.
        return Ok(());
    }

    let db = redb::Database::open(&state_db_path)?;
    let write_txn = db.begin_write()?;
    {
        let mut table = write_txn.open_table(TABLE_META)?;
        table.insert(KEY_STORE_TYPE, STORE_TYPE_ROOTSTORE.as_bytes())?;
    }
    write_txn.commit()?;

    Ok(())
}
