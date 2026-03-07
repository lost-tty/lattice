use blake3::Hasher;
use lattice_model::{DagQueries, Hash, Op, PubKey, StorageConfig, StoreMeta};
use redb::{Database, ReadableTable, TableDefinition, TableHandle};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;
use thiserror::Error;
use tracing::warn;
use uuid::Uuid;

// Standard Table Definitions
pub const TABLE_DATA: TableDefinition<&[u8], &[u8]> = TableDefinition::new("data");
pub const TABLE_META: TableDefinition<&[u8], &[u8]> = TableDefinition::new("meta");
pub const TABLE_SYSTEM: TableDefinition<&[u8], &[u8]> = TableDefinition::new("system");

pub const KEY_STORE_ID: &[u8] = b"store_id";
pub const KEY_STORE_TYPE: &[u8] = b"store_type";
pub const KEY_SCHEMA_VERSION: &[u8] = b"schema_version";
pub const PREFIX_TIP: &[u8] = b"tip/";
pub const KEY_LAST_APPLIED_WITNESS: &[u8] = b"last_applied_witness";

// ==================== Scoped Database Access ====================

/// Read-only handle scoped to a single redb table.
///
/// Domain crates receive this instead of `Arc<Database>` so they can only
/// read their own table (e.g. `TABLE_DATA`), not `TABLE_META` or `TABLE_SYSTEM`.
#[derive(Clone)]
pub struct ScopedDb {
    db: Arc<Database>,
    table: TableDefinition<'static, &'static [u8], &'static [u8]>,
}

impl ScopedDb {
    pub fn new(
        db: Arc<Database>,
        table: TableDefinition<'static, &'static [u8], &'static [u8]>,
    ) -> Self {
        Self { db, table }
    }

    /// Open a read-only transaction scoped to this table.
    pub fn begin_read(&self) -> Result<ScopedReadTxn, StateDbError> {
        let txn = self.db.begin_read()?;
        Ok(ScopedReadTxn {
            txn,
            table: self.table,
        })
    }
}

/// A read transaction that can only open the scoped table.
pub struct ScopedReadTxn {
    txn: redb::ReadTransaction,
    table: TableDefinition<'static, &'static [u8], &'static [u8]>,
}

impl ScopedReadTxn {
    /// Open the scoped table. Returns `None` if the table doesn't exist yet.
    pub fn open_table(
        &self,
    ) -> Result<Option<redb::ReadOnlyTable<&'static [u8], &'static [u8]>>, StateDbError> {
        match self.txn.open_table(self.table) {
            Ok(t) => Ok(Some(t)),
            Err(redb::TableError::TableDoesNotExist(_)) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

/// Wrapper for the 5 distinct redb error types.
///
/// Redb doesn't provide a unified error enum. Callers never need to distinguish
/// between database, table, transaction, storage, or commit errors — they're all
/// "the embedded DB failed." This wrapper collapses them into a single variant
/// for use in higher-level error enums.
#[derive(Debug, Error)]
pub enum RedbError {
    #[error("{0}")]
    Database(#[from] redb::DatabaseError),
    #[error("{0}")]
    Table(#[from] redb::TableError),
    #[error("{0}")]
    Transaction(#[from] redb::TransactionError),
    #[error("{0}")]
    Storage(#[from] redb::StorageError),
    #[error("{0}")]
    Commit(#[from] redb::CommitError),
}

#[derive(Debug, Error)]
pub enum StateDbError {
    #[error("Redb error: {0}")]
    Redb(#[from] RedbError),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Store ID mismatch: expected {expected}, got {got}")]
    StoreIdMismatch { expected: Uuid, got: Uuid },
    #[error("Store Type mismatch: expected {expected}, got {got}")]
    StoreTypeMismatch { expected: String, got: String },
    #[error("Invalid chain: {0}")]
    InvalidChain(#[from] ChainError),
    #[error("Conversion error: {0}")]
    Conversion(String),
    #[error("Decode error: {0}")]
    Decode(#[from] prost::DecodeError),
    #[error("Invalid snapshot: {0}")]
    InvalidSnapshot(#[from] SnapshotError),
}

// Allow `?` on individual redb error types to convert through RedbError → StateDbError.
impl From<redb::DatabaseError> for StateDbError {
    fn from(e: redb::DatabaseError) -> Self {
        StateDbError::Redb(e.into())
    }
}
impl From<redb::TableError> for StateDbError {
    fn from(e: redb::TableError) -> Self {
        StateDbError::Redb(e.into())
    }
}
impl From<redb::TransactionError> for StateDbError {
    fn from(e: redb::TransactionError) -> Self {
        StateDbError::Redb(e.into())
    }
}
impl From<redb::StorageError> for StateDbError {
    fn from(e: redb::StorageError) -> Self {
        StateDbError::Redb(e.into())
    }
}
impl From<redb::CommitError> for StateDbError {
    fn from(e: redb::CommitError) -> Self {
        StateDbError::Redb(e.into())
    }
}

/// Chain integrity errors detected during apply.
#[derive(Debug, Error)]
pub enum ChainError {
    #[error("Broken chain for {author}: expected prev {expected}, got {got}")]
    BrokenChain {
        author: PubKey,
        expected: Hash,
        got: Hash,
    },
    #[error("Invalid genesis for {author}: expected ZERO prev, got {prev}")]
    InvalidGenesis { author: PubKey, prev: Hash },
    #[error("Duplicate timestamp for author")]
    DuplicateTimestamp,
}

/// Snapshot format errors detected during restore.
#[derive(Debug, Error)]
pub enum SnapshotError {
    #[error("Invalid magic bytes")]
    InvalidMagic,
    #[error("Unsupported version: {0}")]
    UnsupportedVersion(u32),
    #[error("Unknown record type: {0}")]
    UnknownRecordType(u8),
    #[error("Checksum mismatch")]
    ChecksumMismatch,
}

/// A composite backend that handles standard storage boilerplate for Lattice state machines.
///
/// It owns the `redb::Database` and the `store_id`, and provides standard methods for:
/// - Opening/Creating the store with verification.
/// - Validating chain integrity (tips).
/// - Managing state identity (rolling hash).
#[derive(Clone)]
pub struct StateBackend {
    db: Arc<Database>,
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
            Err(e) => {
                warn!(store_id = %self.id, error = %e, "Failed to begin read transaction for store meta");
                return StoreMeta {
                    store_id: self.id,
                    ..Default::default()
                };
            }
        };
        let table = match read_txn.open_table(TABLE_META) {
            Ok(t) => t,
            Err(e) => {
                warn!(store_id = %self.id, error = %e, "Failed to open meta table for store meta");
                return StoreMeta {
                    store_id: self.id,
                    ..Default::default()
                };
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

    /// Open or create a store with the given configuration.
    pub fn open(
        id: Uuid,
        config: &StorageConfig,
        expected_type: Option<&str>,
        expected_version: u64,
    ) -> Result<Self, StateDbError> {
        let db = match config {
            StorageConfig::File(dir) => {
                if !dir.exists() {
                    std::fs::create_dir_all(dir)?;
                }
                let state_db_path = dir.join("state.db");
                Database::builder().create(&state_db_path)?
            }
            StorageConfig::InMemory => {
                Database::builder().create_with_backend(redb::backends::InMemoryBackend::new())?
            }
        };
        Self::init(db, id, expected_type, expected_version)
    }

    /// Shared init: verify/write store metadata in TABLE_META.
    fn init(
        db: Database,
        id: Uuid,
        expected_type: Option<&str>,
        expected_version: u64,
    ) -> Result<Self, StateDbError> {
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

        Ok(Self {
            db: Arc::new(db),
            id,
        })
    }

    /// Access the underlying Redb database.
    pub fn db(&self) -> &Database {
        &self.db
    }

    /// Get a shared handle to the underlying database.
    ///
    /// Domain crates use this to hold a reference for their own read
    /// transactions without depending on `StateBackend` ownership.
    pub fn db_shared(&self) -> Arc<Database> {
        Arc::clone(&self.db)
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
                    return Err(ChainError::BrokenChain {
                        author: *author,
                        expected: current_tip,
                        got: prev_hash,
                    }
                    .into());
                }
            }
            None => {
                // Genesis Check
                if prev_hash != Hash::ZERO {
                    return Err(ChainError::InvalidGenesis {
                        author: *author,
                        prev: prev_hash,
                    }
                    .into());
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

    /// Get the projection cursor — the content hash of the last witness entry
    /// that was successfully projected onto state.
    ///
    /// Returns `Hash::ZERO` if no entries have been projected yet.
    pub fn last_applied_witness(&self) -> Result<Hash, StateDbError> {
        let txn = self.db.begin_read()?;
        let table = match txn.open_table(TABLE_META) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Hash::ZERO),
            Err(e) => return Err(e.into()),
        };
        match table.get(KEY_LAST_APPLIED_WITNESS)? {
            Some(v) => Hash::try_from(v.value())
                .map_err(|_| StateDbError::Conversion("Invalid projection cursor hash".into())),
            None => Ok(Hash::ZERO),
        }
    }

    /// Advance the projection cursor after a successful state apply.
    pub fn set_last_applied_witness(&self, hash: &Hash) -> Result<(), StateDbError> {
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(TABLE_META)?;
            table.insert(KEY_LAST_APPLIED_WITNESS, hash.as_slice())?;
        }
        txn.commit()?;
        Ok(())
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
            return Err(SnapshotError::InvalidMagic.into());
        }

        let version = u32::from_le_bytes(header_buf[4..8].try_into().unwrap());
        if version != SNAPSHOT_VERSION {
            return Err(SnapshotError::UnsupportedVersion(version).into());
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
                    .ok_or(SnapshotError::UnknownRecordType(record_type))?;

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
            return Err(SnapshotError::ChecksumMismatch.into());
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

/// Domain crate interface for state machines.
///
/// `SystemLayer` owns the write transaction and calls `apply()` with a
/// pre-opened `&mut redb::Table` scoped to the domain crate's table.
/// Domain crates decode the payload, update the table, and return
/// notification data for watchers.
pub trait StateLogic: Send + Sync {
    type Updates;

    /// Construct the state machine from a scoped database handle.
    fn create(db: ScopedDb) -> Self
    where
        Self: Sized;

    /// Decode payload and apply mutations to the table.
    ///
    /// Called by `SystemLayer` inside an open write transaction.
    /// Returning `Err` stalls the author's chain.
    fn apply(
        &self,
        table: &mut redb::Table<&[u8], &[u8]>,
        op: &Op,
        dag: &dyn DagQueries,
    ) -> Result<Self::Updates, StateDbError>;

    /// Notify watchers of changes after the transaction commits.
    fn notify(&self, updates: Self::Updates);
}
