//! IntentionStore: manages `log.db` for the Weaver Protocol.
//!
//! Responsible for:
//! - Persisting signed intentions (TABLE_INTENTIONS: hash → proto bytes)
//! - Persisting witness records (TABLE_WITNESS: seq → proto bytes)
//! - Maintaining in-memory author_tips index (PubKey → latest Hash)
//!
//! The store is intentionally dumb: it accepts any validly-signed intention
//! and persists it. Authorization (peer membership) is the actor's job.
//! Chain tips are derived from `store_prev` linkage during index rebuild.

use std::collections::HashMap;
use std::path::Path;

use lattice_model::types::{Hash, PubKey};
use lattice_model::weaver::{FloatingIntention, Intention, SignedIntention};
use lattice_proto::weaver::{FloatingMeta, WitnessContent, WitnessRecord};
pub use lattice_model::weaver::WitnessEntry;
use super::witness::sign_witness;
use prost::Message;
use redb::{Database, MultimapTableDefinition, TableDefinition, ReadableTable, ReadableTableMetadata};
use thiserror::Error;
use uuid::Uuid;

/// Intentions table: blake3(borsh(intention)) → protobuf SignedIntention bytes
const TABLE_INTENTIONS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("intentions");

/// Witness table: monotonic u64 (big-endian) → protobuf WitnessRecord bytes
const TABLE_WITNESS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("witness");

/// Witness index: intention hash (32 bytes) → witness sequence number (8 bytes big-endian).
const TABLE_WITNESS_INDEX: TableDefinition<&[u8], &[u8]> = TableDefinition::new("witness_index");

/// Author tips table: PubKey (32 bytes) → latest intention Hash (32 bytes).
const TABLE_AUTHOR_TIPS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("author_tips");

/// Floating index: intention hash → protobuf FloatingMeta
const TABLE_FLOATING: TableDefinition<&[u8], &[u8]> = TableDefinition::new("floating");

/// Reverse index: store_prev hash → intention hash (multimap, multiple authors can share a prev)
const TABLE_FLOATING_BY_PREV: MultimapTableDefinition<&[u8], &[u8]> = MultimapTableDefinition::new("floating_by_prev");

/// Maximum number of hashes to return in `hashes_in_range` to prevent DoS.
const MAX_RANGE_HASHES: usize = 2048;

#[derive(Debug, Error)]
pub enum IntentionStoreError {
    #[error("database error: {0}")]
    Database(#[from] redb::DatabaseError),
    #[error("table error: {0}")]
    Table(#[from] redb::TableError),
    #[error("transaction error: {0}")]
    Transaction(#[from] redb::TransactionError),
    #[error("commit error: {0}")]
    Commit(#[from] redb::CommitError),
    #[error("storage error: {0}")]
    Storage(#[from] redb::StorageError),
    #[error("borsh decode error: {0}")]
    Borsh(#[from] borsh::io::Error),
    #[error("protobuf decode error: {0}")]
    Proto(#[from] prost::DecodeError),
    #[error("invalid intention data: {0}")]
    InvalidData(String),
    #[error("intention already witnessed: {0}")]
    AlreadyWitnessed(Hash),
}

pub struct IntentionStore {
    db: Database,
    
    /// The UUID of the store this IntentionStore belongs to.
    store_id: Uuid,

    /// In-memory index: author → hash of their latest intention in this store.
    /// Derived from `store_prev` linkage — only tracks heads of complete chains.
    author_tips: HashMap<PubKey, Hash>,

    /// Next witness sequence number (monotonically increasing).
    witness_seq: u64,

    /// blake3 hash of the last WitnessRecord.content bytes.
    /// Used to populate `prev_hash` in the next WitnessContent.
    /// `Hash::ZERO` when the witness log is empty (genesis sentinel).
    last_witness_hash: Hash,

    /// Key used to sign witness records.
    signing_key: ed25519_dalek::SigningKey,

    /// XOR fingerprint of all intention hashes in TABLE_INTENTIONS.
    /// Derived on startup, maintained incrementally on insert().
    table_fingerprint: Hash,
}

impl IntentionStore {
    /// Open or create a `log.db` at the given directory.
    ///
    /// Reads persisted metadata (author tips, witness_seq, last_witness_hash)
    /// from dedicated tables for O(1) startup. Falls back to a full witness
    /// log replay if the meta table is empty (first open after migration).
    pub fn open(
        store_dir: impl AsRef<Path>,
        store_id: Uuid,
        signing_key: &ed25519_dalek::SigningKey,
    ) -> Result<Self, IntentionStoreError> {
        let dir = store_dir.as_ref();
        std::fs::create_dir_all(dir)
            .map_err(|e| IntentionStoreError::InvalidData(format!("cannot create dir: {e}")))?;

        let db_path = dir.join("log.db");
        let db = Database::builder().create(&db_path)?;

        // Ensure all tables exist
        {
            let write_txn = db.begin_write()?;
            let _ = write_txn.open_table(TABLE_INTENTIONS)?;
            let _ = write_txn.open_table(TABLE_WITNESS)?;
            let _ = write_txn.open_table(TABLE_WITNESS_INDEX)?;
            let _ = write_txn.open_table(TABLE_AUTHOR_TIPS)?;
            let _ = write_txn.open_table(TABLE_FLOATING)?;
            let _ = write_txn.open_multimap_table(TABLE_FLOATING_BY_PREV)?;
            write_txn.commit()?;
        }

        let mut store = Self {
            db,
            store_id,
            author_tips: HashMap::new(),
            witness_seq: 0,
            last_witness_hash: Hash::ZERO,
            signing_key: signing_key.clone(),
            table_fingerprint: Hash::ZERO,
        };

        // Fast path: load author tips and derive witness state from tables
        let has_tips = {
            let read_txn = store.db.begin_read()?;
            let tips = read_txn.open_table(TABLE_AUTHOR_TIPS)?;
            tips.len()? > 0
        };

        if has_tips {
            store.load_persisted_state()?;
        } else {
            // First open or migration: replay witness log, persist author tips + witness index
            store.rebuild_indexes()?;
        }

        // Derive table fingerprint from all intention keys
        store.derive_table_fingerprint()?;

        Ok(store)
    }

    /// Decode a `SignedIntention` from its on-disk protobuf bytes.
    fn decode_signed(bytes: &[u8]) -> Result<SignedIntention, IntentionStoreError> {
        let proto = lattice_proto::weaver::SignedIntention::decode(bytes)?;
        let intention = Intention::from_borsh(&proto.intention_borsh)?;
        let sig_bytes: [u8; 64] = proto.signature.try_into()
            .map_err(|_| IntentionStoreError::InvalidData("bad signature length".into()))?;
        Ok(SignedIntention {
            intention,
            signature: lattice_model::types::Signature(sig_bytes),
        })
    }

    /// Load persisted state from TABLE_AUTHOR_TIPS and TABLE_WITNESS.
    /// Derives witness_seq and last_witness_hash from the last witness entry.
    /// O(N_authors) — avoids replaying the full witness log.
    fn load_persisted_state(&mut self) -> Result<(), IntentionStoreError> {
        let read_txn = self.db.begin_read()?;

        // Derive witness_seq and last_witness_hash from the last witness entry
        let witness_table = read_txn.open_table(TABLE_WITNESS)?;
        if let Some(entry) = witness_table.iter()?.rev().next() {
            let (k, v) = entry?;
            let mut buf = [0u8; 8];
            buf.copy_from_slice(k.value());
            self.witness_seq = u64::from_be_bytes(buf);

            let record = WitnessRecord::decode(v.value())
                .map_err(|e| IntentionStoreError::InvalidData(format!("witness proto: {e}")))?;
            self.last_witness_hash = Hash(*blake3::hash(&record.content).as_bytes());
        }

        let tips = read_txn.open_table(TABLE_AUTHOR_TIPS)?;
        for entry in tips.iter()? {
            let (k, v) = entry?;
            let pk = PubKey(k.value().try_into()
                .map_err(|_| IntentionStoreError::InvalidData("bad pubkey in author_tips".into()))?);
            let hash = Hash::try_from(v.value())
                .map_err(|_| IntentionStoreError::InvalidData("bad hash in author_tips".into()))?;
            self.author_tips.insert(pk, hash);
        }

        Ok(())
    }

    /// Rebuild all persisted indexes from the witness log.
    ///
    /// Used on first open after migration (when TABLE_META is empty).
    /// Replays the full witness log, verifies the hash chain, and persists
    /// author_tips, witness_seq, last_witness_hash, and witness_index.
    fn rebuild_indexes(&mut self) -> Result<(), IntentionStoreError> {
        let write_txn = self.db.begin_write()?;
        {
            let intention_table = write_txn.open_table(TABLE_INTENTIONS)?;
            let witness_table = write_txn.open_table(TABLE_WITNESS)?;
            let mut witness_index = write_txn.open_table(TABLE_WITNESS_INDEX)?;

            let mut max_seq = 0;
            let mut expected_prev = Hash::ZERO;

            for entry in witness_table.iter()? {
                let (k, v) = entry?;

                let mut buf = [0u8; 8];
                buf.copy_from_slice(k.value());
                let seq = u64::from_be_bytes(buf);
                if seq > max_seq {
                    max_seq = seq;
                }

                let record = WitnessRecord::decode(v.value())
                    .map_err(|e| IntentionStoreError::InvalidData(format!("witness proto: {e}")))?;
                let content = WitnessContent::decode(record.content.as_slice())
                    .map_err(|e| IntentionStoreError::InvalidData(format!("witness content: {e}")))?;

                // Verify hash chain
                let actual_prev = Hash::try_from(content.prev_hash.as_slice())
                    .map_err(|_| IntentionStoreError::InvalidData(format!(
                        "CORRUPTION: Witness seq {} has invalid prev_hash length ({})",
                        seq, content.prev_hash.len()
                    )))?;
                if actual_prev != expected_prev {
                    return Err(IntentionStoreError::InvalidData(format!(
                        "CORRUPTION: Witness chain broken at seq {}: prev_hash {} != expected {}",
                        seq, actual_prev, expected_prev,
                    )));
                }
                expected_prev = Hash(*blake3::hash(&record.content).as_bytes());

                let intention_hash = Hash::try_from(content.intention_hash.as_slice())
                    .map_err(|_| IntentionStoreError::InvalidData("bad intention_hash in witness".into()))?;

                let intent_val = intention_table.get(intention_hash.as_bytes().as_slice())?
                    .ok_or_else(|| IntentionStoreError::InvalidData(format!(
                        "CORRUPTION: Witness seq {} references intention {} which does not exist",
                        seq, intention_hash,
                    )))?;
                let signed = Self::decode_signed(intent_val.value())?;
                self.author_tips.insert(signed.intention.author, intention_hash);

                // Backfill witness index
                let seq_key = seq.to_be_bytes();
                witness_index.insert(intention_hash.as_bytes().as_slice(), seq_key.as_slice())?;
            }

            self.witness_seq = max_seq;
            self.last_witness_hash = expected_prev;

            // Persist author tips so future opens are O(1)

            let mut tips_table = write_txn.open_table(TABLE_AUTHOR_TIPS)?;
            for (pk, hash) in &self.author_tips {
                tips_table.insert(pk.0.as_slice(), hash.as_bytes().as_slice())?;
            }
        }
        write_txn.commit()?;

        Ok(())
    }

    /// Insert a signed intention into the store.
    ///
    /// Idempotent — inserting the same intention twice returns its hash
    /// without error. Does NOT validate linearity or authorization;
    /// those are the actor's responsibility.
    pub fn insert(&mut self, signed: &SignedIntention) -> Result<Hash, IntentionStoreError> {
        let intention = &signed.intention;
        let hash = intention.hash();

        let proto = lattice_proto::weaver::SignedIntention {
            intention_borsh: intention.to_borsh(),
            signature: signed.signature.0.to_vec(),
        };
        let proto_bytes = proto.encode_to_vec();

        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(TABLE_INTENTIONS)?;
            if table.get(hash.as_bytes().as_slice())?.is_some() {
                return Ok(hash); // idempotent
            }
            table.insert(hash.as_bytes().as_slice(), proto_bytes.as_slice())?;

            // Track as floating until witnessed
            let mut floating = write_txn.open_table(TABLE_FLOATING)?;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            let meta = FloatingMeta { received_at: now };
            let meta_bytes = meta.encode_to_vec();
            floating.insert(hash.as_bytes().as_slice(), meta_bytes.as_slice())?;

            let mut by_prev = write_txn.open_multimap_table(TABLE_FLOATING_BY_PREV)?;
            by_prev.insert(intention.store_prev.as_bytes().as_slice(), hash.as_bytes().as_slice())?;
        }
        write_txn.commit()?;

        // Update in-memory fingerprint
        self.xor_fingerprint(&hash);

        Ok(hash)
    }

    /// Retrieve a signed intention by its content hash.
    pub fn get(&self, hash: &Hash) -> Result<Option<SignedIntention>, IntentionStoreError> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(TABLE_INTENTIONS)?;

        match table.get(hash.as_bytes().as_slice())? {
            Some(v) => {
                Ok(Some(Self::decode_signed(v.value())?))
            }
            None => Ok(None),
        }
    }

    /// Encode a `WitnessContent` proto and return the raw bytes.
    fn encode_witness_content(intention_hash: &Hash, wall_time: u64, store_id: &Uuid, prev_hash: &Hash) -> Vec<u8> {
        let content = WitnessContent {
            intention_hash: intention_hash.as_bytes().to_vec(),
            wall_time,
            store_id: store_id.as_bytes().to_vec(),
            prev_hash: prev_hash.as_bytes().to_vec(),
        };
        content.encode_to_vec()
    }

    /// Record a witness entry for an intention.
    ///
    /// Returns `AlreadyWitnessed` if the intention has already been witnessed.
    pub fn witness(
        &mut self,
        intention: &Intention,
        wall_time: u64,
    ) -> Result<WitnessRecord, IntentionStoreError> {
        let hash = intention.hash();
        if self.is_witnessed(&hash)? {
            return Err(IntentionStoreError::AlreadyWitnessed(hash));
        }
        let content_bytes = Self::encode_witness_content(&hash, wall_time, &self.store_id, &self.last_witness_hash);
        let record = sign_witness(content_bytes.clone(), &self.signing_key);
        let proto_bytes = record.encode_to_vec();

        let seq = self.witness_seq + 1;
        let seq_key = seq.to_be_bytes();
        let new_witness_hash = Hash(*blake3::hash(&content_bytes).as_bytes());

        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(TABLE_WITNESS)?;
            table.insert(seq_key.as_slice(), proto_bytes.as_slice())?;

            // Update witness index
            let mut witness_idx = write_txn.open_table(TABLE_WITNESS_INDEX)?;
            witness_idx.insert(hash.as_bytes().as_slice(), seq_key.as_slice())?;


            let mut tips = write_txn.open_table(TABLE_AUTHOR_TIPS)?;
            tips.insert(intention.author.0.as_slice(), hash.as_bytes().as_slice())?;

            // Remove from floating indexes
            let mut floating = write_txn.open_table(TABLE_FLOATING)?;
            floating.remove(hash.as_bytes().as_slice())?;

            let mut by_prev = write_txn.open_multimap_table(TABLE_FLOATING_BY_PREV)?;
            by_prev.remove(intention.store_prev.as_bytes().as_slice(), hash.as_bytes().as_slice())?;
        }
        write_txn.commit()?;

        // Update in-memory cache after successful commit
        self.witness_seq = seq;
        self.last_witness_hash = new_witness_hash;
        self.author_tips.insert(intention.author, hash);

        Ok(record)
    }

    /// Get the current tip hash for an author, or `Hash::ZERO` if none.
    pub fn author_tip(&self, author: &PubKey) -> Hash {
        self.author_tips
            .get(author)
            .copied()
            .unwrap_or(Hash::ZERO)
    }

    /// Get all current author tips.
    pub fn all_author_tips(&self) -> &HashMap<PubKey, Hash> {
        &self.author_tips
    }

    /// Get all floating (unwitnessed) intentions with metadata.
    pub fn floating(&self) -> Result<Vec<FloatingIntention>, IntentionStoreError> {
        let read_txn = self.db.begin_read()?;
        let floating_table = read_txn.open_table(TABLE_FLOATING)?;
        let intention_table = read_txn.open_table(TABLE_INTENTIONS)?;

        let mut results = Vec::new();
        for entry in floating_table.iter()? {
            let (k, v) = entry?;
            let meta = FloatingMeta::decode(v.value())?;
            if let Some(iv) = intention_table.get(k.value())? {
                results.push(FloatingIntention {
                    signed: Self::decode_signed(iv.value())?,
                    received_at: meta.received_at,
                });
            }
        }
        Ok(results)
    }

    /// Look up all floating intentions whose `store_prev` matches the given hash.
    pub fn floating_by_prev(&self, prev: &Hash) -> Result<Vec<SignedIntention>, IntentionStoreError> {
        let read_txn = self.db.begin_read()?;
        let by_prev = read_txn.open_multimap_table(TABLE_FLOATING_BY_PREV)?;
        let intention_table = read_txn.open_table(TABLE_INTENTIONS)?;

        let mut results = Vec::new();
        let values = by_prev.get(prev.as_bytes().as_slice())?;
        for entry in values {
            let intention_hash = entry?;
            if let Some(iv) = intention_table.get(intention_hash.value())? {
                results.push(Self::decode_signed(iv.value())?);
            }
        }
        Ok(results)
    }

    /// Derive table fingerprint by XOR-scanning all TABLE_INTENTIONS keys.
    fn derive_table_fingerprint(&mut self) -> Result<(), IntentionStoreError> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(TABLE_INTENTIONS)?;
        let mut fp = [0u8; 32];
        for entry in table.iter()? {
            let (k, _) = entry?;
            for (i, byte) in k.value().iter().enumerate() {
                fp[i] ^= byte;
            }
        }
        self.table_fingerprint = Hash(fp);
        Ok(())
    }

    /// XOR a single hash into the running fingerprint.
    fn xor_fingerprint(&mut self, hash: &Hash) {
        for (i, byte) in hash.as_bytes().iter().enumerate() {
            self.table_fingerprint.0[i] ^= byte;
        }
    }

    /// XOR fingerprint of all stored intention hashes.
    /// Two nodes with identical fingerprints have identical intention sets.
    pub fn table_fingerprint(&self) -> Hash {
        self.table_fingerprint
    }

    /// Number of intentions in the store.
    pub fn intention_count(&self) -> Result<u64, IntentionStoreError> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(TABLE_INTENTIONS)?;
        Ok(table.len()?)
    }

    // --- Negentropy range query support ---

    /// Count intention hashes in the range [start, end).
    pub fn count_range(&self, start: &Hash, end: &Hash) -> Result<u64, IntentionStoreError> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(TABLE_INTENTIONS)?;
        let mut range = table.range(start.as_bytes().as_slice()..end.as_bytes().as_slice())?;
        range.try_fold(0u64, |acc, item| item.map(|_| acc + 1).map_err(IntentionStoreError::from))
    }

    /// XOR fingerprint of all intention hashes in the range [start, end).
    /// Returns `Hash::ZERO` for an empty range.
    pub fn fingerprint_range(&self, start: &Hash, end: &Hash) -> Result<Hash, IntentionStoreError> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(TABLE_INTENTIONS)?;
        let mut fp = [0u8; 32];
        for entry in table.range(start.as_bytes().as_slice()..end.as_bytes().as_slice())? {
            let (k, _) = entry?;
            let b = k.value();
            for (i, byte) in b.iter().enumerate() {
                fp[i] ^= byte;
            }
        }
        Ok(Hash(fp))
    }

    /// List all intention hashes in the range [start, end).
    /// Intended for small leaf ranges during Negentropy reconciliation.
    /// Returns an error if the range contains more than `MAX_RANGE_HASHES` items to prevent DoS.
    pub fn hashes_in_range(&self, start: &Hash, end: &Hash) -> Result<Vec<Hash>, IntentionStoreError> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(TABLE_INTENTIONS)?;
        let range = table.range(start.as_bytes().as_slice()..end.as_bytes().as_slice())?;
        
        let mut results = Vec::new();
        for (i, entry) in range.enumerate() {
            if i >= MAX_RANGE_HASHES {
                return Err(IntentionStoreError::InvalidData(format!(
                    "Range too large: >{} items. Use fingerprint_range/count_range first.",
                    MAX_RANGE_HASHES
                )));
            }
            let (k, _) = entry?;
            results.push(Hash::try_from(k.value())
                .map_err(|_| IntentionStoreError::InvalidData("bad hash key in intentions table".into()))?);
        }
        Ok(results)
    }

    /// Number of witness log entries in the store.
    pub fn witness_count(&self) -> Result<u64, IntentionStoreError> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(TABLE_WITNESS)?;
        Ok(table.len()?)
    }

    /// Iterate all witness records in sequence order.
    /// Returns (seq, content_hash, record) tuples.
    /// Get the entire witness log (WARNING: O(N) memory usage)
    pub fn witness_log(&self) -> Result<Vec<WitnessEntry>, IntentionStoreError> {
        self.scan_witness_log(None, usize::MAX)
    }

    /// Get the witness sequence number for a given intention hash.
    fn get_witness_seq(&self, hash: &Hash) -> Result<Option<u64>, IntentionStoreError> {
        let read_txn = self.db.begin_read()?;
        let index = read_txn.open_table(TABLE_WITNESS_INDEX)?;
        match index.get(hash.as_bytes().as_slice())? {
            Some(v) => Ok(Some(u64::from_be_bytes(v.value().try_into().map_err(|_| {
                IntentionStoreError::InvalidData("invalid witness seq length".into())
            })?))),
            None => Ok(None),
        }
    }

    /// Scan witness log from a given start point.
    /// 
    /// If `start_hash` is provided:
    /// - If `Hash::ZERO`, starts from genesis (seq 1).
    /// - If found, starts from the entry *after* it.
    /// - If not found, returns error.
    ///
    /// If `start_hash` is None, starts from genesis.
    pub fn scan_witness_log(
        &self,
        start_hash: Option<Hash>,
        limit: usize,
    ) -> Result<Vec<WitnessEntry>, IntentionStoreError> {
        let start_seq = match start_hash {
            Some(h) if h != Hash::ZERO => {
                 self.get_witness_seq(&h)?
                     .ok_or_else(|| IntentionStoreError::InvalidData(format!("witness hash {} not found", h)))? 
                     + 1 // Start scanning AFTER the known hash (inclusive start = seq + 1)
            },
            _ => 1, // Genesis starts at 1
        };

        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(TABLE_WITNESS)?;
        
        let mut results = Vec::new();
        let start_bytes = start_seq.to_be_bytes();
        
        // Use range iterator: start_seq..
        for res in table.range(start_bytes.as_slice()..)? {
            let (key, value) = res?;
            let seq = u64::from_be_bytes(key.value().try_into().map_err(|_| {
                IntentionStoreError::InvalidData("bad witness key".into())
            })?);
            let record = WitnessRecord::decode(value.value())
                .map_err(|e| IntentionStoreError::InvalidData(format!("proto: {e}")))?;
            let content_hash = Hash(*blake3::hash(&record.content).as_bytes());
            results.push(WitnessEntry {
                seq,
                content_hash,
                content: record.content,
                signature: record.signature,
            });
            
            if results.len() >= limit {
                break;
            }
        }
        Ok(results)
    }

    /// Check whether the store contains a given intention hash.
    pub fn contains(&self, hash: &Hash) -> Result<bool, IntentionStoreError> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(TABLE_INTENTIONS)?;
        Ok(table.get(hash.as_bytes().as_slice())?.is_some())
    }

    /// Check whether an intention has been witnessed (appears in the witness log).
    /// `Hash::ZERO` (genesis sentinel) is always considered witnessed.
    pub fn is_witnessed(&self, hash: &Hash) -> Result<bool, IntentionStoreError> {
        if *hash == Hash::ZERO {
            return Ok(true);
        }
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(TABLE_WITNESS_INDEX)?;
        Ok(table.get(hash.as_bytes().as_slice())?.is_some())
    }

    /// Find all intentions whose hash starts with the given prefix.
    ///
    /// Uses redb range queries for efficient lookup — does NOT scan the full table.
    pub fn get_by_prefix(&self, prefix: &[u8]) -> Result<Vec<SignedIntention>, IntentionStoreError> {
        // If prefix is exactly 32 bytes, do direct lookup
        if prefix.len() == 32 {
            let hash = Hash::try_from(prefix)
                .map_err(|_| IntentionStoreError::InvalidData("bad hash".into()))?;
            return Ok(self.get(&hash)?.into_iter().collect());
        }

        if prefix.is_empty() || prefix.len() > 32 {
            return Ok(Vec::new());
        }

        // Build range bounds: pad prefix with 0x00 for start, 0xFF for end
        let mut start = [0u8; 32];
        start[..prefix.len()].copy_from_slice(prefix);
        // start is already zero-padded

        let mut end = [0xFFu8; 32];
        end[..prefix.len()].copy_from_slice(prefix);

        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(TABLE_INTENTIONS)?;
        let mut results = Vec::new();

        for entry in table.range(start.as_slice()..=end.as_slice())? {
            let (_k, v) = entry?;
            results.push(Self::decode_signed(v.value())?);
        }

        Ok(results)
    }

    /// Walk back the chain of intentions from `target` until `since` is found (exclusive).
    ///
    /// Returns a vector of intentions in *reverse chronological order* (target first).
    /// If `since` is provided, stops when `store_prev` equals `since`.
    /// If `since` is None, walks back until Genesis (Hash::ZERO).
    ///
    /// Returns error if:
    /// - `target` is not found
    /// - `since` is provided but not found in the chain (gap too large or fork)
    /// - Logic limit exceeded (to prevent infinite loops)
    pub fn walk_back_until(
        &self,
        target: &Hash,
        since: Option<&Hash>,
        limit: usize,
    ) -> Result<Vec<SignedIntention>, IntentionStoreError> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(TABLE_INTENTIONS)?;

        // Pre-allocate memory to avoid resizing
        let mut results = Vec::with_capacity(limit);
        let mut current_hash = *target;

        // since is strictly required for gap filling
        let since_hash = since.ok_or_else(|| {
            IntentionStoreError::InvalidData("walk_back_until requires a 'since' hash".into())
        })?;

        for _ in 0..limit {
            match current_hash {
                // Success: Found the ancestor (exclusive)
                h if h == *since_hash => return Ok(results),
                
                // Failure: Hit Genesis without finding ancestor
                Hash::ZERO => return Err(IntentionStoreError::InvalidData(format!(
                    "Hit Genesis without finding ancestor {}", since_hash
                ))),
                
                // Continue: Fetch and advance
                hash => {
                    let val = table.get(hash.as_bytes().as_slice())? 
                        .ok_or_else(|| IntentionStoreError::InvalidData(
                            format!("Missing link in chain: {}", hash)
                        ))?;

                    let signed = Self::decode_signed(val.value())?;
                    
                    // Advance
                    current_hash = signed.intention.store_prev;
                    results.push(signed);
                }
            }
        }

        // Failure: Hit limit without finding ancestor
        Err(IntentionStoreError::InvalidData(format!(
            "Failed to find ancestor {} from target {} within limit {}",
            since_hash, target, limit
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::witness::verify_witness;
    use lattice_model::hlc::HLC;
    use lattice_model::weaver::Condition;

    fn make_key() -> (ed25519_dalek::SigningKey, PubKey) {
        let key = ed25519_dalek::SigningKey::from_bytes(&[42u8; 32]);
        let pk = PubKey(key.verifying_key().to_bytes());
        (key, pk)
    }

    fn make_intention(author: PubKey, store_prev: Hash, ops: Vec<u8>) -> Intention {
        Intention {
            author,
            timestamp: HLC::new(1000, 0),
            store_id: uuid::Uuid::from_bytes([0xAA; 16]),
            store_prev,
            condition: Condition::v1(vec![]),
            ops,
        }
    }

    #[test]
    fn insert_and_get() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open_store(dir.path());
        let (key, pk) = make_key();

        let intention = make_intention(pk, Hash::ZERO, vec![1, 2, 3]);
        let signed = SignedIntention::sign(intention, &key);
        let hash = store.insert(&signed).unwrap();

        let retrieved = store.get(&hash).unwrap().unwrap();
        assert_eq!(retrieved.intention, signed.intention);
        assert_eq!(retrieved.signature, signed.signature);
    }

    #[test]
    fn duplicate_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open_store(dir.path());
        let (key, pk) = make_key();

        let signed = SignedIntention::sign(make_intention(pk, Hash::ZERO, vec![1]), &key);
        let h1 = store.insert(&signed).unwrap();
        let h2 = store.insert(&signed).unwrap();
        assert_eq!(h1, h2);
        assert_eq!(store.intention_count().unwrap(), 1);
    }

    #[test]
    fn out_of_order_insertion() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open_store(dir.path());
        let (key, pk) = make_key();

        let i1 = make_intention(pk, Hash::ZERO, vec![1]);
        let s1 = SignedIntention::sign(i1.clone(), &key);

        let i2 = make_intention(pk, i1.hash(), vec![2]);
        let s2 = SignedIntention::sign(i2.clone(), &key);

        // Insert i2 first (out of order) — no tip yet (nothing witnessed)
        store.insert(&s2).unwrap();
        assert_eq!(store.author_tip(&pk), Hash::ZERO);

        // Insert i1
        store.insert(&s1).unwrap();
        assert_eq!(store.author_tip(&pk), Hash::ZERO);

        // Witness them — tip updates on witness
        store.witness(&i1, 100).unwrap();
        assert_eq!(store.author_tip(&pk), i1.hash());
        store.witness(&i2, 200).unwrap();
        assert_eq!(store.author_tip(&pk), i2.hash());

        assert_eq!(store.intention_count().unwrap(), 2);
    }

    #[test]
    fn author_tip_tracking() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open_store(dir.path());
        let (key, pk) = make_key();

        assert_eq!(store.author_tip(&pk), Hash::ZERO);

        let i1 = make_intention(pk, Hash::ZERO, vec![1]);
        let s1 = SignedIntention::sign(i1.clone(), &key);
        let h1 = store.insert(&s1).unwrap();
        // Tip not updated on insert — only on witness
        assert_eq!(store.author_tip(&pk), Hash::ZERO);

        store.witness(&i1, 100).unwrap();
        assert_eq!(store.author_tip(&pk), h1);

        let i2 = make_intention(pk, h1, vec![2]);
        let s2 = SignedIntention::sign(i2.clone(), &key);
        let h2 = store.insert(&s2).unwrap();
        // Still h1 until i2 is witnessed
        assert_eq!(store.author_tip(&pk), h1);
        store.witness(&i2, 200).unwrap();
        assert_eq!(store.author_tip(&pk), h2);
    }

    fn test_store_id() -> Uuid {
        Uuid::from_bytes([0xAA; 16])
    }

    fn test_signing_key() -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[0xBB; 32])
    }

    fn open_store(dir: &Path) -> IntentionStore {
        IntentionStore::open(dir, test_store_id(), &test_signing_key()).unwrap()
    }

    #[test]
    fn witness_record() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open_store(dir.path());
        let (key, pk) = make_key();

        let i = make_intention(pk, Hash::ZERO, vec![1, 2]);
        let h = i.hash();
        let s = SignedIntention::sign(i.clone(), &key);
        store.insert(&s).unwrap();
        
        let record = store.witness(&i, 1_700_000_000_000).unwrap();
        // Tip is set by insert, not witness
        assert_eq!(store.author_tip(&pk), h);
        
        let content = verify_witness(&record, &test_signing_key().verifying_key()).unwrap();
        assert_eq!(Uuid::from_slice(&content.store_id).unwrap(), test_store_id());
        // First witness in log: prev_hash must be all-zeros (genesis)
        assert_eq!(content.prev_hash, Hash::ZERO.as_bytes().to_vec());
    }

    #[test]
    fn witness_log_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open_store(dir.path());
        let (_key, pk) = make_key();

        let i = make_intention(pk, Hash::ZERO, vec![1]);
        let h = i.hash();
        store.witness(&i, 42_000).unwrap();

        let log = store.witness_log().unwrap();
        assert_eq!(log.len(), 1);
        let entry = &log[0];
        assert_eq!(entry.seq, 1);
        let record = WitnessRecord { content: entry.content.clone(), signature: entry.signature.clone() };
        let content = verify_witness(&record, &test_signing_key().verifying_key()).unwrap();
        assert_eq!(Hash::try_from(content.intention_hash.as_slice()).unwrap(), h);
        assert_eq!(content.wall_time, 42_000);
        assert_eq!(Uuid::from_slice(&content.store_id).unwrap(), test_store_id());
        assert_eq!(content.prev_hash, Hash::ZERO.as_bytes().to_vec());
    }

    #[test]
    fn witness_chain_integrity() {
        let dir = tempfile::tempdir().unwrap();
        let (key, pk) = make_key();

        {
            let mut store = open_store(dir.path());

            let i1 = make_intention(pk, Hash::ZERO, vec![1]);
            let s1 = SignedIntention::sign(i1.clone(), &key);
            store.insert(&s1).unwrap();
            store.witness(&i1, 100).unwrap();

            let i2 = make_intention(pk, i1.hash(), vec![2]);
            let s2 = SignedIntention::sign(i2.clone(), &key);
            store.insert(&s2).unwrap();
            store.witness(&i2, 200).unwrap();

            let i3 = make_intention(pk, i2.hash(), vec![3]);
            let s3 = SignedIntention::sign(i3.clone(), &key);
            store.insert(&s3).unwrap();
            store.witness(&i3, 300).unwrap();

            // Verify chain: each record's prev_hash == blake3(previous record.content)
            let log = store.witness_log().unwrap();
            assert_eq!(log.len(), 3);

            let c0 = WitnessContent::decode(log[0].content.as_slice()).unwrap();
            let c1 = WitnessContent::decode(log[1].content.as_slice()).unwrap();
            let c2 = WitnessContent::decode(log[2].content.as_slice()).unwrap();

            // First entry: prev_hash is genesis zero
            assert_eq!(c0.prev_hash, Hash::ZERO.as_bytes().to_vec());
            // Second entry: prev_hash == blake3(first record's content)
            assert_eq!(c1.prev_hash, blake3::hash(&log[0].content).as_bytes().to_vec());
            // Third entry: prev_hash == blake3(second record's content)
            assert_eq!(c2.prev_hash, blake3::hash(&log[1].content).as_bytes().to_vec());
        }

        // Reopen and verify chain verification passes + last_witness_hash is correct
        let mut store = open_store(dir.path());
        assert_eq!(store.witness_count().unwrap(), 3);

        // Can continue the chain after reopen
        let i4 = make_intention(pk, make_intention(pk, make_intention(pk, Hash::ZERO, vec![1]).hash(), vec![2]).hash(), vec![4]);
        let s4 = SignedIntention::sign(i4.clone(), &key);
        store.insert(&s4).unwrap();
        store.witness(&i4, 400).unwrap();

        // Verify the 4th entry chains to the 3rd
        let log = store.witness_log().unwrap();
        assert_eq!(log.len(), 4);
        let c3 = WitnessContent::decode(log[3].content.as_slice()).unwrap();
        assert_eq!(c3.prev_hash, blake3::hash(&log[2].content).as_bytes().to_vec());
    }


    #[test]
    fn rebuild_after_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let (key, pk) = make_key();

        let h2;
        {
            let mut store = open_store(dir.path());
            let i1 = make_intention(pk, Hash::ZERO, vec![1]);
            let s1 = SignedIntention::sign(i1.clone(), &key);
            let h1 = store.insert(&s1).unwrap();
            store.witness(&i1, 100).unwrap();

            let i2 = make_intention(pk, h1, vec![2]);
            let s2 = SignedIntention::sign(i2.clone(), &key);
            h2 = store.insert(&s2).unwrap();
            store.witness(&i2, 200).unwrap();
        }

        let mut store = open_store(dir.path());
        assert_eq!(store.author_tip(&pk), h2);
        assert_eq!(store.intention_count().unwrap(), 2);

        // Can continue the chain
        let i3 = make_intention(pk, h2, vec![3]);
        let s3 = SignedIntention::sign(i3.clone(), &key);
        let _h3 = store.insert(&s3).unwrap();
        store.witness(&i3, 300).unwrap();
        assert_eq!(store.intention_count().unwrap(), 3);
    }

    #[test]
    fn rebuild_tip_with_witness_gap() {
        // Regression test for rebuild_indexes consistency.
        //
        // The IntentionStore is "intentionally dumb" — witness() does not
        // enforce that store_prev has been witnessed first. A bug in the
        // application layer or partial data corruption can create a gap:
        //   witness_log = [A1, A3]  (A2 was never witnessed)
        //
        // Because A3's store_prev points to A2 (not A1), a topological
        // approach would see both A1 and A3 as "unreferenced" and
        // arbitrarily pick one — likely A1 since it appears first.
        //
        // The correct invariant is last-witnessed-wins: the witness log
        // is the authoritative timeline, so A3 must be the tip after
        // rebuild regardless of chain gaps.
        let dir = tempfile::tempdir().unwrap();
        let (key, pk) = make_key();

        let a1 = make_intention(pk, Hash::ZERO, vec![1]);
        let a2 = make_intention(pk, a1.hash(), vec![2]);
        let a3 = make_intention(pk, a2.hash(), vec![3]);

        {
            let mut store = open_store(dir.path());
            store.insert(&SignedIntention::sign(a1.clone(), &key)).unwrap();
            store.insert(&SignedIntention::sign(a2.clone(), &key)).unwrap();
            store.insert(&SignedIntention::sign(a3.clone(), &key)).unwrap();

            store.witness(&a1, 100).unwrap();
            assert_eq!(store.author_tip(&pk), a1.hash());

            // Skip A2, witness A3 directly
            store.witness(&a3, 300).unwrap();
            assert_eq!(store.author_tip(&pk), a3.hash());
        }

        // Reopen — rebuild_indexes must recover tip = A3
        let store = open_store(dir.path());
        assert_eq!(store.author_tip(&pk), a3.hash());
        assert!(store.is_witnessed(&a1.hash()).unwrap());
        assert!(!store.is_witnessed(&a2.hash()).unwrap());
        assert!(store.is_witnessed(&a3.hash()).unwrap());
    }

    fn make_intention_with_condition(
        author: PubKey,
        store_prev: Hash,
        ops: Vec<u8>,
        deps: Vec<Hash>,
    ) -> Intention {
        Intention {
            author,
            timestamp: HLC::new(1000, 0),
            store_id: uuid::Uuid::from_bytes([0xAA; 16]),
            store_prev,
            condition: Condition::v1(deps),
            ops,
        }
    }

    #[test]
    fn floating_blocked_by_missing_store_prev() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open_store(dir.path());
        let (key, pk) = make_key();

        // Intention whose store_prev doesn't exist in the store
        let bogus_prev = Hash([0xFF; 32]);
        let i = make_intention(pk, bogus_prev, vec![1]);
        let signed = SignedIntention::sign(i, &key);
        let hash = store.insert(&signed).unwrap();

        // Should be floating
        let floating = store.floating().unwrap();
        assert_eq!(floating.len(), 1);
        assert_eq!(floating[0].signed.intention.hash(), hash);

        // by_prev index should map bogus_prev → hash
        let by_prev = store.floating_by_prev(&bogus_prev).unwrap();
        assert_eq!(by_prev.len(), 1);
        assert_eq!(by_prev[0].intention.hash(), hash);

        // Nothing at Hash::ZERO
        assert!(store.floating_by_prev(&Hash::ZERO).unwrap().is_empty());

        // Not witnessed
        assert!(!store.is_witnessed(&hash).unwrap());
    }

    #[test]
    fn floating_blocked_by_missing_causal_dep() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open_store(dir.path());
        let (key, pk) = make_key();

        // Intention with a causal dep on a nonexistent hash
        let missing_dep = Hash([0xDD; 32]);
        let i = make_intention_with_condition(pk, Hash::ZERO, vec![1], vec![missing_dep]);
        let signed = SignedIntention::sign(i, &key);
        let hash = store.insert(&signed).unwrap();

        // Should be floating
        assert_eq!(store.floating().unwrap().len(), 1);

        // Causal dep is not witnessed
        assert!(!store.is_witnessed(&missing_dep).unwrap());

        // Hash::ZERO IS considered witnessed (genesis sentinel)
        assert!(store.is_witnessed(&Hash::ZERO).unwrap());

        // The intention itself is not witnessed
        assert!(!store.is_witnessed(&hash).unwrap());
    }

    #[test]
    fn floating_idempotent_insert() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open_store(dir.path());
        let (key, pk) = make_key();

        let i = make_intention(pk, Hash::ZERO, vec![1]);
        let signed = SignedIntention::sign(i, &key);

        let h1 = store.insert(&signed).unwrap();
        let h2 = store.insert(&signed).unwrap();
        assert_eq!(h1, h2);

        // Only one floating entry
        assert_eq!(store.floating().unwrap().len(), 1);

        // Only one entry in by_prev index
        let by_prev = store.floating_by_prev(&Hash::ZERO).unwrap();
        assert_eq!(by_prev.len(), 1);
    }

    #[test]
    fn floating_partial_chain_stays_blocked() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open_store(dir.path());
        let (key, pk) = make_key();

        // Build chain A → B → C but only insert B and C
        let ia = make_intention(pk, Hash::ZERO, vec![1]);
        let sa = SignedIntention::sign(ia.clone(), &key);
        let ha = ia.hash();

        let ib = make_intention(pk, ha, vec![2]);
        let sb = SignedIntention::sign(ib.clone(), &key);
        let hb = ib.hash();

        let ic = make_intention(pk, hb, vec![3]);
        let sc = SignedIntention::sign(ic.clone(), &key);
        let hc = ic.hash();

        // Insert B and C (A is missing)
        store.insert(&sb).unwrap();
        store.insert(&sc).unwrap();

        // Both floating
        assert_eq!(store.floating().unwrap().len(), 2);

        // B is at store_prev=ha, but A is missing from store
        let at_ha = store.floating_by_prev(&ha).unwrap();
        assert_eq!(at_ha.len(), 1);
        assert_eq!(at_ha[0].intention.hash(), hb);

        // C is at store_prev=hb
        let at_hb = store.floating_by_prev(&hb).unwrap();
        assert_eq!(at_hb.len(), 1);
        assert_eq!(at_hb[0].intention.hash(), hc);

        // Nothing at ZERO (A was never inserted)
        assert!(store.floating_by_prev(&Hash::ZERO).unwrap().is_empty());

        // Now insert A
        store.insert(&sa).unwrap();
        assert_eq!(store.floating().unwrap().len(), 3);

        // A is findable at ZERO
        let at_zero = store.floating_by_prev(&Hash::ZERO).unwrap();
        assert_eq!(at_zero.len(), 1);
        assert_eq!(at_zero[0].intention.hash(), ha);

        // Witness A — removes it from floating, B becomes next in line
        store.witness(&ia, 100).unwrap();
        assert!(store.is_witnessed(&ha).unwrap());
        assert_eq!(store.floating().unwrap().len(), 2);
        assert!(store.floating_by_prev(&Hash::ZERO).unwrap().is_empty());

        // B is still at store_prev=ha
        let at_ha = store.floating_by_prev(&ha).unwrap();
        assert_eq!(at_ha.len(), 1);

        // Witness B
        store.witness(&ib, 200).unwrap();
        assert!(store.is_witnessed(&hb).unwrap());
        assert_eq!(store.floating().unwrap().len(), 1);

        // C is the only one left
        let remaining = store.floating().unwrap();
        assert_eq!(remaining[0].signed.intention.hash(), hc);
    }

    #[test]
    fn multi_author_complex_dependencies() {
        // Two authors: Alice and Bob.
        // Alice: A1 → A2 → A3
        // Bob:   B1 → B2
        // B2 has a causal dep on A2 (cross-author dependency).
        //
        // Witness order: A1, B1, A2, B2, A3
        // Verifies: per-author tips, floating transitions, witness log
        // ordering, and hash chain integrity.

        let dir = tempfile::tempdir().unwrap();
        let mut store = open_store(dir.path());

        let alice_key = ed25519_dalek::SigningKey::from_bytes(&[0x11; 32]);
        let alice = PubKey(alice_key.verifying_key().to_bytes());

        let bob_key = ed25519_dalek::SigningKey::from_bytes(&[0x22; 32]);
        let bob = PubKey(bob_key.verifying_key().to_bytes());

        // Build all intentions
        let a1 = make_intention(alice, Hash::ZERO, vec![0xA1]);
        let a2 = make_intention(alice, a1.hash(), vec![0xA2]);
        let a3 = make_intention(alice, a2.hash(), vec![0xA3]);

        let b1 = make_intention(bob, Hash::ZERO, vec![0xB1]);
        let b2 = make_intention_with_condition(bob, b1.hash(), vec![0xB2], vec![a2.hash()]);

        // Insert all (order shouldn't matter for correctness)
        store.insert(&SignedIntention::sign(a1.clone(), &alice_key)).unwrap();
        store.insert(&SignedIntention::sign(a2.clone(), &alice_key)).unwrap();
        store.insert(&SignedIntention::sign(a3.clone(), &alice_key)).unwrap();
        store.insert(&SignedIntention::sign(b1.clone(), &bob_key)).unwrap();
        store.insert(&SignedIntention::sign(b2.clone(), &bob_key)).unwrap();

        // All 5 should be floating
        assert_eq!(store.floating().unwrap().len(), 5);
        assert_eq!(store.author_tip(&alice), Hash::ZERO);
        assert_eq!(store.author_tip(&bob), Hash::ZERO);

        // Witness A1
        store.witness(&a1, 100).unwrap();
        assert!(store.is_witnessed(&a1.hash()).unwrap());
        assert_eq!(store.author_tip(&alice), a1.hash());
        assert_eq!(store.floating().unwrap().len(), 4);

        // Witness B1
        store.witness(&b1, 200).unwrap();
        assert!(store.is_witnessed(&b1.hash()).unwrap());
        assert_eq!(store.author_tip(&bob), b1.hash());
        assert_eq!(store.floating().unwrap().len(), 3);

        // Witness A2 — this satisfies B2's causal dep
        store.witness(&a2, 300).unwrap();
        assert_eq!(store.author_tip(&alice), a2.hash());
        assert_eq!(store.floating().unwrap().len(), 2);

        // Witness B2 — its causal dep (A2) is now witnessed
        store.witness(&b2, 400).unwrap();
        assert_eq!(store.author_tip(&bob), b2.hash());
        assert_eq!(store.floating().unwrap().len(), 1);

        // Witness A3
        store.witness(&a3, 500).unwrap();
        assert_eq!(store.author_tip(&alice), a3.hash());
        assert_eq!(store.floating().unwrap().len(), 0);

        // Verify witness log: 5 entries in witness order
        let log = store.witness_log().unwrap();
        assert_eq!(log.len(), 5);
        for (i, entry) in log.iter().enumerate() {
            assert_eq!(entry.seq, (i + 1) as u64);
        }

        // Verify hash chain integrity
        let c0 = WitnessContent::decode(log[0].content.as_slice()).unwrap();
        assert_eq!(c0.prev_hash, Hash::ZERO.as_bytes().to_vec());

        for i in 1..5 {
            let c = WitnessContent::decode(log[i].content.as_slice()).unwrap();
            assert_eq!(c.prev_hash, blake3::hash(&log[i - 1].content).as_bytes().to_vec());
        }

        // Verify the witness entries reference the correct intentions
        let intention_hashes: Vec<Hash> = log.iter().map(|e| {
            let c = WitnessContent::decode(e.content.as_slice()).unwrap();
            Hash::try_from(c.intention_hash.as_slice()).unwrap()
        }).collect();
        assert_eq!(intention_hashes, vec![a1.hash(), b1.hash(), a2.hash(), b2.hash(), a3.hash()]);

        // Verify total intention count
        assert_eq!(store.intention_count().unwrap(), 5);
    }

    #[test]
    fn witness_chain_tamper_detected() {
        // Witness 3 items, close, corrupt W2's content in the DB,
        // then assert that reopen detects the broken hash chain.
        let dir = tempfile::tempdir().unwrap();
        let (key, pk) = make_key();

        {
            let mut store = open_store(dir.path());
            let i1 = make_intention(pk, Hash::ZERO, vec![1]);
            let s1 = SignedIntention::sign(i1.clone(), &key);
            store.insert(&s1).unwrap();
            store.witness(&i1, 100).unwrap();

            let i2 = make_intention(pk, i1.hash(), vec![2]);
            let s2 = SignedIntention::sign(i2.clone(), &key);
            store.insert(&s2).unwrap();
            store.witness(&i2, 200).unwrap();

            let i3 = make_intention(pk, i2.hash(), vec![3]);
            let s3 = SignedIntention::sign(i3.clone(), &key);
            store.insert(&s3).unwrap();
            store.witness(&i3, 300).unwrap();
        }

        // Corrupt W2: overwrite its value with garbage bytes,
        // and clear author tips to force a rebuild that catches it.
        {
            let db = redb::Database::open(dir.path().join("log.db")).unwrap();

            // Corrupt W2
            let write_txn = db.begin_write().unwrap();
            {
                let mut table = write_txn.open_table(TABLE_WITNESS).unwrap();
                let seq2_key = 2u64.to_be_bytes();
                table.insert(seq2_key.as_slice(), b"CORRUPTED".as_slice()).unwrap();
            }
            write_txn.commit().unwrap();

            // Clear author tips (separate txn to avoid borrow conflicts)
            let read_txn = db.begin_read().unwrap();
            let tips = read_txn.open_table(TABLE_AUTHOR_TIPS).unwrap();
            let keys: Vec<Vec<u8>> = tips.iter().unwrap()
                .map(|e| { let (k, _) = e.unwrap(); k.value().to_vec() })
                .collect();
            drop(tips);
            drop(read_txn);

            let write_txn = db.begin_write().unwrap();
            {
                let mut tips = write_txn.open_table(TABLE_AUTHOR_TIPS).unwrap();
                for k in &keys {
                    tips.remove(k.as_slice()).unwrap();
                }
            }
            write_txn.commit().unwrap();
        }

        // Reopen — should fail with chain broken error
        let result = IntentionStore::open(dir.path(), test_store_id(), &test_signing_key());
        match result {
            Ok(_) => panic!("Store should reject corrupted witness chain"),
            Err(e) => {
                let err = e.to_string();
                assert!(
                    err.contains("CORRUPTION") || err.contains("witness"),
                    "Error should mention corruption: {err}"
                );
            }
        }
    }

    #[test]
    fn extreme_wall_time_values() {
        // The store is "dumb" and should accept any wall_time without
        // panicking or overflowing, including boundary values.
        let dir = tempfile::tempdir().unwrap();
        let (key, pk) = make_key();
        let mut store = open_store(dir.path());

        let i1 = make_intention(pk, Hash::ZERO, vec![1]);
        store.insert(&SignedIntention::sign(i1.clone(), &key)).unwrap();
        store.witness(&i1, u64::MAX).unwrap();

        let i2 = make_intention(pk, i1.hash(), vec![2]);
        store.insert(&SignedIntention::sign(i2.clone(), &key)).unwrap();
        store.witness(&i2, 0).unwrap();

        assert_eq!(store.author_tip(&pk), i2.hash());
        assert_eq!(store.intention_count().unwrap(), 2);

        // Verify the values round-trip through the witness log
        let log = store.witness_log().unwrap();
        assert_eq!(log.len(), 2);
        let c1 = WitnessContent::decode(log[0].content.as_slice()).unwrap();
        let c2 = WitnessContent::decode(log[1].content.as_slice()).unwrap();
        assert_eq!(c1.wall_time, u64::MAX);
        assert_eq!(c2.wall_time, 0);
    }

    #[test]
    fn double_witness_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let (key, pk) = make_key();
        let mut store = open_store(dir.path());

        let i1 = make_intention(pk, Hash::ZERO, vec![1]);
        store.insert(&SignedIntention::sign(i1.clone(), &key)).unwrap();
        store.witness(&i1, 100).unwrap();

        // Second witness of same intention should fail
        let err = store.witness(&i1, 200).unwrap_err();
        assert!(
            err.to_string().contains("already witnessed"),
            "Expected AlreadyWitnessed, got: {err}"
        );

        // Log should have exactly one entry
        assert_eq!(store.witness_log().unwrap().len(), 1);
    }

    #[test]
    fn range_queries_empty_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());

        let lo = Hash([0x00; 32]);
        let hi = Hash([0xFF; 32]);

        assert_eq!(store.count_range(&lo, &hi).unwrap(), 0);
        assert_eq!(store.fingerprint_range(&lo, &hi).unwrap(), Hash::ZERO);
        assert!(store.hashes_in_range(&lo, &hi).unwrap().is_empty());
    }

    #[test]
    fn range_queries_full_range() {
        let dir = tempfile::tempdir().unwrap();
        let (key, pk) = make_key();
        let mut store = open_store(dir.path());

        // Insert three intentions
        let i1 = make_intention(pk, Hash::ZERO, vec![1]);
        let s1 = SignedIntention::sign(i1.clone(), &key);
        store.insert(&s1).unwrap();

        let i2 = make_intention(pk, i1.hash(), vec![2]);
        let s2 = SignedIntention::sign(i2.clone(), &key);
        store.insert(&s2).unwrap();

        let i3 = make_intention(pk, i2.hash(), vec![3]);
        let s3 = SignedIntention::sign(i3.clone(), &key);
        store.insert(&s3).unwrap();

        let lo = Hash([0x00; 32]);
        let hi = Hash([0xFF; 32]);

        // Count covers everything
        assert_eq!(store.count_range(&lo, &hi).unwrap(), 3);

        // hashes_in_range returns all three, sorted
        let hashes = store.hashes_in_range(&lo, &hi).unwrap();
        assert_eq!(hashes.len(), 3);
        // Verify sorted order
        for w in hashes.windows(2) {
            assert!(w[0].as_bytes() < w[1].as_bytes(), "hashes should be sorted");
        }

        // All three hashes are present
        let expected: std::collections::HashSet<Hash> = [i1.hash(), i2.hash(), i3.hash()].into();
        let actual: std::collections::HashSet<Hash> = hashes.into_iter().collect();
        assert_eq!(actual, expected);

        // Fingerprint is XOR of all three
        let mut expected_fp = [0u8; 32];
        for h in &[i1.hash(), i2.hash(), i3.hash()] {
            for i in 0..32 {
                expected_fp[i] ^= h.as_bytes()[i];
            }
        }
        assert_eq!(store.fingerprint_range(&lo, &hi).unwrap(), Hash(expected_fp));
    }

    #[test]
    fn range_queries_partial_range() {
        let dir = tempfile::tempdir().unwrap();
        let (key, pk) = make_key();
        let mut store = open_store(dir.path());

        // Insert several intentions
        let i1 = make_intention(pk, Hash::ZERO, vec![1]);
        store.insert(&SignedIntention::sign(i1.clone(), &key)).unwrap();
        let i2 = make_intention(pk, i1.hash(), vec![2]);
        store.insert(&SignedIntention::sign(i2.clone(), &key)).unwrap();
        let i3 = make_intention(pk, i2.hash(), vec![3]);
        store.insert(&SignedIntention::sign(i3.clone(), &key)).unwrap();

        // Collect and sort all hashes
        let lo = Hash([0x00; 32]);
        let hi = Hash([0xFF; 32]);
        let all = store.hashes_in_range(&lo, &hi).unwrap();
        assert_eq!(all.len(), 3);

        // Use the middle hash as a boundary: [lo, mid) should exclude mid and above
        let mid = all[1];
        let below = store.hashes_in_range(&lo, &mid).unwrap();
        assert_eq!(below.len(), 1);
        assert_eq!(below[0], all[0]);

        // [mid, hi) should include mid and above
        let above = store.hashes_in_range(&mid, &hi).unwrap();
        assert_eq!(above.len(), 2);
        assert_eq!(above[0], all[1]);
        assert_eq!(above[1], all[2]);

        // Counts should match
        assert_eq!(store.count_range(&lo, &mid).unwrap(), 1);
        assert_eq!(store.count_range(&mid, &hi).unwrap(), 2);

        // Fingerprints should partition correctly:
        // xor(below) XOR xor(above) == xor(all)
        let fp_below = store.fingerprint_range(&lo, &mid).unwrap();
        let fp_above = store.fingerprint_range(&mid, &hi).unwrap();
        let fp_all = store.fingerprint_range(&lo, &hi).unwrap();
        let mut recombined = [0u8; 32];
        for i in 0..32 {
            recombined[i] = fp_below.as_bytes()[i] ^ fp_above.as_bytes()[i];
        }
        assert_eq!(Hash(recombined), fp_all, "XOR fingerprints should be additively composable");

        // Gap probe: boundary key that doesn't exist but falls between items
        let mut probe_bytes = all[0].0;
        probe_bytes[31] = probe_bytes[31].wrapping_add(1);
        let probe = Hash(probe_bytes);
        assert!(!all.contains(&probe), "probe should not collide with any stored hash");

        // [lo, probe) should contain only the first hash
        assert_eq!(store.count_range(&lo, &probe).unwrap(), 1);
        assert_eq!(store.hashes_in_range(&lo, &probe).unwrap(), vec![all[0]]);

        // [probe, hi) should contain the remaining two
        assert_eq!(store.count_range(&probe, &hi).unwrap(), 2);
        assert_eq!(store.hashes_in_range(&probe, &hi).unwrap(), vec![all[1], all[2]]);
    }

    #[test]
    fn range_queries_zero_width() {
        let dir = tempfile::tempdir().unwrap();
        let (key, pk) = make_key();
        let mut store = open_store(dir.path());

        let i1 = make_intention(pk, Hash::ZERO, vec![1]);
        store.insert(&SignedIntention::sign(i1.clone(), &key)).unwrap();

        let h = i1.hash();

        // [h, h) is an empty set
        assert_eq!(store.count_range(&h, &h).unwrap(), 0);
        assert!(store.hashes_in_range(&h, &h).unwrap().is_empty());
        assert_eq!(store.fingerprint_range(&h, &h).unwrap(), Hash::ZERO);
    }

    #[test]
    fn range_queries_inverted() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());

        let lo = Hash([0x00; 32]);
        let hi = Hash([0xFF; 32]);

        // start > end → empty
        assert_eq!(store.count_range(&hi, &lo).unwrap(), 0);
        assert!(store.hashes_in_range(&hi, &lo).unwrap().is_empty());
        assert_eq!(store.fingerprint_range(&hi, &lo).unwrap(), Hash::ZERO);
    }

    // --- table_fingerprint tests ---

    #[test]
    fn table_fingerprint_empty_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());
        assert_eq!(store.table_fingerprint(), Hash::ZERO);
    }

    #[test]
    fn table_fingerprint_incremental() {
        let dir = tempfile::tempdir().unwrap();
        let (key, pk) = make_key();
        let mut store = open_store(dir.path());

        // Insert first intention
        let i1 = make_intention(pk, Hash::ZERO, vec![1]);
        store.insert(&SignedIntention::sign(i1.clone(), &key)).unwrap();
        assert_eq!(store.table_fingerprint(), i1.hash());

        // Insert second — fingerprint is XOR of both
        let i2 = make_intention(pk, i1.hash(), vec![2]);
        store.insert(&SignedIntention::sign(i2.clone(), &key)).unwrap();

        let mut expected = [0u8; 32];
        for (i, byte) in i1.hash().as_bytes().iter().enumerate() {
            expected[i] ^= byte;
        }
        for (i, byte) in i2.hash().as_bytes().iter().enumerate() {
            expected[i] ^= byte;
        }
        assert_eq!(store.table_fingerprint(), Hash(expected));
    }

    #[test]
    fn table_fingerprint_duplicate_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let (key, pk) = make_key();
        let mut store = open_store(dir.path());

        let i1 = make_intention(pk, Hash::ZERO, vec![1]);
        let signed = SignedIntention::sign(i1.clone(), &key);
        store.insert(&signed).unwrap();
        let fp_after_first = store.table_fingerprint();

        // Duplicate insert should not change the fingerprint
        store.insert(&signed).unwrap();
        assert_eq!(store.table_fingerprint(), fp_after_first);
    }

    #[test]
    fn table_fingerprint_consistent_with_range() {
        let dir = tempfile::tempdir().unwrap();
        let (key, pk) = make_key();
        let mut store = open_store(dir.path());

        let i1 = make_intention(pk, Hash::ZERO, vec![1]);
        store.insert(&SignedIntention::sign(i1.clone(), &key)).unwrap();
        let i2 = make_intention(pk, i1.hash(), vec![2]);
        store.insert(&SignedIntention::sign(i2.clone(), &key)).unwrap();

        let lo = Hash([0x00; 32]);
        let hi = Hash([0xFF; 32]);
        let range_fp = store.fingerprint_range(&lo, &hi).unwrap();
        assert_eq!(store.table_fingerprint(), range_fp);
    }

    #[test]
    fn table_fingerprint_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let (key, pk) = make_key();

        let fp_before;
        {
            let mut store = open_store(dir.path());
            let i1 = make_intention(pk, Hash::ZERO, vec![1]);
            store.insert(&SignedIntention::sign(i1.clone(), &key)).unwrap();
            store.witness(&i1, 100).unwrap();
            let i2 = make_intention(pk, i1.hash(), vec![2]);
            store.insert(&SignedIntention::sign(i2.clone(), &key)).unwrap();
            // i2 is floating (not witnessed) — fingerprint still covers both
            fp_before = store.table_fingerprint();
            assert_ne!(fp_before, Hash::ZERO);
        }

        // Reopen — fingerprint is re-derived from TABLE_INTENTIONS
        let store = open_store(dir.path());
        assert_eq!(store.table_fingerprint(), fp_before);
    }

    #[test]
    fn table_fingerprint_divergent_stores() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let (key, pk) = make_key();

        let mut store_a = open_store(dir_a.path());
        let mut store_b = open_store(dir_b.path());

        // Same first intention
        let i1 = make_intention(pk, Hash::ZERO, vec![1]);
        let signed1 = SignedIntention::sign(i1.clone(), &key);
        store_a.insert(&signed1).unwrap();
        store_b.insert(&signed1).unwrap();
        assert_eq!(store_a.table_fingerprint(), store_b.table_fingerprint());

        // Diverge: only store_a gets i2
        let i2 = make_intention(pk, i1.hash(), vec![2]);
        store_a.insert(&SignedIntention::sign(i2.clone(), &key)).unwrap();
        assert_ne!(store_a.table_fingerprint(), store_b.table_fingerprint());

        // Converge: store_b also gets i2
        store_b.insert(&SignedIntention::sign(i2.clone(), &key)).unwrap();
        assert_eq!(store_a.table_fingerprint(), store_b.table_fingerprint());
    }
}
