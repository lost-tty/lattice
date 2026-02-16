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
use std::sync::atomic::{AtomicU64, Ordering};

use lattice_model::types::{Hash, PubKey};
use lattice_model::weaver::{Intention, SignedIntention};
use lattice_proto::weaver::{WitnessContent, WitnessRecord};
use super::witness::sign_witness;
use prost::Message;
use redb::{Database, TableDefinition, ReadableTable, ReadableTableMetadata};
use thiserror::Error;
use uuid::Uuid;

/// Intentions table: blake3(borsh(intention)) → protobuf SignedIntention bytes
const TABLE_INTENTIONS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("intentions");

/// Witness table: monotonic u64 (big-endian) → protobuf WitnessRecord bytes
const TABLE_WITNESS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("witness");

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
}

pub struct IntentionStore {
    db: Database,
    /// The UUID of the store this IntentionStore belongs to.
    store_id: Uuid,
    /// In-memory index: author → hash of their latest intention in this store.
    /// Derived from `store_prev` linkage — only tracks heads of complete chains.
    author_tips: std::sync::RwLock<HashMap<PubKey, Hash>>,
    /// Next witness sequence number (monotonically increasing).
    witness_seq: AtomicU64,
}

impl IntentionStore {
    /// Open or create a `log.db` at the given directory.
    ///
    /// On open, rebuilds the in-memory `author_tips` index by scanning
    /// the intentions table.
    pub fn open(
        store_dir: impl AsRef<Path>,
        store_id: Uuid,
    ) -> Result<Self, IntentionStoreError> {
        let dir = store_dir.as_ref();
        std::fs::create_dir_all(dir)
            .map_err(|e| IntentionStoreError::InvalidData(format!("cannot create dir: {e}")))?;

        let db_path = dir.join("log.db");
        let db = Database::builder().create(&db_path)?;

        {
            let write_txn = db.begin_write()?;
            let _ = write_txn.open_table(TABLE_INTENTIONS)?;
            let _ = write_txn.open_table(TABLE_WITNESS)?;
            write_txn.commit()?;
        }

        let store = Self {
            db,
            store_id,
            author_tips: std::sync::RwLock::new(HashMap::new()),
            witness_seq: AtomicU64::new(0),
        };

        store.rebuild_indexes()?;
        Ok(store)
    }

    /// Rebuild the in-memory `author_tips` index and `witness_seq` from disk.
    fn rebuild_indexes(&self) -> Result<(), IntentionStoreError> {
        let read_txn = self.db.begin_read()?;

        // Single pass: collect all hashes per author and all referenced store_prevs.
        let table = read_txn.open_table(TABLE_INTENTIONS)?;
        let mut per_author: HashMap<PubKey, Vec<Hash>> = HashMap::new();
        let mut referenced: std::collections::HashSet<Hash> = std::collections::HashSet::new();

        for entry in table.iter()? {
            let (k, v) = entry?;
            let hash = Hash::try_from(k.value())
                .map_err(|_| IntentionStoreError::InvalidData("bad hash key".into()))?;
            let proto = lattice_proto::weaver::SignedIntention::decode(v.value())?;
            let intention = Intention::from_borsh(&proto.intention_borsh)?;

            per_author.entry(intention.author).or_default().push(hash);
            if intention.store_prev != Hash::ZERO {
                referenced.insert(intention.store_prev);
            }
        }

        // The tip for each author is the hash that no other intention references.
        let mut tips: HashMap<PubKey, Hash> = HashMap::new();
        for (author, hashes) in &per_author {
            for h in hashes {
                if !referenced.contains(h) {
                    tips.insert(*author, *h);
                    break;
                }
            }
        }
        *self.author_tips.write().unwrap() = tips;

        // Witness seq: highest key in the witness table.
        let witness_table = read_txn.open_table(TABLE_WITNESS)?;
        let max_seq = witness_table
            .iter()?
            .last()
            .transpose()?
            .map(|(k, _)| {
                let mut buf = [0u8; 8];
                buf.copy_from_slice(k.value());
                u64::from_be_bytes(buf)
            })
            .unwrap_or(0);
        self.witness_seq.store(max_seq, Ordering::Relaxed);

        Ok(())
    }

    /// Insert a signed intention into the store.
    ///
    /// Idempotent — inserting the same intention twice returns its hash
    /// without error. Does NOT validate linearity or authorization;
    /// those are the actor's responsibility.
    pub fn insert(&self, signed: &SignedIntention) -> Result<Hash, IntentionStoreError> {
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
        }
        write_txn.commit()?;

        // Rebuild tips — simpler than incremental updates when out-of-order
        // insertions can change who the tip is.
        self.rebuild_indexes()?;

        Ok(hash)
    }

    /// Retrieve a signed intention by its content hash.
    pub fn get(&self, hash: &Hash) -> Result<Option<SignedIntention>, IntentionStoreError> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(TABLE_INTENTIONS)?;

        match table.get(hash.as_bytes().as_slice())? {
            Some(v) => {
                let proto = lattice_proto::weaver::SignedIntention::decode(v.value())?;
                let intention = Intention::from_borsh(&proto.intention_borsh)?;
                let sig_bytes: [u8; 64] = proto.signature.try_into()
                    .map_err(|_| IntentionStoreError::InvalidData("bad signature length".into()))?;
                Ok(Some(SignedIntention {
                    intention,
                    signature: lattice_model::types::Signature(sig_bytes),
                }))
            }
            None => Ok(None),
        }
    }

    /// Encode a `WitnessContent` proto and return the raw bytes.
    fn encode_witness_content(intention_hash: &Hash, wall_time: u64, store_id: &Uuid) -> Vec<u8> {
        let content = WitnessContent {
            intention_hash: intention_hash.as_bytes().to_vec(),
            wall_time,
            store_id: store_id.as_bytes().to_vec(),
        };
        content.encode_to_vec()
    }

    /// Record a witness entry for an intention.
    pub fn witness(
        &self,
        intention_hash: Hash,
        wall_time: u64,
        signing_key: &ed25519_dalek::SigningKey,
    ) -> Result<WitnessRecord, IntentionStoreError> {
        let content_bytes = Self::encode_witness_content(&intention_hash, wall_time, &self.store_id);
        let record = sign_witness(content_bytes, signing_key);
        let proto_bytes = record.encode_to_vec();

        let seq = self.witness_seq.fetch_add(1, Ordering::Relaxed) + 1;
        let seq_key = seq.to_be_bytes();

        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(TABLE_WITNESS)?;
            table.insert(seq_key.as_slice(), proto_bytes.as_slice())?;
        }
        write_txn.commit()?;

        Ok(record)
    }

    /// Get the current tip hash for an author, or `Hash::ZERO` if none.
    pub fn author_tip(&self, author: &PubKey) -> Hash {
        self.author_tips
            .read()
            .unwrap()
            .get(author)
            .copied()
            .unwrap_or(Hash::ZERO)
    }

    /// Get all current author tips.
    pub fn all_author_tips(&self) -> HashMap<PubKey, Hash> {
        self.author_tips.read().unwrap().clone()
    }

    /// Number of intentions in the store.
    pub fn intention_count(&self) -> Result<u64, IntentionStoreError> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(TABLE_INTENTIONS)?;
        Ok(table.len()?)
    }

    /// Number of witness log entries in the store.
    pub fn witness_count(&self) -> Result<u64, IntentionStoreError> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(TABLE_WITNESS)?;
        Ok(table.len()?)
    }

    /// Iterate all witness records in sequence order.
    pub fn witness_log(&self) -> Result<Vec<(u64, WitnessRecord)>, IntentionStoreError> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(TABLE_WITNESS)?;
        let mut results = Vec::new();
        for entry in table.iter()? {
            let (key, value) = entry?;
            let seq = u64::from_be_bytes(key.value().try_into().map_err(|_| {
                IntentionStoreError::InvalidData("bad witness key".into())
            })?);
            let record = WitnessRecord::decode(value.value())
                .map_err(|e| IntentionStoreError::InvalidData(format!("proto: {e}")))?;
            results.push((seq, record));
        }
        Ok(results)
    }

    /// Check whether the store contains a given intention hash.
    pub fn contains(&self, hash: &Hash) -> Result<bool, IntentionStoreError> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(TABLE_INTENTIONS)?;
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
            let proto = lattice_proto::weaver::SignedIntention::decode(v.value())?;
            let intention = Intention::from_borsh(&proto.intention_borsh)?;
            let sig_bytes: [u8; 64] = proto.signature.try_into()
                .map_err(|_| IntentionStoreError::InvalidData("bad signature length".into()))?;
            results.push(SignedIntention {
                intention,
                signature: lattice_model::types::Signature(sig_bytes),
            });
        }

        Ok(results)
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
        let store = open_store(dir.path());
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
        let store = open_store(dir.path());
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
        let store = open_store(dir.path());
        let (key, pk) = make_key();

        let i1 = make_intention(pk, Hash::ZERO, vec![1]);
        let s1 = SignedIntention::sign(i1, &key);
        let h1 = s1.intention.hash();

        let i2 = make_intention(pk, h1, vec![2]);
        let s2 = SignedIntention::sign(i2, &key);
        let h2 = s2.intention.hash();

        // Insert i2 first (out of order) — should succeed
        store.insert(&s2).unwrap();
        // Tip is dangling — no complete chain yet
        assert_eq!(store.author_tip(&pk), h2);

        // Insert i1 — completes the chain
        store.insert(&s1).unwrap();
        // Tip should still be h2 (head of chain)
        assert_eq!(store.author_tip(&pk), h2);
        assert_eq!(store.intention_count().unwrap(), 2);
    }

    #[test]
    fn author_tip_tracking() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());
        let (key, pk) = make_key();

        assert_eq!(store.author_tip(&pk), Hash::ZERO);

        let s1 = SignedIntention::sign(make_intention(pk, Hash::ZERO, vec![1]), &key);
        let h1 = store.insert(&s1).unwrap();
        assert_eq!(store.author_tip(&pk), h1);

        let s2 = SignedIntention::sign(make_intention(pk, h1, vec![2]), &key);
        let h2 = store.insert(&s2).unwrap();
        assert_eq!(store.author_tip(&pk), h2);
    }

    fn test_store_id() -> Uuid {
        Uuid::from_bytes([0xAA; 16])
    }

    fn open_store(dir: &Path) -> IntentionStore {
        IntentionStore::open(dir, test_store_id()).unwrap()
    }

    #[test]
    fn witness_record() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());
        let (key, _pk) = make_key();

        let hash = Hash([0xBB; 32]);
        let record = store.witness(hash, 1_700_000_000_000, &key).unwrap();
        let content = verify_witness(&record, &key.verifying_key()).unwrap();
        assert_eq!(Uuid::from_slice(&content.store_id).unwrap(), test_store_id());
    }

    #[test]
    fn witness_log_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());
        let (key, _pk) = make_key();

        let hash = Hash([0xCC; 32]);
        store.witness(hash, 42_000, &key).unwrap();

        let log = store.witness_log().unwrap();
        assert_eq!(log.len(), 1);
        let (seq, record) = &log[0];
        assert_eq!(*seq, 1);
        let content = verify_witness(record, &key.verifying_key()).unwrap();
        assert_eq!(Hash::try_from(content.intention_hash.as_slice()).unwrap(), hash);
        assert_eq!(content.wall_time, 42_000);
        assert_eq!(Uuid::from_slice(&content.store_id).unwrap(), test_store_id());
    }

    #[test]
    fn rebuild_after_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let (key, pk) = make_key();

        let h2;
        {
            let store = open_store(dir.path());
            let s1 = SignedIntention::sign(make_intention(pk, Hash::ZERO, vec![1]), &key);
            let h1 = store.insert(&s1).unwrap();

            let s2 = SignedIntention::sign(make_intention(pk, h1, vec![2]), &key);
            h2 = store.insert(&s2).unwrap();
        }

        let store = open_store(dir.path());
        assert_eq!(store.author_tip(&pk), h2);
        assert_eq!(store.intention_count().unwrap(), 2);

        // Can continue the chain
        let s3 = SignedIntention::sign(make_intention(pk, h2, vec![3]), &key);
        store.insert(&s3).unwrap();
        assert_eq!(store.intention_count().unwrap(), 3);
    }
}
