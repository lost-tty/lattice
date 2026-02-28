use blake3::Hasher;
use lattice_model::{Hash, Op, PubKey, StateMachine, StoreMeta};
use redb::{Database, ReadableTable, TableDefinition, TableHandle};
use std::io::{Read, Write};
use std::path::Path;
use thiserror::Error;
use uuid::Uuid;

// Standard Table Definitions
pub const TABLE_DATA: TableDefinition<&[u8], &[u8]> = TableDefinition::new("data");
pub const TABLE_META: TableDefinition<&[u8], &[u8]> = TableDefinition::new("meta");
pub const TABLE_SYSTEM: TableDefinition<&[u8], &[u8]> = TableDefinition::new("system");

pub const KEY_STORE_ID: &[u8] = b"store_id";
pub const KEY_STORE_TYPE: &[u8] = b"store_type";
pub const KEY_SCHEMA_VERSION: &[u8] = b"schema_version";
pub const PREFIX_TIP: &[u8] = b"tip/";

#[derive(Debug, Error)]
pub enum StateDbError {
    #[error("Database error: {0}")]
    Database(#[from] redb::DatabaseError),
    #[error("Table error: {0}")]
    Table(#[from] redb::TableError),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Store ID mismatch: expected {expected}, got {got}")]
    StoreIdMismatch { expected: Uuid, got: Uuid },
    #[error("Store Type mismatch: expected {expected}, got {got}")]
    StoreTypeMismatch { expected: String, got: String },
    #[error("Invalid chain: {0}")]
    InvalidChain(String),
    #[error("Conversion error: {0}")]
    Conversion(String),
    #[error("Commit error: {0}")]
    Commit(#[from] redb::CommitError),
    #[error("Transaction error: {0}")]
    Transaction(#[from] redb::TransactionError),
    #[error("Storage error: {0}")]
    Storage(#[from] redb::StorageError),
    #[error("Invalid Snapshot: {0}")]
    InvalidSnapshot(String),
}

/// A composite backend that handles standard storage boilerplate for Lattice state machines.
///
/// It owns the `redb::Database` and the `store_id`, and provides standard methods for:
/// - Opening/Creating the store with verification.
/// - Validating chain integrity (tips).
/// - Managing state identity (rolling hash).
pub struct StateBackend {
    db: Database,
    id: Uuid,
}

impl StateBackend {
    /// Peek metadata (ID, Type, Version) without fully initializing the backend.
    pub fn peek_info(state_dir: impl AsRef<Path>) -> Result<(Uuid, String, u64), StateDbError> {
        let dir = state_dir.as_ref();
        let state_db_path = dir.join("state.db");
        if !state_db_path.exists() {
            return Err(StateDbError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "state.db not found",
            )));
        }

        let db = Database::open(&state_db_path)?;
        let read_txn = db.begin_read()?;
        let table = read_txn.open_table(TABLE_META)?;

        let id = table
            .get(KEY_STORE_ID)?
            .map(|v| Uuid::from_bytes(v.value().try_into().unwrap_or_default()))
            .ok_or_else(|| StateDbError::Conversion("Missing store_id".into()))?;

        let type_str = table
            .get(KEY_STORE_TYPE)?
            .map(|v| String::from_utf8_lossy(v.value()).to_string())
            .unwrap_or_else(|| "unknown".to_string());

        let version = table
            .get(KEY_SCHEMA_VERSION)?
            .map(|v| u64::from_le_bytes(v.value().try_into().unwrap_or_default()))
            .unwrap_or(0);

        Ok((id, type_str, version))
    }

    /// Get store metadata from an open backend.
    pub fn get_meta(&self) -> StoreMeta {
        let read_txn = match self.db.begin_read() {
            Ok(txn) => txn,
            Err(_) => {
                return StoreMeta {
                    store_id: self.id,
                    ..Default::default()
                }
            }
        };
        let table = match read_txn.open_table(TABLE_META) {
            Ok(t) => t,
            Err(_) => {
                return StoreMeta {
                    store_id: self.id,
                    ..Default::default()
                }
            }
        };

        let store_type = table
            .get(KEY_STORE_TYPE)
            .ok()
            .flatten()
            .map(|v| String::from_utf8_lossy(v.value()).to_string())
            .unwrap_or_default();

        let schema_version = table
            .get(KEY_SCHEMA_VERSION)
            .ok()
            .flatten()
            .map(|v| u64::from_le_bytes(v.value().try_into().unwrap_or_default()))
            .unwrap_or(0);

        StoreMeta {
            store_id: self.id,
            store_type,
            schema_version,
        }
    }

    /// Open or create a store at the given path.
    pub fn open(
        id: Uuid,
        state_dir: impl AsRef<Path>,
        expected_type: Option<&str>,
        expected_version: u64,
    ) -> Result<Self, StateDbError> {
        let dir = state_dir.as_ref();
        if !dir.exists() {
            std::fs::create_dir_all(dir)?;
        }

        let state_db_path = dir.join("state.db");
        let db = Database::builder().create(&state_db_path)?;

        // Verify Store ID & Type
        let write_txn = db.begin_write()?;
        {
            let mut table_meta = write_txn.open_table(TABLE_META)?;

            let existing_id = table_meta
                .get(KEY_STORE_ID)?
                .map(|v| Uuid::from_bytes(v.value().try_into().unwrap_or_default()));

            if let Some(existing_id) = existing_id {
                // Verify ID
                if existing_id != id {
                    return Err(StateDbError::StoreIdMismatch {
                        expected: id,
                        got: existing_id,
                    });
                }

                // Verify Type (if expected is provided)
                if let Some(expected_type_str) = expected_type {
                    let existing_type = table_meta
                        .get(KEY_STORE_TYPE)?
                        .map(|v| String::from_utf8_lossy(v.value()).to_string());

                    if let Some(existing_type_str) = existing_type {
                        if existing_type_str != expected_type_str {
                            return Err(StateDbError::StoreTypeMismatch {
                                expected: expected_type_str.to_string(),
                                got: existing_type_str,
                            });
                        }
                    } else {
                        // Migration: If no type exists, write it now (assume it matches expected)
                        table_meta.insert(KEY_STORE_TYPE, expected_type_str.as_bytes())?;
                        table_meta.insert(
                            KEY_SCHEMA_VERSION,
                            expected_version.to_le_bytes().as_slice(),
                        )?;
                    }
                }
            } else {
                // New Store or Missing ID
                table_meta.insert(KEY_STORE_ID, id.as_bytes().as_slice())?;
                if let Some(type_str) = expected_type {
                    table_meta.insert(KEY_STORE_TYPE, type_str.as_bytes())?;
                    table_meta.insert(
                        KEY_SCHEMA_VERSION,
                        expected_version.to_le_bytes().as_slice(),
                    )?;
                }
            }
        }
        write_txn.commit()?;

        Ok(Self { db, id })
    }

    /// Access the underlying Redb database.
    pub fn db(&self) -> &Database {
        &self.db
    }

    /// Access the Store ID.
    pub fn id(&self) -> Uuid {
        self.id
    }

    /// Verify chain integrity (link to prev_hash) and update author tip.
    /// Returns Ok(true) if op should be applied, Ok(false) if idempotent duplicate.
    pub fn verify_and_update_tip(
        &self,
        txn: &redb::WriteTransaction,
        author: &PubKey,
        op_id: Hash,
        prev_hash: Hash,
    ) -> Result<bool, StateDbError> {
        let mut table_meta = txn.open_table(TABLE_META)?;
        let tip_key = [PREFIX_TIP, author.as_slice()].concat();
        let current_tip_bytes = table_meta
            .get(tip_key.as_slice())?
            .map(|v| v.value().to_vec());

        match current_tip_bytes {
            Some(bytes) => {
                let current_tip = Hash::try_from(bytes.as_slice())
                    .map_err(|_| StateDbError::Conversion("Invalid tip hash in meta".into()))?;

                // Idempotency
                if op_id == current_tip {
                    return Ok(false);
                }

                // Link Check
                if prev_hash != current_tip {
                    return Err(StateDbError::InvalidChain(format!(
                        "Broken chain for {:?}: expected prev {}, got {}",
                        author, current_tip, prev_hash
                    )));
                }
            }
            None => {
                // Genesis Check
                if prev_hash != Hash::ZERO {
                    return Err(StateDbError::InvalidChain(format!(
                        "Invalid genesis for {:?}: expected ZERO prev, got {}",
                        author, prev_hash
                    )));
                }
            }
        }

        // Update tip
        table_meta.insert(tip_key.as_slice(), op_id.as_slice())?;
        Ok(true)
    }

    /// Retrieve all applied chain tips (Author -> Last Hash).
    pub fn get_applied_chaintips(&self) -> Result<Vec<(PubKey, Hash)>, StateDbError> {
        let txn = self.db.begin_read()?;
        let table = match txn.open_table(TABLE_META) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(vec![]),
            Err(e) => return Err(e.into()),
        };

        let mut tips = Vec::new();
        // Scan all keys starting with "tip/"
        for entry in table.range(PREFIX_TIP..)? {
            let (k, v) = entry?;
            let k_bytes = k.value();

            // Safety check prefix
            if !k_bytes.starts_with(PREFIX_TIP) {
                break;
            }

            // Extract PubKey from key (strip "tip/")
            let author_bytes = &k_bytes[PREFIX_TIP.len()..];
            let author = PubKey::try_from(author_bytes)
                .map_err(|_| StateDbError::Conversion("Invalid author in meta".into()))?;

            let hash = Hash::try_from(v.value())
                .map_err(|_| StateDbError::Conversion("Invalid hash in meta".into()))?;

            tips.push((author, hash));
        }

        Ok(tips)
    }

    /// Helper for standard Lattice stores: snapshots 'data' table and 'meta' table.
    pub fn snapshot(&self, writer: &mut impl Write) -> Result<(), StateDbError> {
        let tables: TableMap = &[
            (SNAPSHOT_RECORD_DATA, TABLE_DATA.name()),
            (SNAPSHOT_RECORD_META, TABLE_META.name()),
            (SNAPSHOT_RECORD_SYSTEM, TABLE_SYSTEM.name()),
        ];
        self.snapshot_internal(tables, writer)
    }

    /// Helper for standard Lattice stores: restores 'data' table and 'meta' table.
    pub fn restore(&self, reader: &mut impl Read) -> Result<(), StateDbError> {
        let tables: TableMap = &[
            (SNAPSHOT_RECORD_DATA, TABLE_DATA.name()),
            (SNAPSHOT_RECORD_DATA, TABLE_DATA.name()),
            (SNAPSHOT_RECORD_META, TABLE_META.name()),
            (SNAPSHOT_RECORD_SYSTEM, TABLE_SYSTEM.name()),
        ];
        self.restore_internal(tables, reader)
    }

    /// Internal generic snapshot implementation
    fn snapshot_internal(
        &self,
        tables: TableMap,
        writer: &mut impl Write,
    ) -> Result<(), StateDbError> {
        let txn = self.db.begin_read()?;

        let mut hashing_writer = HashingWriter::new(writer);

        // Write Header
        hashing_writer.write_all(SNAPSHOT_MAGIC)?;
        hashing_writer.write_all(&SNAPSHOT_VERSION.to_le_bytes())?;
        hashing_writer.write_all(self.id.as_bytes())?;

        for (record_type, table_name) in tables {
            let def = TableDefinition::<&[u8], &[u8]>::new(table_name);
            // It's okay if a table doesn't exist, just skip it
            if let Ok(table) = txn.open_table(def) {
                for entry in table.iter()? {
                    let (k_access, v_access) = entry?;
                    let k = k_access.value();
                    let v = v_access.value();

                    // Record Type
                    hashing_writer.write_all(&[*record_type])?;

                    // Key Len + Key
                    hashing_writer.write_all(&(k.len() as u64).to_le_bytes())?;
                    hashing_writer.write_all(k)?;

                    // Val Len + Val
                    hashing_writer.write_all(&(v.len() as u64).to_le_bytes())?;
                    hashing_writer.write_all(v)?;
                }
            }
        }

        // EOF
        hashing_writer.write_all(&[SNAPSHOT_EOF])?;

        // Write Checksum (32 bytes)
        let checksum = hashing_writer.hasher.finalize();
        writer.write_all(checksum.as_bytes())?; // Write directly to inner writer

        Ok(())
    }

    /// Internal generic restore implementation
    fn restore_internal(
        &self,
        tables: TableMap,
        reader: &mut impl Read,
    ) -> Result<(), StateDbError> {
        let mut hashing_reader = HashingReader::new(reader);

        let write_txn = self.db.begin_write()?;

        // Clear existing tables
        for (_, table_name) in tables {
            let def = TableDefinition::<&[u8], &[u8]>::new(table_name);
            // Ignore error if table doesn't exist yet
            let _ = write_txn.delete_table(def);
            // Create it empty
            let _ = write_txn.open_table(def)?;
        }

        // Read Header (24 bytes: 4 Magic + 4 Version + 16 UUID)
        let mut header_buf = [0u8; 24];
        hashing_reader.read_exact(&mut header_buf)?;

        if &header_buf[0..4] != SNAPSHOT_MAGIC {
            return Err(StateDbError::InvalidSnapshot("Invalid magic bytes".into()));
        }

        let version = u32::from_le_bytes(header_buf[4..8].try_into().unwrap());
        if version != SNAPSHOT_VERSION {
            return Err(StateDbError::InvalidSnapshot(format!(
                "Unsupported version: {}",
                version
            )));
        }

        let store_id = Uuid::from_bytes(header_buf[8..24].try_into().unwrap());

        if store_id != self.id {
            return Err(StateDbError::StoreIdMismatch {
                expected: self.id,
                got: store_id,
            });
        }

        // Read Records
        let mut last_type = None;
        let mut current_table: Option<redb::Table<&[u8], &[u8]>> = None;

        // Reusable buffers to minimize allocation
        let mut k_buf = Vec::new();
        let mut v_buf = Vec::new();

        loop {
            let mut type_buf = [0u8; 1];
            if hashing_reader.read_exact(&mut type_buf).is_err() {
                break;
            }
            let record_type = type_buf[0];

            if record_type == SNAPSHOT_EOF {
                break;
            }

            // If type changed, switch table
            if last_type != Some(record_type) {
                // Drop previous table to release borrow on write_txn
                drop(current_table.take());

                let table_name = tables
                    .iter()
                    .find(|(t, _)| *t == record_type)
                    .map(|(_, name)| *name)
                    .ok_or(StateDbError::InvalidSnapshot(format!(
                        "Unknown record type: {}",
                        record_type
                    )))?;

                let def = TableDefinition::<&[u8], &[u8]>::new(table_name);
                current_table = Some(write_txn.open_table(def)?);
                last_type = Some(record_type);
            }

            // Read Key
            let mut len_buf = [0u8; 8];
            hashing_reader.read_exact(&mut len_buf)?;
            let k_len = u64::from_le_bytes(len_buf) as usize;

            // Resize buffer (only allocates if capacity is insufficient)
            k_buf.resize(k_len, 0);
            hashing_reader.read_exact(&mut k_buf)?;

            // Read Value
            hashing_reader.read_exact(&mut len_buf)?;
            let v_len = u64::from_le_bytes(len_buf) as usize;

            v_buf.resize(v_len, 0);
            hashing_reader.read_exact(&mut v_buf)?;

            if let Some(table) = current_table.as_mut() {
                table.insert(k_buf.as_slice(), v_buf.as_slice())?;
            }
        }
        // Explicitly drop table to release transaction borrow
        drop(current_table);

        // Verify Checksum
        let computed_checksum = hashing_reader.hasher.finalize();
        let mut expected_checksum = [0u8; 32];
        reader.read_exact(&mut expected_checksum)?; // Read from inner reader

        if computed_checksum.as_bytes() != &expected_checksum {
            return Err(StateDbError::InvalidSnapshot("Checksum mismatch".into()));
        }

        write_txn.commit()?;
        Ok(())
    }
}

// Snapshot Constants
pub const SNAPSHOT_MAGIC: &[u8; 4] = b"LATS";
pub const SNAPSHOT_VERSION: u32 = 1;
pub const SNAPSHOT_EOF: u8 = 0;
pub const SNAPSHOT_RECORD_DATA: u8 = 1; // Standard data table ID
pub const SNAPSHOT_RECORD_META: u8 = 2; // Standard meta table ID
pub const SNAPSHOT_RECORD_SYSTEM: u8 = 3; // Standard system table ID

/// Map a snapshot record type (u8) to a table name (str)
pub type TableMap<'a> = &'a [(u8, &'a str)];

/// Writer wrapper that calculates BLAKE3 hash
struct HashingWriter<'a, W: Write> {
    inner: &'a mut W,
    hasher: Hasher,
}

impl<'a, W: Write> HashingWriter<'a, W> {
    fn new(inner: &'a mut W) -> Self {
        Self {
            inner,
            hasher: Hasher::new(),
        }
    }
}

impl<'a, W: Write> Write for HashingWriter<'a, W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Reader wrapper that calculates BLAKE3 hash
struct HashingReader<'a, R: Read> {
    inner: &'a mut R,
    hasher: Hasher,
}

impl<'a, R: Read> HashingReader<'a, R> {
    fn new(inner: &'a mut R) -> Self {
        Self {
            inner,
            hasher: Hasher::new(),
        }
    }
}

impl<'a, R: Read> Read for HashingReader<'a, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }
}

// ==================== Composition Pattern ====================

use lattice_store_base::{FieldFormat, Introspectable};
use prost_reflect::DynamicMessage;
use std::collections::HashMap;

/// Core trait for Lattice state machines.
///
/// Provides the unified apply pattern: txn → verify → mutate → update identity → commit → notify
///
/// Implementors provide:
/// - `backend()` - access to storage backend
/// - `mutate()` - decode payload and apply mutations
/// - `notify()` - notify watchers of changes
pub trait StateLogic: Send + Sync {
    /// Store-specific notification data type.
    type Updates;

    /// Access the underlying backend.
    fn backend(&self) -> &StateBackend;

    /// Decode payload and apply mutations to the table.
    /// Returns notification data for watchers.
    fn mutate(
        &self,
        table: &mut redb::Table<&[u8], &[u8]>,
        op: &Op,
    ) -> Result<Self::Updates, StateDbError>;

    /// Notify watchers of changes.
    fn notify(&self, updates: Self::Updates);

    /// Apply an operation to the state.
    ///
    /// Default implementation: begin txn → verify chain → mutate → commit → notify
    fn apply(&self, op: &Op) -> Result<(), StateDbError> {
        let write_txn = self.backend().db().begin_write()?;

        // 0. Validate Chain Integrity (and idempotence)
        let should_apply =
            self.backend()
                .verify_and_update_tip(&write_txn, &op.author, op.id, op.prev_hash)?;
        if !should_apply {
            // Idempotent duplicate, just return success
            return Ok(());
        }

        // 1. Delegate to logic
        let mut table = write_txn.open_table(TABLE_DATA)?;
        let updates = self.mutate(&mut table, op)?;
        drop(table); // Release borrow

        write_txn.commit()?;

        // 2. Notify watchers (only if applied)
        self.notify(updates);

        Ok(())
    }
}

/// A composite StateMachine wrapper.
///
/// - Wraps T (e.g. KvState) which holds the Backend and implements Logic.
/// - Implements `StateMachine` by delegating:
///     - `apply` -> `logic.apply(op)`
///     - `snapshot`, `restore`, etc. -> `logic.backend().snapshot()`
/// - Derefs to T so consumers can call `store.get()` directly.
#[derive(Clone)]
pub struct PersistentState<T: StateLogic> {
    inner: T,
}

impl<T: StateLogic> PersistentState<T> {
    pub fn new(inner: T) -> Self {
        Self { inner }
    }

    pub fn inner(&self) -> &T {
        &self.inner
    }
}

// Deref to inner logic to expose methods like `get`, `scan`
impl<T: StateLogic> std::ops::Deref for PersistentState<T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

// PersistentState<T> must implement StateLogic for SystemLayer<PersistentState<T>> to work.
// It simplifies delegates to the inner logic.
impl<T: StateLogic> StateLogic for PersistentState<T> {
    type Updates = T::Updates;

    fn backend(&self) -> &StateBackend {
        self.inner.backend()
    }

    fn mutate(
        &self,
        table: &mut redb::Table<&[u8], &[u8]>,
        op: &Op,
    ) -> Result<Self::Updates, StateDbError> {
        self.inner.mutate(table, op)
    }

    fn notify(&self, updates: Self::Updates) {
        self.inner.notify(updates)
    }
}

impl<T: StateLogic> StateMachine for PersistentState<T> {
    type Error = StateDbError;

    fn apply(&self, op: &Op) -> Result<(), Self::Error> {
        self.inner.apply(op)
    }

    fn snapshot(&self) -> Result<Box<dyn Read + Send>, Self::Error> {
        let mut buffer = Vec::new();
        self.inner.backend().snapshot(&mut buffer)?;
        Ok(Box::new(std::io::Cursor::new(buffer)))
    }

    fn restore(&self, snapshot: Box<dyn Read + Send>) -> Result<(), Self::Error> {
        let mut reader = snapshot;
        self.inner.backend().restore(&mut reader)
    }

    fn applied_chaintips(&self) -> Result<Vec<(PubKey, Hash)>, Self::Error> {
        self.inner.backend().get_applied_chaintips()
    }

    fn store_meta(&self) -> StoreMeta {
        self.inner.backend().get_meta()
    }
}

// Delegate Introspectable if valid
impl<T: StateLogic + Introspectable> Introspectable for PersistentState<T> {
    fn service_descriptor(&self) -> prost_reflect::ServiceDescriptor {
        self.inner.service_descriptor()
    }

    fn decode_payload(
        &self,
        payload: &[u8],
    ) -> Result<DynamicMessage, Box<dyn std::error::Error + Send + Sync>> {
        self.inner.decode_payload(payload)
    }

    fn command_docs(&self) -> HashMap<String, String> {
        self.inner.command_docs()
    }

    fn field_formats(&self) -> HashMap<String, FieldFormat> {
        self.inner.field_formats()
    }

    fn matches_filter(&self, payload: &DynamicMessage, filter: &str) -> bool {
        self.inner.matches_filter(payload, filter)
    }

    fn summarize_payload(&self, payload: &DynamicMessage) -> Vec<lattice_model::SExpr> {
        self.inner.summarize_payload(payload)
    }
}

// NOTE: StreamReflectable delegation is now in the Handle-Less Pattern section below
// using StreamProvider instead of requiring T: StreamReflectable directly.

// =============================================================================
// Additional Trait Delegations for Handle-Less Pattern
// =============================================================================
//
// These delegations allow Store<PersistentState<S>> to satisfy StoreHandle
// requirements by forwarding trait impls from the inner state type.
//
// NOTE: CommandHandler is NOT needed here - there's a blanket impl in lattice-store-base
// for types that Deref to a CommandHandler. PersistentState<T>: Deref<Target = T> so
// when T: CommandHandler, PersistentState<T> is automatically CommandHandler too.

use lattice_model::StoreTypeProvider;
use lattice_store_base::{BoxByteStream, StreamError, StreamHandler, StreamProvider, Subscriber};
use std::future::Future;
use std::pin::Pin;

// PersistentState<T> implements StreamProvider by forwarding to inner type's handlers.
// Each handler uses the same forwarding function that looks up the inner handler by name.
impl<T: StateLogic + StreamProvider + Send + Sync + 'static> StreamProvider for PersistentState<T> {
    fn stream_handlers(&self) -> Vec<StreamHandler<Self>> {
        // Get descriptors from inner type and create handlers for each.
        self.inner
            .stream_handlers()
            .into_iter()
            .map(|h| StreamHandler {
                descriptor: h.descriptor.clone(),
                subscriber: Box::new(ForwardSubscriber {
                    stream_name: h.descriptor.name,
                }),
            })
            .collect()
    }
}

/// Subscriber that forwards subscriptions to the inner state by name lookup.
struct ForwardSubscriber {
    stream_name: String,
}

impl<T: StateLogic + StreamProvider + Send + Sync> Subscriber<PersistentState<T>>
    for ForwardSubscriber
{
    fn subscribe<'a>(
        &'a self,
        state: &'a PersistentState<T>,
        params: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = Result<BoxByteStream, StreamError>> + Send + 'a>> {
        let stream_name = self.stream_name.clone();

        Box::pin(async move {
            // Find the inner handler by name
            let handler = state
                .inner
                .stream_handlers()
                .into_iter()
                .find(|h| h.descriptor.name == stream_name)
                .ok_or_else(|| {
                    StreamError::NotFound(format!("Inner handler for {} not found", stream_name))
                })?;

            // Delegate subscription to the inner handler's subscriber
            handler.subscriber.subscribe(&state.inner, params).await
        })
    }
}

// Delegate StoreTypeProvider for store type identification
impl<T: StateLogic + StoreTypeProvider> StoreTypeProvider for PersistentState<T> {
    fn store_type() -> &'static str {
        T::store_type()
    }
}

/// Helper to standardize the "Open" ceremony for PersistentState.
///
/// Handles opening the StateBackend, creating the inner logic, and wrapping it in PersistentState.
pub fn setup_persistent_state<L: StateLogic + StoreTypeProvider>(
    id: Uuid,
    path: &Path,
    constructor: impl FnOnce(StateBackend) -> L,
) -> Result<PersistentState<L>, StateDbError> {
    let store_type = L::store_type();
    let backend = StateBackend::open(id, path, Some(store_type), 1)?;
    Ok(PersistentState::new(constructor(backend)))
}

/// Trait for constructing state logic from a backend.
/// Allows generic implementation of Openable for PersistentState<T>.
pub trait StateFactory: StateLogic {
    /// Create the state logic instance from an open backend.
    fn create(backend: StateBackend) -> Self;
}

// Generic implementation of Openable for any PersistentState<T> where T implements StateFactory + StoreTypeProvider.
// This solves the Orphan Rule violation by implementing it in the crate where PersistentState is defined.
impl<T: StateFactory + StoreTypeProvider + 'static> lattice_model::Openable for PersistentState<T> {
    fn open(id: Uuid, path: &Path) -> Result<Self, String> {
        setup_persistent_state(id, path, T::create).map_err(|e| e.to_string())
    }
}
