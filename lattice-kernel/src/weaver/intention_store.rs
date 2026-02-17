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
use super::witness::sign_witness;
use prost::Message;
use redb::{Database, TableDefinition, ReadableTable, ReadableTableMetadata};
use thiserror::Error;
use uuid::Uuid;

/// Intentions table: blake3(borsh(intention)) → protobuf SignedIntention bytes
const TABLE_INTENTIONS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("intentions");

/// Witness table: monotonic u64 (big-endian) → protobuf WitnessRecord bytes
const TABLE_WITNESS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("witness");

/// Floating index: intention hash → protobuf FloatingMeta
const TABLE_FLOATING: TableDefinition<&[u8], &[u8]> = TableDefinition::new("floating");

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
    author_tips: HashMap<PubKey, Hash>,

    /// Next witness sequence number (monotonically increasing).
    witness_seq: u64,

    /// blake3 hash of the last WitnessRecord.content bytes.
    /// Used to populate `prev_hash` in the next WitnessContent.
    /// `Hash::ZERO` when the witness log is empty (genesis sentinel).
    last_witness_hash: Hash,

    /// Key used to sign witness records.
    signing_key: ed25519_dalek::SigningKey,
}

impl IntentionStore {
    /// Open or create a `log.db` at the given directory.
    ///
    /// On open, rebuilds the in-memory indexes from the witness log.
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

        {
            let write_txn = db.begin_write()?;
            let _ = write_txn.open_table(TABLE_INTENTIONS)?;
            let _ = write_txn.open_table(TABLE_WITNESS)?;
            let _ = write_txn.open_table(TABLE_FLOATING)?;
            write_txn.commit()?;
        }

        let mut store = Self {
            db,
            store_id,
            author_tips: HashMap::new(),
            witness_seq: 0,
            last_witness_hash: Hash::ZERO,
            signing_key: signing_key.clone(),
        };

        store.rebuild_indexes()?;
        store.backfill_floating_index()?;
        Ok(store)
    }

    /// One-off migration: populate TABLE_FLOATING for existing databases.
    ///
    /// If the floating table is empty but there are intentions not covered by
    /// the witness log, this inserts them with `received_at = 0` (unknown).
    /// Once populated, `insert()` and `witness()` keep the table in sync.
    fn backfill_floating_index(&self) -> Result<(), IntentionStoreError> {
        let write_txn = self.db.begin_write()?;
        {
            let floating_table = write_txn.open_table(TABLE_FLOATING)?;
            if floating_table.len()? > 0 {
                return Ok(()); // already populated
            }
            drop(floating_table);

            let intention_table = write_txn.open_table(TABLE_INTENTIONS)?;
            if intention_table.len()? == 0 {
                return Ok(()); // empty store
            }
            drop(intention_table);

            // Build witnessed set from witness log
            let witness_table = write_txn.open_table(TABLE_WITNESS)?;
            let mut witnessed: std::collections::HashSet<Hash> = std::collections::HashSet::new();
            for entry in witness_table.iter()? {
                let (_, v) = entry?;
                let record = WitnessRecord::decode(v.value())
                    .map_err(|e| IntentionStoreError::InvalidData(format!("witness proto: {e}")))?;
                let content = WitnessContent::decode(record.content.as_slice())
                    .map_err(|e| IntentionStoreError::InvalidData(format!("witness content: {e}")))?;
                let h = Hash::try_from(content.intention_hash.as_slice())
                    .map_err(|_| IntentionStoreError::InvalidData("bad intention_hash".into()))?;
                witnessed.insert(h);
            }
            drop(witness_table);

            // Insert all unwitnessed intentions
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            let meta = FloatingMeta { received_at: now };
            let meta_bytes = meta.encode_to_vec();
            let mut floating = write_txn.open_table(TABLE_FLOATING)?;
            let intention_table = write_txn.open_table(TABLE_INTENTIONS)?;
            for entry in intention_table.iter()? {
                let (k, _) = entry?;
                let hash = Hash::try_from(k.value())
                    .map_err(|_| IntentionStoreError::InvalidData("bad intention hash key".into()))?;
                if !witnessed.contains(&hash) {
                    floating.insert(k.value(), meta_bytes.as_slice())?;
                }
            }
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Rebuild in-memory state from disk.
    ///
    /// Derives `author_tips` from the witness log (only witnessed intentions
    /// are considered). Verifies the witness hash chain strictly.
    fn rebuild_indexes(&mut self) -> Result<(), IntentionStoreError> {
        let read_txn = self.db.begin_read()?;
        let intention_table = read_txn.open_table(TABLE_INTENTIONS)?;
        let witness_table = read_txn.open_table(TABLE_WITNESS)?;

        let mut max_seq = 0;
        let mut expected_prev = Hash::ZERO;
        let mut per_author: HashMap<PubKey, Vec<Hash>> = HashMap::new();
        let mut referenced: std::collections::HashSet<Hash> = std::collections::HashSet::new();

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

            // Rebuild author_tips from witnessed intentions
            let intention_hash = Hash::try_from(content.intention_hash.as_slice())
                .map_err(|_| IntentionStoreError::InvalidData("bad intention_hash in witness".into()))?;

            if let Some(intent_val) = intention_table.get(intention_hash.as_bytes().as_slice())? {
                let proto = lattice_proto::weaver::SignedIntention::decode(intent_val.value())?;
                let intention = Intention::from_borsh(&proto.intention_borsh)?;
                per_author.entry(intention.author).or_default().push(intention_hash);
                if intention.store_prev != Hash::ZERO {
                    referenced.insert(intention.store_prev);
                }
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
        self.author_tips = tips;
        self.witness_seq = max_seq;
        self.last_witness_hash = expected_prev;

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
        }
        write_txn.commit()?;

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
    pub fn witness(
        &mut self,
        intention: &Intention,
        wall_time: u64,
    ) -> Result<WitnessRecord, IntentionStoreError> {
        let hash = intention.hash();
        let content_bytes = Self::encode_witness_content(&hash, wall_time, &self.store_id, &self.last_witness_hash);
        let record = sign_witness(content_bytes.clone(), &self.signing_key);
        let proto_bytes = record.encode_to_vec();

        let seq = self.witness_seq + 1;
        let seq_key = seq.to_be_bytes();

        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(TABLE_WITNESS)?;
            table.insert(seq_key.as_slice(), proto_bytes.as_slice())?;

            // Remove from floating index
            let mut floating = write_txn.open_table(TABLE_FLOATING)?;
            floating.remove(hash.as_bytes().as_slice())?;
        }
        write_txn.commit()?;

        // All in-memory updates after successful commit
        self.witness_seq = seq;
        self.last_witness_hash = Hash(*blake3::hash(&content_bytes).as_bytes());
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
                let proto = lattice_proto::weaver::SignedIntention::decode(iv.value())?;
                let intention = Intention::from_borsh(&proto.intention_borsh)?;
                let sig_bytes: [u8; 64] = proto.signature.try_into()
                    .map_err(|_| IntentionStoreError::InvalidData("bad signature length".into()))?;
                results.push(FloatingIntention {
                    signed: SignedIntention {
                        intention,
                        signature: lattice_model::types::Signature(sig_bytes),
                    },
                    received_at: meta.received_at,
                });
            }
        }
        Ok(results)
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
    /// Returns (seq, content_hash, record) tuples.
    pub fn witness_log(&self) -> Result<Vec<(u64, Hash, WitnessRecord)>, IntentionStoreError> {
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
            let hash = Hash(*blake3::hash(&record.content).as_bytes());
            results.push((seq, hash, record));
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
    use super::super::witness::{sign_witness, verify_witness};
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
        let h1 = s1.intention.hash();

        let i2 = make_intention(pk, h1, vec![2]);
        let s2 = SignedIntention::sign(i2.clone(), &key);
        let h2 = s2.intention.hash();

        // Insert i2 first (out of order) — tip is h2 (only unreferenced hash)
        store.insert(&s2).unwrap();
        assert_eq!(store.author_tip(&pk), h2);

        // Insert i1 — now h1 is referenced by i2's store_prev, so tip stays h2
        store.insert(&s1).unwrap();
        assert_eq!(store.author_tip(&pk), h2);

        // Witness them
        store.witness(&i1, 100).unwrap();
        store.witness(&i2, 200).unwrap();
        
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
        // Tip updated immediately on insert (derived from store_prev linkage)
        assert_eq!(store.author_tip(&pk), h1);
        
        // Witness it
        store.witness(&i1, 100).unwrap();
        assert_eq!(store.author_tip(&pk), h1);

        let i2 = make_intention(pk, h1, vec![2]);
        let s2 = SignedIntention::sign(i2.clone(), &key);
        let h2 = store.insert(&s2).unwrap();
        assert_eq!(store.author_tip(&pk), h2);
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
        
        let content = verify_witness(&record, &key.verifying_key()).unwrap();
        assert_eq!(Uuid::from_slice(&content.store_id).unwrap(), test_store_id());
        // First witness in log: prev_hash must be all-zeros (genesis)
        assert_eq!(content.prev_hash, Hash::ZERO.as_bytes().to_vec());
    }

    #[test]
    fn witness_log_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open_store(dir.path());
        let (key, pk) = make_key();

        let i = make_intention(pk, Hash::ZERO, vec![1]);
        let h = i.hash();
        store.witness(&i, 42_000).unwrap();

        let log = store.witness_log().unwrap();
        assert_eq!(log.len(), 1);
        let (seq, record) = &log[0];
        assert_eq!(*seq, 1);
        let content = verify_witness(record, &key.verifying_key()).unwrap();
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

            let c0 = WitnessContent::decode(log[0].1.content.as_slice()).unwrap();
            let c1 = WitnessContent::decode(log[1].1.content.as_slice()).unwrap();
            let c2 = WitnessContent::decode(log[2].1.content.as_slice()).unwrap();

            // First entry: prev_hash is genesis zero
            assert_eq!(c0.prev_hash, Hash::ZERO.as_bytes().to_vec());
            // Second entry: prev_hash == blake3(first record's content)
            assert_eq!(c1.prev_hash, blake3::hash(&log[0].1.content).as_bytes().to_vec());
            // Third entry: prev_hash == blake3(second record's content)
            assert_eq!(c2.prev_hash, blake3::hash(&log[1].1.content).as_bytes().to_vec());
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
        let c3 = WitnessContent::decode(log[3].1.content.as_slice()).unwrap();
        assert_eq!(c3.prev_hash, blake3::hash(&log[2].1.content).as_bytes().to_vec());
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
}
