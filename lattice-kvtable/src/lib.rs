//! Unified CRDT head-tracking engine for KV-shaped conflict domains.
//!
//! Both `KvState` (app data) and `SystemTable` (system metadata) maintain
//! per-key head lists with identical semantics: idempotency check, causal
//! subsumption via parent hashes, deterministic sort, protobuf-encoded
//! `HeadList` storage. This crate extracts that shared logic into `KVTable`.
//!
//! The engine is agnostic to which redb table it operates on — callers
//! pass in their own `TABLE_DATA` or `TABLE_SYSTEM` table reference.

use std::collections::HashSet;

use lattice_model::head::Head;
use lattice_model::types::Hash;
use lattice_proto::storage::{HeadInfo as ProtoHeadInfo, HeadList};
use prost::Message;
use redb::ReadableTable;

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
}

// ---------------------------------------------------------------------------
// Decoding helper (shared by read and write paths)
// ---------------------------------------------------------------------------

/// Decode a protobuf-encoded `HeadList` value into `Vec<Head>`.
pub fn decode_heads(raw: &[u8]) -> Result<Vec<Head>, KvTableError> {
    let head_list = HeadList::decode(raw).map_err(|e| KvTableError::Conversion(e.to_string()))?;
    head_list
        .heads
        .into_iter()
        .map(|h| Head::try_from(h).map_err(KvTableError::Conversion))
        .collect()
}

/// Encode `Vec<Head>` into protobuf `HeadList` bytes.
pub fn encode_heads(heads: &[Head]) -> Vec<u8> {
    let proto_heads: Vec<ProtoHeadInfo> = heads.iter().map(|h| h.clone().into()).collect();
    HeadList { heads: proto_heads }.encode_to_vec()
}

/// Deterministic sort order for heads: descending HLC, then descending author.
///
/// This ensures the HeadList binary encoding is identical regardless of
/// insertion order, which is required for convergent state across nodes.
fn sort_heads(heads: &mut [Head]) {
    heads.sort_by(|a, b| b.hlc.cmp(&a.hlc).then_with(|| b.author.cmp(&a.author)));
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

    /// Apply a new head to a key with causal subsumption.
    ///
    /// Steps:
    /// 1. Read existing heads for `key`
    /// 2. Idempotency check — skip if `new_head.hash` already present
    /// 3. Remove heads subsumed by `parent_hashes` (causal deps)
    /// 4. Push `new_head`, sort deterministically
    /// 5. Encode and store
    ///
    /// Returns `None` on idempotent skip, `Some(new_heads)` on mutation.
    pub fn apply_head(
        &mut self,
        key: &[u8],
        new_head: Head,
        parent_hashes: &[Hash],
    ) -> Result<Option<Vec<Head>>, KvTableError> {
        // 1. Read existing
        let mut heads = match self.table.get(key)? {
            Some(v) => decode_heads(v.value())?,
            None => Vec::new(),
        };

        // 2. Idempotency
        if heads.iter().any(|h| h.hash == new_head.hash) {
            return Ok(None);
        }

        // 3. Causal subsumption
        let parent_set: HashSet<Hash> = parent_hashes.iter().cloned().collect();
        heads.retain(|h| !parent_set.contains(&h.hash));

        // 4. Push and sort
        heads.push(new_head);
        sort_heads(&mut heads);

        // 5. Encode and store
        let encoded = encode_heads(&heads);
        self.table.insert(key, encoded.as_slice())?;

        Ok(Some(heads))
    }

    /// Read and decode heads for a key (from the write transaction).
    pub fn get_heads(&self, key: &[u8]) -> Result<Vec<Head>, KvTableError> {
        match self.table.get(key)? {
            Some(v) => {
                let mut heads = decode_heads(v.value())?;
                sort_heads(&mut heads);
                Ok(heads)
            }
            None => Ok(Vec::new()),
        }
    }

    /// Return just the head hashes for a key.
    pub fn heads(&self, key: &[u8]) -> Result<Vec<Hash>, KvTableError> {
        Ok(self.get_heads(key)?.into_iter().map(|h| h.hash).collect())
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

    /// Read and decode heads for a key.
    pub fn get_heads(&self, key: &[u8]) -> Result<Vec<Head>, KvTableError> {
        match self.table.get(key)? {
            Some(v) => {
                let mut heads = decode_heads(v.value())?;
                sort_heads(&mut heads);
                Ok(heads)
            }
            None => Ok(Vec::new()),
        }
    }

    /// Return just the head hashes for a key.
    pub fn heads(&self, key: &[u8]) -> Result<Vec<Hash>, KvTableError> {
        Ok(self.get_heads(key)?.into_iter().map(|h| h.hash).collect())
    }
}
