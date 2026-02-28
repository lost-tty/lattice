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

use lattice_model::dag_queries::DagQueries;
use lattice_model::hlc::HLC;
use lattice_model::state_machine::Op;
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
    proto::Value::decode(raw).map_err(|e| KvTableError::Conversion(e.to_string()))
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
    /// `op` provides the new intention's hash, HLC, and author.
    /// `value` is the new value (empty for tombstones).
    /// `tombstone` is true for deletes.
    /// `dag` is used to look up the existing winner's HLC/author for LWW comparison.
    ///
    /// Returns `None` on idempotent skip, `Some(Some(bytes))` for a live
    /// winner, `Some(None)` for a tombstone winner.
    pub fn apply_head(
        &mut self,
        key: &[u8],
        op: &Op,
        value: Vec<u8>,
        tombstone: bool,
        dag: &dyn DagQueries,
    ) -> Result<Option<Option<Vec<u8>>>, KvTableError> {
        let new_hash = op.id.as_bytes().to_vec();
        let new_hlc = op.timestamp;
        let new_author = op.author;

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
        let parent_set: HashSet<&[u8]> = op
            .causal_deps
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

    /// Get the materialized LWW value for a key.
    /// Returns `None` for missing keys or tombstone-only.
    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, KvTableError> {
        match self.table.get(key)? {
            Some(v) => Ok(decode_lww(v.value())?),
            None => Ok(None),
        }
    }

    /// Return just the head hashes for a key.
    pub fn heads(&self, key: &[u8]) -> Result<Vec<Hash>, KvTableError> {
        match self.table.get(key)? {
            Some(v) => decode_heads(v.value()),
            None => Ok(Vec::new()),
        }
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

    /// Get the materialized LWW value for a key.
    /// Returns `None` for missing keys or tombstone-only.
    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, KvTableError> {
        match self.table.get(key)? {
            Some(v) => Ok(decode_lww(v.value())?),
            None => Ok(None),
        }
    }

    /// Return just the head hashes for a key.
    pub fn heads(&self, key: &[u8]) -> Result<Vec<Hash>, KvTableError> {
        match self.table.get(key)? {
            Some(v) => decode_heads(v.value()),
            None => Ok(Vec::new()),
        }
    }

    /// Iterate a key range, yielding `(key_bytes, Option<value>)` per entry.
    ///
    /// Each value is LWW-resolved: `Some(bytes)` for live entries, `None` for
    /// tombstones. Callers never see raw encoding.
    pub fn range<'b, KR>(
        &self,
        range: impl RangeBounds<KR> + 'b,
    ) -> Result<LwwRange<'_>, KvTableError>
    where
        KR: std::borrow::Borrow<<&'static [u8] as redb::Value>::SelfType<'b>> + 'b,
    {
        let inner = self.table.range(range)?;
        Ok(LwwRange { inner })
    }

    /// Iterate all entries, yielding `(key_bytes, Option<value>)` per entry.
    pub fn iter(&self) -> Result<LwwRange<'_>, KvTableError> {
        let inner = self.table.iter()?;
        Ok(LwwRange { inner })
    }
}

// ---------------------------------------------------------------------------
// LwwRange — LWW-resolving iterator adapter
// ---------------------------------------------------------------------------

/// Iterator that wraps a redb `Range`, decoding and LWW-resolving each value.
///
/// Yields `(Vec<u8>, Option<Vec<u8>>)`: owned key bytes and the resolved value
/// (`None` for tombstones).
pub struct LwwRange<'a> {
    inner: redb::Range<'a, &'static [u8], &'static [u8]>,
}

impl Iterator for LwwRange<'_> {
    type Item = Result<(Vec<u8>, Option<Vec<u8>>), KvTableError>;

    fn next(&mut self) -> Option<Self::Item> {
        let result = self.inner.next()?;
        Some(
            result
                .map_err(|e| KvTableError::Storage(e.into()))
                .and_then(|(k, v)| {
                    let key = k.value().to_vec();
                    let value = decode_lww(v.value())?;
                    Ok((key, value))
                }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_model::{NullDag, Op};
    use redb::TableDefinition;

    const TEST_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("test");

    fn test_op() -> Op<'static> {
        Op {
            id: Hash::from([1u8; 32]),
            timestamp: lattice_model::hlc::HLC::new(1, 0),
            author: PubKey::from([0xAA; 32]),
            payload: &[],
            causal_deps: &[],
            prev_hash: Hash::from([0u8; 32]),
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
                .apply_head(b"key", &test_op(), vec![], false, &NullDag)
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
                .apply_head(b"key", &test_op(), vec![], true, &NullDag)
                .unwrap();

            // Should be Some(None) — tombstone
            assert_eq!(result, Some(None));

            // Read back via get — should return None
            assert_eq!(kvt.get(b"key").unwrap(), None);
        }
        txn.commit().unwrap();
    }
}
