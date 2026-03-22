//! Unified CRDT head-tracking engine for KV-shaped conflict domains.
//!
//! Both `KvState` (app data) and `SystemTable` (system metadata) maintain
//! per-key head lists with identical semantics: idempotency check, causal
//! subsumption via parent hashes, LWW resolution at write time.
//!
//! **On-disk format (14D):** `Value { oneof kind { value | tombstone }, heads[] }`
//! where `heads[0]` is the LWW winner. HLC/author metadata is looked up from
//! the DAG on demand — not stored per-head.
//!
//! The engine is agnostic to which redb table it operates on — callers
//! pass in their own `TABLE_DATA` or `TABLE_SYSTEM` table reference.

use std::collections::HashSet;
use std::ops::RangeBounds;

use lattice_model::dag_queries::{DagQueries, IntentionInfo};
use lattice_model::hlc::HLC;
use lattice_model::types::{Hash, PubKey};
use prost::Message;
use redb::ReadableTable;

mod proto {
    include!(concat!(env!("OUT_DIR"), "/lattice.kvtable.rs"));
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors from KVTable operations.
#[derive(Debug, thiserror::Error)]
pub enum KvTableError {
    #[error("Storage error: {0}")]
    Storage(#[from] redb::StorageError),

    #[error("Decode error: {0}")]
    Decode(#[from] prost::DecodeError),

    #[error("Conversion error: {0}")]
    Conversion(String),

    #[error("DAG lookup error: {0}")]
    Dag(#[source] anyhow::Error),
}

// ---------------------------------------------------------------------------
// On-disk encoding/decoding (proto::Value used directly, no domain wrapper)
// ---------------------------------------------------------------------------

/// Decode bytes into a `proto::Value`.
fn decode_value(raw: &[u8]) -> Result<proto::Value, KvTableError> {
    Ok(proto::Value::decode(raw)?)
}

// ---------------------------------------------------------------------------
// LWW comparison
// ---------------------------------------------------------------------------

/// Compare (hlc_a, author_a) vs (hlc_b, author_b) for LWW.
/// Returns true if A wins (A >= B).
fn lww_wins(hlc_a: &HLC, author_a: &PubKey, hlc_b: &HLC, author_b: &PubKey) -> bool {
    (hlc_a, author_a) >= (hlc_b, author_b)
}

// ---------------------------------------------------------------------------
// Read helpers
// ---------------------------------------------------------------------------

/// Decode raw bytes and resolve to the LWW winner value.
/// Returns `None` for tombstones or missing kind.
fn decode_lww(raw: &[u8]) -> Result<Option<Vec<u8>>, KvTableError> {
    let v = decode_value(raw)?;
    match v.kind {
        Some(proto::value::Kind::Value(data)) => Ok(Some(data)),
        _ => Ok(None),
    }
}

/// Decode raw bytes and resolve value + conflict status in one pass.
/// Returns `(value, conflicted)` where `conflicted` is true when `heads.len() > 1`.
fn decode_lww_with_conflict(raw: &[u8]) -> Result<(Option<Vec<u8>>, bool), KvTableError> {
    let v = decode_value(raw)?;
    let conflicted = v.heads.len() > 1;
    let value = match v.kind {
        Some(proto::value::Kind::Value(data)) => Some(data),
        _ => None,
    };
    Ok((value, conflicted))
}

/// Full inspection result for a single key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectResult {
    /// Whether the key has any state at all.
    pub exists: bool,
    /// The LWW winner value, or `None` for tombstone / missing.
    pub value: Option<Vec<u8>>,
    /// Whether the winner is a tombstone (delete).
    pub tombstone: bool,
    /// Whether concurrent heads exist (`heads.len() > 1`).
    pub conflicted: bool,
    /// All head hashes (`heads[0]` = LWW winner).
    pub heads: Vec<Hash>,
}

/// Decode raw bytes into a full `InspectResult`.
fn decode_inspect(raw: &[u8]) -> Result<InspectResult, KvTableError> {
    let v = decode_value(raw)?;
    let heads: Vec<Hash> = v
        .heads
        .iter()
        .map(|h| {
            Hash::try_from(h.as_slice())
                .map_err(|_| KvTableError::Conversion("bad hash in heads".into()))
        })
        .collect::<Result<_, _>>()?;
    let (value, tombstone) = match &v.kind {
        Some(proto::value::Kind::Value(data)) => (Some(data.clone()), false),
        Some(proto::value::Kind::Tombstone(_)) => (None, true),
        None => (None, false),
    };
    Ok(InspectResult {
        exists: true,
        value,
        tombstone,
        conflicted: heads.len() > 1,
        heads,
    })
}

/// Decode raw bytes and extract head hashes as `Vec<Hash>`.
fn decode_heads(raw: &[u8]) -> Result<Vec<Hash>, KvTableError> {
    let v = decode_value(raw)?;
    v.heads
        .iter()
        .map(|h| {
            Hash::try_from(h.as_slice())
                .map_err(|_| KvTableError::Conversion("bad hash in heads".into()))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// KVTable — mutable wrapper for write transactions
// ---------------------------------------------------------------------------

/// Mutable wrapper over a redb table for CRDT head-tracking writes.
///
/// Borrows a `redb::Table` (from an active write transaction) and provides
/// the shared apply logic. Both `KvState` and `SystemTable` construct this
/// transiently during their write paths.
pub struct KVTable<'a, 'txn> {
    table: &'a mut redb::Table<'txn, &'static [u8], &'static [u8]>,
}

impl<'a, 'txn> KVTable<'a, 'txn> {
    /// Wrap an open redb table for write operations.
    pub fn new(table: &'a mut redb::Table<'txn, &'static [u8], &'static [u8]>) -> Self {
        Self { table }
    }

    /// Apply a new head to a key with causal subsumption and LWW resolution.
    ///
    /// `info` identifies the intention (hash, HLC, author).
    /// `causal_deps` are parent hashes this intention supersedes.
    /// `value` is the new value (empty for tombstones).
    /// `tombstone` is true for deletes.
    /// `dag` is used to look up the existing winner's HLC/author for LWW comparison.
    ///
    /// Returns `None` on idempotent skip, `Some(Some(bytes))` for a live
    /// winner, `Some(None)` for a tombstone winner.
    pub fn apply_head(
        &mut self,
        key: &[u8],
        info: &IntentionInfo,
        causal_deps: &[Hash],
        value: Vec<u8>,
        tombstone: bool,
        dag: &dyn DagQueries,
    ) -> Result<Option<Option<Vec<u8>>>, KvTableError> {
        let new_hash = info.hash.as_bytes().to_vec();
        let new_hlc = info.timestamp;
        let new_author = info.author;

        // 1. Read existing
        let mut existing = match self.table.get(key)? {
            Some(v) => decode_value(v.value())?,
            None => proto::Value::default(),
        };

        // 2. Idempotency
        if existing.heads.iter().any(|h| h.as_slice() == new_hash) {
            return Ok(None);
        }

        // 3. Causal subsumption — remove heads that are in parent_hashes
        let parent_set: HashSet<&[u8]> = causal_deps
            .iter()
            .map(|h| h.as_bytes().as_slice())
            .collect();
        existing
            .heads
            .retain(|h| !parent_set.contains(h.as_slice()));

        // 4. Build the new kind
        let new_kind = if tombstone {
            proto::value::Kind::Tombstone(true)
        } else {
            proto::value::Kind::Value(value)
        };

        // 5. LWW resolution: determine if new head wins over current winner
        if existing.heads.is_empty() {
            // No existing heads — new head is the winner
            existing.heads.push(new_hash);
            existing.kind = Some(new_kind);
        } else {
            // Compare new head against current winner (heads[0])
            let current_winner_hash = Hash::try_from(existing.heads[0].as_slice())
                .map_err(|_| KvTableError::Conversion("bad hash in heads[0]".into()))?;
            let current_info = dag
                .get_intention(&current_winner_hash)
                .map_err(KvTableError::Dag)?;

            if lww_wins(
                &new_hlc,
                &new_author,
                &current_info.timestamp,
                &current_info.author,
            ) {
                // New head wins — insert at front, update value
                existing.heads.insert(0, new_hash);
                existing.kind = Some(new_kind);
            } else {
                // Current winner stays — append new head as loser
                existing.heads.push(new_hash);
            }
        }

        // 6. Encode and store
        let encoded = existing.encode_to_vec();
        self.table.insert(key, encoded.as_slice())?;

        let winner = match existing.kind {
            Some(proto::value::Kind::Value(data)) => Some(data),
            _ => None,
        };
        Ok(Some(winner))
    }
}

// ---------------------------------------------------------------------------
// KvRead — shared read interface for both KVTable and ReadOnlyKVTable
// ---------------------------------------------------------------------------

/// Read-only operations on a KV table with LWW-resolved values.
///
/// Implemented by both `KVTable` (mutable, write-transaction) and
/// `ReadOnlyKVTable` (immutable, read-transaction) to avoid duplicating
/// the get/heads/range/inspect logic.
pub trait KvRead {
    #[doc(hidden)]
    fn readable_table(&self) -> &(impl ReadableTable<&'static [u8], &'static [u8]> + ?Sized);

    /// Get the materialized LWW value for a key.
    /// Returns `None` for missing keys or tombstone-only.
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, KvTableError> {
        match self.readable_table().get(key)? {
            Some(v) => Ok(decode_lww(v.value())?),
            None => Ok(None),
        }
    }

    /// Get the materialized LWW value and conflict status for a key.
    ///
    /// Returns `(value, conflicted)` where `conflicted` is true when
    /// `heads.len() > 1`, indicating concurrent writes exist.
    fn get_with_conflict(&self, key: &[u8]) -> Result<(Option<Vec<u8>>, bool), KvTableError> {
        match self.readable_table().get(key)? {
            Some(v) => decode_lww_with_conflict(v.value()),
            None => Ok((None, false)),
        }
    }

    /// Return just the head hashes for a key.
    fn heads(&self, key: &[u8]) -> Result<Vec<Hash>, KvTableError> {
        match self.readable_table().get(key)? {
            Some(v) => decode_heads(v.value()),
            None => Ok(Vec::new()),
        }
    }

    /// Full inspection of a key: value, tombstone status, conflict flag, and all head hashes.
    fn inspect(&self, key: &[u8]) -> Result<InspectResult, KvTableError> {
        match self.readable_table().get(key)? {
            Some(v) => decode_inspect(v.value()),
            None => Ok(InspectResult {
                exists: false,
                value: None,
                tombstone: false,
                conflicted: false,
                heads: Vec::new(),
            }),
        }
    }

    /// Iterate a key range, yielding `(key_bytes, Option<value>, conflicted)`.
    ///
    /// Each value is LWW-resolved: `Some(bytes)` for live entries, `None` for
    /// tombstones. Callers never see raw encoding.
    fn range<'b, 'me, KR>(
        &'me self,
        range: impl RangeBounds<KR> + 'b,
    ) -> Result<LwwRange<'me>, KvTableError>
    where
        KR: std::borrow::Borrow<<&'static [u8] as redb::Value>::SelfType<'b>> + 'b,
    {
        let inner = self.readable_table().range(range)?;
        Ok(LwwRange { inner })
    }

    /// Iterate all entries, yielding `(key_bytes, Option<value>, conflicted)`.
    fn iter(&self) -> Result<LwwRange<'_>, KvTableError> {
        let inner = self.readable_table().iter()?;
        Ok(LwwRange { inner })
    }
}

impl KvRead for KVTable<'_, '_> {
    fn readable_table(&self) -> &(impl ReadableTable<&'static [u8], &'static [u8]> + ?Sized) {
        &*self.table
    }
}

// ---------------------------------------------------------------------------
// ReadOnlyKVTable — read-only wrapper for read transactions
// ---------------------------------------------------------------------------

/// Read-only wrapper over a redb table for head reads.
///
/// Works with `ReadOnlyTable` from read transactions. Used by `KvState::get()`
/// and `ReadOnlySystemTable` read methods.
pub struct ReadOnlyKVTable<T> {
    table: T,
}

impl<T: ReadableTable<&'static [u8], &'static [u8]>> ReadOnlyKVTable<T> {
    /// Wrap a readable redb table.
    pub fn new(table: T) -> Self {
        Self { table }
    }
}

impl<T: ReadableTable<&'static [u8], &'static [u8]>> KvRead for ReadOnlyKVTable<T> {
    fn readable_table(&self) -> &(impl ReadableTable<&'static [u8], &'static [u8]> + ?Sized) {
        &self.table
    }
}

// ---------------------------------------------------------------------------
// LwwRange — LWW-resolving iterator adapter
// ---------------------------------------------------------------------------

/// Iterator that wraps a redb `Range`, decoding and LWW-resolving each value.
///
/// Yields `(Vec<u8>, Option<Vec<u8>>, bool)`: owned key bytes, the resolved value
/// (`None` for tombstones), and a `conflicted` flag (`true` when `heads.len() > 1`).
pub struct LwwRange<'a> {
    inner: redb::Range<'a, &'static [u8], &'static [u8]>,
}

impl Iterator for LwwRange<'_> {
    type Item = Result<(Vec<u8>, Option<Vec<u8>>, bool), KvTableError>;

    fn next(&mut self) -> Option<Self::Item> {
        let result = self.inner.next()?;
        Some(result.map_err(KvTableError::Storage).and_then(|(k, v)| {
            let key = k.value().to_vec();
            let (value, conflicted) = decode_lww_with_conflict(v.value())?;
            Ok((key, value, conflicted))
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_model::NullDag;
    use redb::TableDefinition;

    const TEST_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("test");

    fn test_info() -> IntentionInfo<'static> {
        IntentionInfo {
            hash: Hash::from([1u8; 32]),
            payload: std::borrow::Cow::Borrowed(&[]),
            timestamp: lattice_model::hlc::HLC::new(1, 0),
            author: PubKey::from([0xAA; 32]),
        }
    }

    #[test]
    fn empty_value_roundtrips() {
        let db = redb::Database::builder()
            .create_with_backend(redb::backends::InMemoryBackend::new())
            .unwrap();
        let txn = db.begin_write().unwrap();
        {
            let mut table = txn.open_table(TEST_TABLE).unwrap();
            let mut kvt = KVTable::new(&mut table);

            let result = kvt
                .apply_head(b"key", &test_info(), &[], vec![], false, &NullDag)
                .unwrap();

            // Should be Some(Some(empty vec)) — live entry with empty value
            assert_eq!(result, Some(Some(vec![])));

            // Read back via get — should return Some(empty)
            assert_eq!(kvt.get(b"key").unwrap(), Some(vec![]));
        }
        txn.commit().unwrap();
    }

    #[test]
    fn tombstone_roundtrips() {
        let db = redb::Database::builder()
            .create_with_backend(redb::backends::InMemoryBackend::new())
            .unwrap();
        let txn = db.begin_write().unwrap();
        {
            let mut table = txn.open_table(TEST_TABLE).unwrap();
            let mut kvt = KVTable::new(&mut table);

            let result = kvt
                .apply_head(b"key", &test_info(), &[], vec![], true, &NullDag)
                .unwrap();

            // Should be Some(None) — tombstone
            assert_eq!(result, Some(None));

            // Read back via get — should return None
            assert_eq!(kvt.get(b"key").unwrap(), None);
        }
        txn.commit().unwrap();
    }

    #[test]
    fn get_with_conflict_single_head() {
        let db = redb::Database::builder()
            .create_with_backend(redb::backends::InMemoryBackend::new())
            .unwrap();
        let txn = db.begin_write().unwrap();
        {
            let mut table = txn.open_table(TEST_TABLE).unwrap();
            let mut kvt = KVTable::new(&mut table);

            kvt.apply_head(b"key", &test_info(), &[], b"val".to_vec(), false, &NullDag)
                .unwrap();
        }
        txn.commit().unwrap();

        let rtx = db.begin_read().unwrap();
        let table = rtx.open_table(TEST_TABLE).unwrap();
        let ro = ReadOnlyKVTable::new(table);

        let (value, conflicted) = ro.get_with_conflict(b"key").unwrap();
        assert_eq!(value, Some(b"val".to_vec()));
        assert!(!conflicted, "Single head should not be conflicted");
    }

    #[test]
    fn get_with_conflict_multi_heads() {
        use lattice_model::dag_queries::HashMapDag;
        use lattice_model::state_machine::Op;

        let db = redb::Database::builder()
            .create_with_backend(redb::backends::InMemoryBackend::new())
            .unwrap();
        let dag = HashMapDag::new();

        // Apply two concurrent heads (different authors, same HLC, no deps)
        let info1 = IntentionInfo {
            hash: Hash::from([1u8; 32]),
            payload: std::borrow::Cow::Borrowed(&[]),
            timestamp: lattice_model::hlc::HLC::new(1, 0),
            author: PubKey::from([0xAA; 32]),
        };
        let info2 = IntentionInfo {
            hash: Hash::from([2u8; 32]),
            payload: std::borrow::Cow::Borrowed(&[]),
            timestamp: lattice_model::hlc::HLC::new(1, 0),
            author: PubKey::from([0xBB; 32]),
        };

        // First apply needs no DAG (no existing heads), second needs the first in the DAG
        let txn = db.begin_write().unwrap();
        {
            let mut table = txn.open_table(TEST_TABLE).unwrap();
            let mut kvt = KVTable::new(&mut table);

            // First head — no existing heads, so NullDag suffices
            kvt.apply_head(b"key", &info1, &[], b"v1".to_vec(), false, &NullDag)
                .unwrap();

            // Record info1 so the DAG can look it up during LWW comparison
            let empty_deps: Vec<Hash> = vec![];
            let op1 = Op {
                info: info1.clone(),
                causal_deps: &empty_deps,
                prev_hash: Hash::ZERO,
            };
            dag.record(&op1);

            // Second head — concurrent, DAG needed to compare against current winner
            kvt.apply_head(b"key", &info2, &[], b"v2".to_vec(), false, &dag)
                .unwrap();
        }
        txn.commit().unwrap();

        let rtx = db.begin_read().unwrap();
        let table = rtx.open_table(TEST_TABLE).unwrap();
        let ro = ReadOnlyKVTable::new(table);

        let (value, conflicted) = ro.get_with_conflict(b"key").unwrap();
        assert!(value.is_some(), "LWW winner should be returned");
        assert!(conflicted, "Two concurrent heads should be conflicted");
    }

    #[test]
    fn get_with_conflict_missing_key() {
        let db = redb::Database::builder()
            .create_with_backend(redb::backends::InMemoryBackend::new())
            .unwrap();
        let txn = db.begin_write().unwrap();
        {
            txn.open_table(TEST_TABLE).unwrap();
        }
        txn.commit().unwrap();

        let rtx = db.begin_read().unwrap();
        let table = rtx.open_table(TEST_TABLE).unwrap();
        let ro = ReadOnlyKVTable::new(table);

        let (value, conflicted) = ro.get_with_conflict(b"nope").unwrap();
        assert_eq!(value, None);
        assert!(!conflicted);
    }

    #[test]
    fn kvtable_get_missing_key() {
        let db = redb::Database::builder()
            .create_with_backend(redb::backends::InMemoryBackend::new())
            .unwrap();
        let txn = db.begin_write().unwrap();
        {
            let mut table = txn.open_table(TEST_TABLE).unwrap();
            let kvt = KVTable::new(&mut table);
            assert_eq!(kvt.get(b"nope").unwrap(), None);
        }
        txn.commit().unwrap();
    }

    #[test]
    fn kvtable_heads() {
        let db = redb::Database::builder()
            .create_with_backend(redb::backends::InMemoryBackend::new())
            .unwrap();
        let txn = db.begin_write().unwrap();
        {
            let mut table = txn.open_table(TEST_TABLE).unwrap();
            let mut kvt = KVTable::new(&mut table);

            // Missing key returns empty
            assert!(kvt.heads(b"nope").unwrap().is_empty());

            // After apply, returns the head hash
            let info = test_info();
            kvt.apply_head(b"key", &info, &[], b"val".to_vec(), false, &NullDag)
                .unwrap();
            assert_eq!(kvt.heads(b"key").unwrap(), vec![info.hash]);
        }
        txn.commit().unwrap();
    }

    #[test]
    fn inspect_missing_key() {
        let db = redb::Database::builder()
            .create_with_backend(redb::backends::InMemoryBackend::new())
            .unwrap();
        let txn = db.begin_write().unwrap();
        {
            txn.open_table(TEST_TABLE).unwrap();
        }
        txn.commit().unwrap();

        let rtx = db.begin_read().unwrap();
        let table = rtx.open_table(TEST_TABLE).unwrap();
        let ro = ReadOnlyKVTable::new(table);

        let result = ro.inspect(b"nope").unwrap();
        assert!(!result.exists);
        assert_eq!(result.value, None);
        assert!(!result.tombstone);
        assert!(!result.conflicted);
        assert!(result.heads.is_empty());
    }

    #[test]
    fn iter_all_entries() {
        let db = redb::Database::builder()
            .create_with_backend(redb::backends::InMemoryBackend::new())
            .unwrap();
        let txn = db.begin_write().unwrap();
        {
            let mut table = txn.open_table(TEST_TABLE).unwrap();
            let mut kvt = KVTable::new(&mut table);

            let info_a = IntentionInfo {
                hash: Hash::from([1u8; 32]),
                timestamp: lattice_model::hlc::HLC::new(1, 0),
                author: PubKey::from([0xAA; 32]),
                payload: std::borrow::Cow::Borrowed(&[]),
            };
            let info_b = IntentionInfo {
                hash: Hash::from([2u8; 32]),
                timestamp: lattice_model::hlc::HLC::new(2, 0),
                author: PubKey::from([0xAA; 32]),
                payload: std::borrow::Cow::Borrowed(&[]),
            };

            kvt.apply_head(b"a", &info_a, &[], b"va".to_vec(), false, &NullDag)
                .unwrap();
            kvt.apply_head(b"b", &info_b, &[], b"vb".to_vec(), false, &NullDag)
                .unwrap();
        }
        txn.commit().unwrap();

        let rtx = db.begin_read().unwrap();
        let table = rtx.open_table(TEST_TABLE).unwrap();
        let ro = ReadOnlyKVTable::new(table);

        let entries: Vec<_> = ro.iter().unwrap().collect::<Result<_, _>>().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].0, b"a");
        assert_eq!(entries[0].1, Some(b"va".to_vec()));
        assert!(!entries[0].2);
        assert_eq!(entries[1].0, b"b");
        assert_eq!(entries[1].1, Some(b"vb".to_vec()));
        assert!(!entries[1].2);
    }

    #[test]
    fn get_with_conflict_tombstone() {
        let db = redb::Database::builder()
            .create_with_backend(redb::backends::InMemoryBackend::new())
            .unwrap();

        let info1 = IntentionInfo {
            hash: Hash::from([1u8; 32]),
            timestamp: lattice_model::hlc::HLC::new(1, 0),
            author: PubKey::from([0xAA; 32]),
            payload: std::borrow::Cow::Borrowed(&[]),
        };
        let info2 = IntentionInfo {
            hash: Hash::from([2u8; 32]),
            timestamp: lattice_model::hlc::HLC::new(2, 0),
            author: PubKey::from([0xAA; 32]),
            payload: std::borrow::Cow::Borrowed(&[]),
        };

        let txn = db.begin_write().unwrap();
        {
            let mut table = txn.open_table(TEST_TABLE).unwrap();
            let mut kvt = KVTable::new(&mut table);

            // Put a value, then delete it (tombstone supersedes via causal dep)
            kvt.apply_head(b"key", &info1, &[], b"val".to_vec(), false, &NullDag)
                .unwrap();
            kvt.apply_head(b"key", &info2, &[info1.hash], vec![], true, &NullDag)
                .unwrap();
        }
        txn.commit().unwrap();

        let rtx = db.begin_read().unwrap();
        let table = rtx.open_table(TEST_TABLE).unwrap();
        let ro = ReadOnlyKVTable::new(table);

        let (value, conflicted) = ro.get_with_conflict(b"key").unwrap();
        assert_eq!(value, None, "Tombstone should return None");
        assert!(!conflicted, "Single head after causal subsumption");
    }

    #[test]
    fn iter_with_tombstone() {
        let db = redb::Database::builder()
            .create_with_backend(redb::backends::InMemoryBackend::new())
            .unwrap();

        let info1 = IntentionInfo {
            hash: Hash::from([1u8; 32]),
            timestamp: lattice_model::hlc::HLC::new(1, 0),
            author: PubKey::from([0xAA; 32]),
            payload: std::borrow::Cow::Borrowed(&[]),
        };
        let info2 = IntentionInfo {
            hash: Hash::from([2u8; 32]),
            timestamp: lattice_model::hlc::HLC::new(2, 0),
            author: PubKey::from([0xAA; 32]),
            payload: std::borrow::Cow::Borrowed(&[]),
        };

        let txn = db.begin_write().unwrap();
        {
            let mut table = txn.open_table(TEST_TABLE).unwrap();
            let mut kvt = KVTable::new(&mut table);

            // Live entry
            kvt.apply_head(b"alive", &info1, &[], b"val".to_vec(), false, &NullDag)
                .unwrap();
            // Tombstone entry
            kvt.apply_head(b"dead", &info2, &[], vec![], true, &NullDag)
                .unwrap();
        }
        txn.commit().unwrap();

        let rtx = db.begin_read().unwrap();
        let table = rtx.open_table(TEST_TABLE).unwrap();
        let ro = ReadOnlyKVTable::new(table);

        let entries: Vec<_> = ro.iter().unwrap().collect::<Result<_, _>>().unwrap();
        assert_eq!(entries.len(), 2);
        // "alive" < "dead" lexicographically
        assert_eq!(entries[0].0, b"alive");
        assert_eq!(entries[0].1, Some(b"val".to_vec()));
        assert_eq!(entries[1].0, b"dead");
        assert_eq!(entries[1].1, None, "Tombstone should yield None");
    }

    #[test]
    fn inspect_value_and_tombstone() {
        let db = redb::Database::builder()
            .create_with_backend(redb::backends::InMemoryBackend::new())
            .unwrap();
        let txn = db.begin_write().unwrap();
        {
            let mut table = txn.open_table(TEST_TABLE).unwrap();
            let mut kvt = KVTable::new(&mut table);

            let info1 = IntentionInfo {
                hash: Hash::from([1u8; 32]),
                timestamp: lattice_model::hlc::HLC::new(1, 0),
                author: PubKey::from([0xAA; 32]),
                payload: std::borrow::Cow::Borrowed(&[]),
            };
            let info2 = IntentionInfo {
                hash: Hash::from([2u8; 32]),
                timestamp: lattice_model::hlc::HLC::new(2, 0),
                author: PubKey::from([0xAA; 32]),
                payload: std::borrow::Cow::Borrowed(&[]),
            };

            kvt.apply_head(b"live", &info1, &[], b"val".to_vec(), false, &NullDag)
                .unwrap();
            kvt.apply_head(b"dead", &info2, &[], vec![], true, &NullDag)
                .unwrap();
        }
        txn.commit().unwrap();

        let rtx = db.begin_read().unwrap();
        let table = rtx.open_table(TEST_TABLE).unwrap();
        let ro = ReadOnlyKVTable::new(table);

        let live = ro.inspect(b"live").unwrap();
        assert!(live.exists);
        assert_eq!(live.value, Some(b"val".to_vec()));
        assert!(!live.tombstone);

        let dead = ro.inspect(b"dead").unwrap();
        assert!(dead.exists);
        assert_eq!(dead.value, None);
        assert!(dead.tombstone);
    }

    #[test]
    fn readonly_get_and_heads_and_range() {
        let db = redb::Database::builder()
            .create_with_backend(redb::backends::InMemoryBackend::new())
            .unwrap();
        let txn = db.begin_write().unwrap();
        {
            let mut table = txn.open_table(TEST_TABLE).unwrap();
            let mut kvt = KVTable::new(&mut table);

            let info = test_info();
            kvt.apply_head(b"a", &info, &[], b"va".to_vec(), false, &NullDag)
                .unwrap();
        }
        txn.commit().unwrap();

        let rtx = db.begin_read().unwrap();
        let table = rtx.open_table(TEST_TABLE).unwrap();
        let ro = ReadOnlyKVTable::new(table);

        assert_eq!(ro.get(b"a").unwrap(), Some(b"va".to_vec()));
        assert_eq!(ro.get(b"nope").unwrap(), None);
        assert_eq!(ro.heads(b"a").unwrap(), vec![test_info().hash]);
        assert!(ro.heads(b"nope").unwrap().is_empty());

        let range: Vec<_> = ro
            .range::<&[u8]>(..)
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(range.len(), 1);
        assert_eq!(range[0].0, b"a");
        assert_eq!(range[0].1, Some(b"va".to_vec()));
    }

    #[test]
    fn apply_idempotent_and_loser_branch() {
        use lattice_model::dag_queries::HashMapDag;
        use lattice_model::state_machine::Op;

        let db = redb::Database::builder()
            .create_with_backend(redb::backends::InMemoryBackend::new())
            .unwrap();
        let dag = HashMapDag::new();

        let info1 = IntentionInfo {
            hash: Hash::from([1u8; 32]),
            timestamp: lattice_model::hlc::HLC::new(2, 0),
            author: PubKey::from([0xBB; 32]),
            payload: std::borrow::Cow::Borrowed(&[]),
        };
        let info2 = IntentionInfo {
            hash: Hash::from([2u8; 32]),
            timestamp: lattice_model::hlc::HLC::new(1, 0),
            author: PubKey::from([0xAA; 32]),
            payload: std::borrow::Cow::Borrowed(&[]),
        };

        let txn = db.begin_write().unwrap();
        {
            let mut table = txn.open_table(TEST_TABLE).unwrap();
            let mut kvt = KVTable::new(&mut table);

            // First apply
            kvt.apply_head(b"key", &info1, &[], b"v1".to_vec(), false, &NullDag)
                .unwrap();

            // Idempotent re-apply returns None
            let dup = kvt
                .apply_head(b"key", &info1, &[], b"v1".to_vec(), false, &NullDag)
                .unwrap();
            assert_eq!(dup, None, "Duplicate should be idempotent");

            // Record info1 in DAG for LWW comparison
            let empty_deps: Vec<Hash> = vec![];
            dag.record(&Op {
                info: info1.clone(),
                causal_deps: &empty_deps,
                prev_hash: Hash::ZERO,
            });

            // info2 has lower HLC, so it loses
            let result = kvt
                .apply_head(b"key", &info2, &[], b"v2".to_vec(), false, &dag)
                .unwrap();
            // Current winner stays, value returned is still v1
            assert_eq!(result, Some(Some(b"v1".to_vec())));
        }
        txn.commit().unwrap();
    }

    // Apply two concurrent heads in both orders; verify identical winner and head set.
    #[test]
    fn convergence_order_independent() {
        use lattice_model::dag_queries::HashMapDag;
        use lattice_model::state_machine::Op;

        let info_a = IntentionInfo {
            hash: Hash::from([1u8; 32]),
            timestamp: HLC::new(1, 0),
            author: PubKey::from([0xAA; 32]),
            payload: std::borrow::Cow::Borrowed(&[]),
        };
        let info_b = IntentionInfo {
            hash: Hash::from([2u8; 32]),
            timestamp: HLC::new(1, 0),
            author: PubKey::from([0xBB; 32]),
            payload: std::borrow::Cow::Borrowed(&[]),
        };

        let empty_deps: Vec<Hash> = vec![];
        let op_a = Op {
            info: info_a.clone(),
            causal_deps: &empty_deps,
            prev_hash: Hash::ZERO,
        };
        let op_b = Op {
            info: info_b.clone(),
            causal_deps: &empty_deps,
            prev_hash: Hash::ZERO,
        };

        // Order 1: A then B
        let dag1 = HashMapDag::new();
        let db1 = redb::Database::builder()
            .create_with_backend(redb::backends::InMemoryBackend::new())
            .unwrap();
        let txn1 = db1.begin_write().unwrap();
        {
            let mut table = txn1.open_table(TEST_TABLE).unwrap();
            let mut kvt = KVTable::new(&mut table);
            kvt.apply_head(b"k", &info_a, &[], b"vA".to_vec(), false, &NullDag)
                .unwrap();
            dag1.record(&op_a);
            kvt.apply_head(b"k", &info_b, &[], b"vB".to_vec(), false, &dag1)
                .unwrap();
        }
        txn1.commit().unwrap();

        // Order 2: B then A
        let dag2 = HashMapDag::new();
        let db2 = redb::Database::builder()
            .create_with_backend(redb::backends::InMemoryBackend::new())
            .unwrap();
        let txn2 = db2.begin_write().unwrap();
        {
            let mut table = txn2.open_table(TEST_TABLE).unwrap();
            let mut kvt = KVTable::new(&mut table);
            kvt.apply_head(b"k", &info_b, &[], b"vB".to_vec(), false, &NullDag)
                .unwrap();
            dag2.record(&op_b);
            kvt.apply_head(b"k", &info_a, &[], b"vA".to_vec(), false, &dag2)
                .unwrap();
        }
        txn2.commit().unwrap();

        let r1 = db1.begin_read().unwrap();
        let t1 = r1.open_table(TEST_TABLE).unwrap();
        let ro1 = ReadOnlyKVTable::new(t1);

        let r2 = db2.begin_read().unwrap();
        let t2 = r2.open_table(TEST_TABLE).unwrap();
        let ro2 = ReadOnlyKVTable::new(t2);

        let (val1, c1) = ro1.get_with_conflict(b"k").unwrap();
        let (val2, c2) = ro2.get_with_conflict(b"k").unwrap();
        assert_eq!(
            val1, val2,
            "LWW winner must be the same regardless of apply order"
        );
        assert_eq!(c1, c2);

        let mut heads1 = ro1.heads(b"k").unwrap();
        let mut heads2 = ro2.heads(b"k").unwrap();
        heads1.sort();
        heads2.sort();
        assert_eq!(
            heads1, heads2,
            "Head sets must match regardless of apply order"
        );
    }

    // Applying a head whose causal_deps include an existing head removes the old head.
    #[test]
    fn causal_supersession_replaces_head() {
        let db = redb::Database::builder()
            .create_with_backend(redb::backends::InMemoryBackend::new())
            .unwrap();

        let hash1 = Hash::from([1u8; 32]);
        let hash2 = Hash::from([2u8; 32]);

        let info1 = IntentionInfo {
            hash: hash1,
            timestamp: HLC::new(1, 0),
            author: PubKey::from([0xAA; 32]),
            payload: std::borrow::Cow::Borrowed(&[]),
        };
        let info2 = IntentionInfo {
            hash: hash2,
            timestamp: HLC::new(2, 0),
            author: PubKey::from([0xAA; 32]),
            payload: std::borrow::Cow::Borrowed(&[]),
        };

        let txn = db.begin_write().unwrap();
        {
            let mut table = txn.open_table(TEST_TABLE).unwrap();
            let mut kvt = KVTable::new(&mut table);

            kvt.apply_head(b"key", &info1, &[], b"v1".to_vec(), false, &NullDag)
                .unwrap();

            // Second head causally supersedes the first
            kvt.apply_head(b"key", &info2, &[hash1], b"v2".to_vec(), false, &NullDag)
                .unwrap();
        }
        txn.commit().unwrap();

        let rtx = db.begin_read().unwrap();
        let table = rtx.open_table(TEST_TABLE).unwrap();
        let ro = ReadOnlyKVTable::new(table);

        let heads = ro.heads(b"key").unwrap();
        assert_eq!(heads, vec![hash2], "Old head should be superseded");

        let (val, conflicted) = ro.get_with_conflict(b"key").unwrap();
        assert_eq!(val, Some(b"v2".to_vec()));
        assert!(
            !conflicted,
            "Single head after supersession should not be conflicted"
        );
    }

    // Causal supersession with a tombstone: delete that deps on a put removes the put head.
    #[test]
    fn causal_supersession_delete() {
        let db = redb::Database::builder()
            .create_with_backend(redb::backends::InMemoryBackend::new())
            .unwrap();

        let put_hash = Hash::from([1u8; 32]);
        let del_hash = Hash::from([2u8; 32]);

        let put_info = IntentionInfo {
            hash: put_hash,
            timestamp: HLC::new(1, 0),
            author: PubKey::from([0xAA; 32]),
            payload: std::borrow::Cow::Borrowed(&[]),
        };
        let del_info = IntentionInfo {
            hash: del_hash,
            timestamp: HLC::new(2, 0),
            author: PubKey::from([0xAA; 32]),
            payload: std::borrow::Cow::Borrowed(&[]),
        };

        let txn = db.begin_write().unwrap();
        {
            let mut table = txn.open_table(TEST_TABLE).unwrap();
            let mut kvt = KVTable::new(&mut table);

            kvt.apply_head(b"key", &put_info, &[], b"val".to_vec(), false, &NullDag)
                .unwrap();

            // Delete causally depends on the put
            kvt.apply_head(b"key", &del_info, &[put_hash], vec![], true, &NullDag)
                .unwrap();
        }
        txn.commit().unwrap();

        let rtx = db.begin_read().unwrap();
        let table = rtx.open_table(TEST_TABLE).unwrap();
        let ro = ReadOnlyKVTable::new(table);

        assert_eq!(
            ro.get(b"key").unwrap(),
            None,
            "Tombstone should hide the value"
        );
        let heads = ro.heads(b"key").unwrap();
        assert_eq!(heads, vec![del_hash], "Only tombstone head should remain");
    }

    #[test]
    fn inspect_no_kind() {
        // A proto::Value with a head but no kind set (malformed/corrupt data).
        // inspect() should return exists=true, value=None, tombstone=false.
        let db = redb::Database::builder()
            .create_with_backend(redb::backends::InMemoryBackend::new())
            .unwrap();

        let hash = Hash::from([1u8; 32]);
        let raw = proto::Value {
            kind: None,
            heads: vec![hash.as_bytes().to_vec()],
        };
        let encoded = raw.encode_to_vec();

        let txn = db.begin_write().unwrap();
        {
            let mut table = txn.open_table(TEST_TABLE).unwrap();
            table
                .insert(b"broken".as_slice(), encoded.as_slice())
                .unwrap();
        }
        txn.commit().unwrap();

        let rtx = db.begin_read().unwrap();
        let table = rtx.open_table(TEST_TABLE).unwrap();
        let ro = ReadOnlyKVTable::new(table);

        let result = ro.inspect(b"broken").unwrap();
        assert!(result.exists);
        assert_eq!(result.value, None);
        assert!(!result.tombstone);
        assert!(!result.conflicted);
        assert_eq!(result.heads, vec![hash]);
    }
}
