use redb::{Database, TableDefinition, ReadableTable, TableHandle};
use std::path::Path;
use std::io::{Read, Write};
use uuid::Uuid;
use lattice_model::{Hash, PubKey, StateMachine, Op};
use thiserror::Error;
use blake3::Hasher;

// Standard Table Definitions
pub const TABLE_DATA: TableDefinition<&[u8], &[u8]> = TableDefinition::new("data");
pub const TABLE_META: TableDefinition<&[u8], &[u8]> = TableDefinition::new("meta");

// Meta Keys
pub const KEY_STORE_ID: &[u8] = b"store_id";
pub const KEY_STATE_HASH: &[u8] = b"state_hash";
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
    /// Open or create a store at the given path.
    /// 
    /// Performs standard cleanup (renaming log.db -> state.db) and verifies the store ID.
    pub fn open(id: Uuid, state_dir: impl AsRef<Path>) -> Result<Self, StateDbError> {
        let dir = state_dir.as_ref();
        if !dir.exists() {
            std::fs::create_dir_all(dir)?;
        }
    
        // Legacy Migration: Rename log.db to state.db if needed (standardize on state.db)
        let log_db_path = dir.join("log.db");
        let state_db_path = dir.join("state.db");
        
        if log_db_path.exists() && !state_db_path.exists() {
            std::fs::rename(&log_db_path, &state_db_path)?;
        }
    
        let db = Database::builder().create(&state_db_path)?;
        
        // Legacy Migration: Handle table renaming ("kv" or "log" -> "data")
        let write_txn = db.begin_write()?;
        {
            let mut has_kv = false;
            let mut has_log = false;
            let mut has_data = false;

            for table in write_txn.list_tables()? {
                match table.name() {
                    "kv" => has_kv = true,
                    "log" => has_log = true,
                    "data" => has_data = true,
                    _ => {}
                }
            }

            if has_kv && has_log {
                panic!("Invalid Storage State: Both 'kv' and 'log' tables exist. Store type is ambiguous.");
            }

            if (has_kv || has_log) && has_data {
                panic!("Invalid Storage State: Legacy table ('kv' or 'log') exists alongside new 'data' table. Partial migration detected.");
            }

            if has_kv {
                 let legacy_def = TableDefinition::<&[u8], &[u8]>::new("kv");
                 write_txn.rename_table(legacy_def, TABLE_DATA)?;
            } else if has_log {
                 let legacy_def = TableDefinition::<&[u8], &[u8]>::new("log");
                 write_txn.rename_table(legacy_def, TABLE_DATA)?;
            }
        }
        write_txn.commit()?;
        
        // Verify Store ID
        let write_txn = db.begin_write()?;
        {
            let mut table_meta = write_txn.open_table(TABLE_META)?;
            
            let existing_id = table_meta.get(KEY_STORE_ID)?
                .map(|v| Uuid::from_bytes(v.value().try_into().unwrap_or_default()));
                
            if let Some(existing_id) = existing_id {
                if existing_id != id {
                    return Err(StateDbError::StoreIdMismatch { expected: id, got: existing_id });
                }
            } else {
                table_meta.insert(KEY_STORE_ID, id.as_bytes().as_slice())?;
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
        prev_hash: Hash
    ) -> Result<bool, StateDbError> {
        let mut table_meta = txn.open_table(TABLE_META)?;
        let tip_key = [PREFIX_TIP, author.as_slice()].concat();
        let current_tip_bytes = table_meta.get(tip_key.as_slice())?.map(|v| v.value().to_vec());
        
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
            },
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

    /// Update global state identity using XOR rolling hash.
    pub fn update_state_identity(
        &self,
        txn: &redb::WriteTransaction,
        identity_diff: Hash
    ) -> Result<Hash, StateDbError> {
        let mut table_meta = txn.open_table(TABLE_META)?;
        
        // 1. Fetch current hash or default to zero
        let current_hash = table_meta.get(KEY_STATE_HASH)?
            .and_then(|v| <[u8; 32]>::try_from(v.value()).ok())
            .map(Hash::from)
            .unwrap_or(Hash::ZERO);
            
        // 2. XOR current with diff
        let new_hash = Hash::from(std::array::from_fn(|i| current_hash[i] ^ identity_diff[i]));
        
        // 3. Persist
        table_meta.insert(KEY_STATE_HASH, new_hash.as_slice())?;
        
        Ok(new_hash)
    }

    /// Retrieve the current state identity hash.
    pub fn get_state_identity(&self) -> Hash {
        let txn = match self.db.begin_read() {
            Ok(t) => t,
            Err(_) => return Hash::ZERO,
        };
        
        let table = match txn.open_table(TABLE_META) {
            Ok(t) => t,
            Err(_) => return Hash::ZERO,
        };
        
        match table.get(KEY_STATE_HASH) {
            Ok(Some(v)) => Hash::try_from(v.value()).unwrap_or(Hash::ZERO),
            _ => Hash::ZERO,
        }
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
            if !k_bytes.starts_with(PREFIX_TIP) { break; }
            
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
            (SNAPSHOT_RECORD_META, TABLE_META.name())
        ];
        self.snapshot_internal(tables, writer)
    }

    /// Helper for standard Lattice stores: restores 'data' table and 'meta' table.
    pub fn restore(&self, reader: &mut impl Read) -> Result<(), StateDbError> {
        let tables: TableMap = &[
            (SNAPSHOT_RECORD_DATA, TABLE_DATA.name()),
            (SNAPSHOT_RECORD_META, TABLE_META.name())
        ];
        self.restore_internal(tables, reader)
    }

    /// Internal generic snapshot implementation
    fn snapshot_internal(&self, tables: TableMap, writer: &mut impl Write) -> Result<(), StateDbError> {
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
    fn restore_internal(&self, tables: TableMap, reader: &mut impl Read) -> Result<(), StateDbError> {
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
            return Err(StateDbError::InvalidSnapshot(format!("Unsupported version: {}", version)));
        }
        
        let store_id = Uuid::from_bytes(header_buf[8..24].try_into().unwrap());
        
        if store_id != self.id {
            return Err(StateDbError::StoreIdMismatch { expected: self.id, got: store_id });
        }
        
        // Read Records
        let mut last_type = None;
        let mut current_table: Option<redb::Table<&[u8], &[u8]>> = None;
        
        // Reusable buffers to minimize allocation
        let mut k_buf = Vec::new();
        let mut v_buf = Vec::new();

        loop {
            let mut type_buf = [0u8; 1];
            if hashing_reader.read_exact(&mut type_buf).is_err() { break; } 
            let record_type = type_buf[0];
            
            if record_type == SNAPSHOT_EOF { break; }
            
            // If type changed, switch table
            if last_type != Some(record_type) {
                // Drop previous table to release borrow on write_txn
                drop(current_table.take());
                
                let table_name = tables.iter()
                    .find(|(t, _)| *t == record_type)
                    .map(|(_, name)| *name)
                    .ok_or(StateDbError::InvalidSnapshot(format!("Unknown record type: {}", record_type)))?;
                    
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

/// Helper to XOR two 32-byte arrays
pub fn xor_hash(a: [u8; 32], b: [u8; 32]) -> [u8; 32] {
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = a[i] ^ b[i];
    }
    out
}
// ==================== Composition Pattern ====================

use lattice_model::{Introspectable, StreamReflectable, StreamDescriptor};
use prost_reflect::DynamicMessage;
use std::collections::HashMap;

/// Logic trait for implementing state machine operations over a storage backend.
/// 
/// Types implementing this trait (e.g. KvState, LogState) provide the domain-specific
/// `apply` logic, while `Store<T>` handles the infrastructure (snapshot/restore).
pub trait StateLogic: Send + Sync {
    /// Access the underlying backend.
    fn backend(&self) -> &StateBackend;
    
    /// Apply an operation to the state.
    fn apply(&self, op: &Op) -> Result<(), StateDbError>;
}

/// A composite StateMachine wrapper.
/// 
/// - Wraps T (e.g. KvState) which holds the Backend and implements Logic.
/// - Implements `StateMachine` by delegating:
///     - `apply` -> `logic.apply(op)`
///     - `snapshot`, `restore`, etc. -> `logic.backend().snapshot()`
/// - Derefs to T so consumers can call `store.get()` directly.
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

    fn state_identity(&self) -> Hash {
        self.inner.backend().get_state_identity()
    }

    fn applied_chaintips(&self) -> Result<Vec<(PubKey, Hash)>, Self::Error> {
        self.inner.backend().get_applied_chaintips()
    }
}

// ==================== StateHasher Strategy ====================

/// Helper for calculating Rolling Hashes (XOR-based Merkle-like root replacement).
/// 
/// This unifies the identity calculation across KvStore (key-value hash) and LogStore (OpID).
pub struct StateHasher {
    accumulator: [u8; 32],
}

impl Default for StateHasher {
    fn default() -> Self {
        Self { accumulator: [0u8; 32] }
    }
}

impl StateHasher {
    pub fn new() -> Self {
        Self::default()
    }

    /// XORs the accumulator with a hash.
    /// Order-independent: update(A) then update(B) == update(B) then update(A).
    pub fn update(&mut self, hash: impl AsRef<[u8]>) {
        for (a, h) in self.accumulator.iter_mut().zip(hash.as_ref()) {
            *a ^= h;
        }
    }

    /// Update using a raw byte slice (hashed first).
    pub fn update_with_bytes(&mut self, data: &[u8]) {
        self.update(Self::hash_bytes(data));
    }

    /// Return the final rolling hash.
    pub fn finish(self) -> Hash {
        Hash::from(self.accumulator)
    }

    /// Utility: Hash arbitrary bytes using Blake3.
    pub fn hash_bytes(data: &[u8]) -> Hash {
        Hash::from(*blake3::hash(data).as_bytes())
    }
}

// Delegate Introspectable if valid
impl<T: StateLogic + Introspectable> Introspectable for PersistentState<T> {
    fn service_descriptor(&self) -> prost_reflect::ServiceDescriptor {
        self.inner.service_descriptor()
    }

    fn decode_payload(&self, payload: &[u8]) -> Result<DynamicMessage, Box<dyn std::error::Error + Send + Sync>> {
        self.inner.decode_payload(payload)
    }

    fn command_docs(&self) -> HashMap<String, String> {
        self.inner.command_docs()
    }

    fn field_formats(&self) -> HashMap<String, lattice_model::introspection::FieldFormat> {
        self.inner.field_formats()
    }

    fn matches_filter(&self, payload: &DynamicMessage, filter: &str) -> bool {
        self.inner.matches_filter(payload, filter)
    }

    fn summarize_payload(&self, payload: &DynamicMessage) -> Vec<String> {
        self.inner.summarize_payload(payload)
    }
}

// Delegate StreamReflectable if valid
impl<T: StateLogic + StreamReflectable> StreamReflectable for PersistentState<T> {
    fn stream_descriptors(&self) -> Vec<StreamDescriptor> {
        self.inner.stream_descriptors()
    }
}

/// Helper to standardize the "Open" ceremony for PersistentState.
/// 
/// Handles opening the StateBackend, creating the inner logic, and wrapping it in PersistentState.
pub fn setup_persistent_state<L: StateLogic>(
    id: Uuid, 
    path: &Path, 
    constructor: impl FnOnce(StateBackend) -> L
) -> Result<PersistentState<L>, StateDbError> {
    let backend = StateBackend::open(id, path)?;
    Ok(PersistentState::new(constructor(backend)))
}

/// Trait for constructing state logic from a backend.
/// Allows generic implementation of Openable for PersistentState<T>.
pub trait StateFactory: StateLogic {
    /// Create the state logic instance from an open backend.
    fn create(backend: StateBackend) -> Self;
}

// Generic implementation of Openable for any PersistentState<T> where T implements StateFactory.
// This solves the Orphan Rule violation by implementing it in the crate where PersistentState is defined.
impl<T: StateFactory + 'static> lattice_model::Openable for PersistentState<T> {
    fn open(id: Uuid, path: &Path) -> Result<Self, String> {
        setup_persistent_state(id, path, T::create)
            .map_err(|e| e.to_string())
    }
}
