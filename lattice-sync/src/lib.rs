//! Set Reconciliation Protocol
//!
//! Binary-search-based protocol to efficiently find the symmetric difference
//! between two sets of intention hashes. Both sides run the same logic.
//!
//! ## Protocol
//!
//! 1. Exchange fingerprint for full range `[0x00, 0xFF)`
//! 2. If fingerprints match → done
//! 3. If they differ and combined count ≤ LEAF_THRESHOLD → exchange actual hashes
//! 4. If they differ and too large → split at byte midpoint, recurse on each half
//! 5. Diff the leaf hash sets → fetch missing intentions

use lattice_model::types::Hash;
use async_trait::async_trait;
pub use lattice_proto::network::{
    ReconcileMessage, 
    reconcile_message::Content as ReconcileContent,
    RangeFingerprint, RangeItemsRequest, RangeItemsReply
};
pub mod sync_provider;
pub use sync_provider::{SyncProvider, SyncError};

/// Maximum number of items in a range before we list hashes directly
/// instead of subdividing further.
const LEAF_THRESHOLD: u64 = 32;

/// Full range bounds.
pub const RANGE_MIN: Hash = Hash([0x00; 32]);
pub const RANGE_MAX: Hash = Hash([0xFF; 32]);

// ---------------------------------------------------------------------------
// Store abstraction
// ---------------------------------------------------------------------------

/// Trait for range queries on a set of hashes.
///
/// Used by `Reconciler` for set reconciliation. Any `SyncProvider` automatically
/// implements this trait via a blanket impl. Test mocks can implement it directly
/// with a custom error type.
#[async_trait]
pub trait RangeStore: Sync + Send {
    type Error: std::fmt::Display + Send + Sync + 'static;

    async fn count_range(&self, start: &Hash, end: &Hash) -> Result<u64, Self::Error>;
    async fn fingerprint_range(&self, start: &Hash, end: &Hash) -> Result<Hash, Self::Error>;
    async fn hashes_in_range(&self, start: &Hash, end: &Hash) -> Result<Vec<Hash>, Self::Error>;
    /// Global fingerprint of all items in the store (O(1)).
    async fn table_fingerprint(&self) -> Result<Hash, Self::Error>;
}

/// Blanket impl: any `SyncProvider` automatically satisfies `RangeStore`.
///
/// This eliminates the need to implement both traits when a type already
/// provides the range query methods through `SyncProvider`.
#[async_trait]
impl<T: SyncProvider + ?Sized> RangeStore for T {
    type Error = SyncError;

    async fn count_range(&self, start: &Hash, end: &Hash) -> Result<u64, SyncError> {
        SyncProvider::count_range(self, start, end).await
    }
    async fn fingerprint_range(&self, start: &Hash, end: &Hash) -> Result<Hash, SyncError> {
        SyncProvider::fingerprint_range(self, start, end).await
    }
    async fn hashes_in_range(&self, start: &Hash, end: &Hash) -> Result<Vec<Hash>, SyncError> {
        SyncProvider::hashes_in_range(self, start, end).await
    }
    async fn table_fingerprint(&self) -> Result<Hash, SyncError> {
        SyncProvider::table_fingerprint(self).await
    }
}

// ---------------------------------------------------------------------------
// Reconciler
// ---------------------------------------------------------------------------

/// Outcome of processing one inbound message.
pub struct ProcessResult {
    /// Messages to send to the peer.
    pub send: Vec<ReconcileMessage>,
    /// Hashes the peer has that we are missing.
    pub need: Vec<Hash>,
    /// Whether reconciliation is complete.
    pub done: bool,
}

/// Stateless reconciler — given a store and an inbound message, produces
/// outbound messages and a list of hashes we need from the peer.
pub struct Reconciler<'a, S: RangeStore> {
    store: &'a S,
}


impl<'a, S: RangeStore> Reconciler<'a, S> {
    pub fn new(store: &'a S) -> Self {
        Self { store }
    }
}

/// Error returned during reconciliation.
#[derive(Debug)]
pub enum ReconcileError<E> {
    Store(E),
    Proto(String),
}

impl<E: std::fmt::Display> std::fmt::Display for ReconcileError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReconcileError::Store(e) => write!(f, "Store error: {}", e),
            ReconcileError::Proto(s) => write!(f, "Protocol error: {}", s),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for ReconcileError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ReconcileError::Store(e) => Some(e),
            ReconcileError::Proto(_) => None,
        }
    }
}

impl<'a, S: RangeStore> Reconciler<'a, S> {

    /// Produce the initial message to start reconciliation.
    pub async fn initiate(&self) -> Result<ReconcileMessage, S::Error> {
        let fp = self.store.table_fingerprint().await?;
        let count = self.store.count_range(&RANGE_MIN, &RANGE_MAX).await?;
        Ok(ReconcileMessage {
            content: Some(ReconcileContent::RangeFingerprint(RangeFingerprint {
                start: RANGE_MIN.as_bytes().to_vec(),
                end: RANGE_MAX.as_bytes().to_vec(),
                fingerprint: fp.as_bytes().to_vec(),
                count,
            }))
        })
    }

    /// Process an inbound message from the peer and produce a response.
    pub async fn process(&self, msg: &ReconcileMessage) -> Result<ProcessResult, ReconcileError<S::Error>> {
        match &msg.content {
            Some(ReconcileContent::RangeFingerprint(p)) => {
                let start = Hash::try_from(p.start.as_slice()).map_err(|_| ReconcileError::Proto("Bad start hash".into()))?;
                let end = Hash::try_from(p.end.as_slice()).map_err(|_| ReconcileError::Proto("Bad end hash".into()))?;
                let fp = Hash::try_from(p.fingerprint.as_slice()).map_err(|_| ReconcileError::Proto("Bad fingerprint".into()))?;
                self.handle_range_fingerprint(&start, &end, &fp, p.count).await
            }
            Some(ReconcileContent::RangeItemsRequest(p)) => {
                let start = Hash::try_from(p.start.as_slice()).map_err(|_| ReconcileError::Proto("Bad start hash".into()))?;
                let end = Hash::try_from(p.end.as_slice()).map_err(|_| ReconcileError::Proto("Bad end hash".into()))?;
                let hashes = p.hashes.iter().map(|h| Hash::try_from(h.as_slice()))
                    .collect::<Result<Vec<_>, _>>().map_err(|_| ReconcileError::Proto("Bad hash in request".into()))?;
                self.handle_range_items(&start, &end, &hashes, true).await
            }
            Some(ReconcileContent::RangeItemsReply(p)) => {
                let start = Hash::try_from(p.start.as_slice()).map_err(|_| ReconcileError::Proto("Bad start hash".into()))?;
                let end = Hash::try_from(p.end.as_slice()).map_err(|_| ReconcileError::Proto("Bad end hash".into()))?;
                let hashes = p.hashes.iter().map(|h| Hash::try_from(h.as_slice()))
                    .collect::<Result<Vec<_>, _>>().map_err(|_| ReconcileError::Proto("Bad hash in reply".into()))?;
                self.handle_range_items(&start, &end, &hashes, false).await
            }
            Some(ReconcileContent::Done(_)) => {
                Ok(ProcessResult { send: vec![], need: vec![], done: true })
            }
            None => Err(ReconcileError::Proto("Empty message content".into())),
        }
    }

    async fn handle_range_fingerprint(
        &self,
        start: &Hash,
        end: &Hash,
        peer_fp: &Hash,
        peer_count: u64,
    ) -> Result<ProcessResult, ReconcileError<S::Error>> {
        let my_fp = self.store.fingerprint_range(start, end).await.map_err(ReconcileError::Store)?;
        let my_count = self.store.count_range(start, end).await.map_err(ReconcileError::Store)?;

        // Fingerprints match — this range is in sync
        if my_fp == *peer_fp {
            return Ok(ProcessResult {
                send: vec![ReconcileMessage { content: Some(ReconcileContent::Done(true)) }],
                need: vec![],
                done: true,
            });
        }

        // Small enough to enumerate directly
        if my_count + peer_count <= LEAF_THRESHOLD {
            let my_hashes = self.store.hashes_in_range(start, end).await.map_err(ReconcileError::Store)?;
            return Ok(ProcessResult {
                send: vec![ReconcileMessage {
                    content: Some(ReconcileContent::RangeItemsRequest(RangeItemsRequest {
                        start: start.as_bytes().to_vec(),
                        end: end.as_bytes().to_vec(),
                        hashes: my_hashes.iter().map(|h| h.as_bytes().to_vec()).collect(),
                    }))
                }],
                need: vec![],
                done: false,
            });
        }

        // Too large — split at midpoint and send fingerprints for each half
        let mid = midpoint(start, end);
        let fp_lo = self.store.fingerprint_range(start, &mid).await.map_err(ReconcileError::Store)?;
        let count_lo = self.store.count_range(start, &mid).await.map_err(ReconcileError::Store)?;
        let fp_hi = self.store.fingerprint_range(&mid, end).await.map_err(ReconcileError::Store)?;
        let count_hi = self.store.count_range(&mid, end).await.map_err(ReconcileError::Store)?;

        Ok(ProcessResult {
            send: vec![
                ReconcileMessage {
                    content: Some(ReconcileContent::RangeFingerprint(RangeFingerprint {
                        start: start.as_bytes().to_vec(),
                        end: mid.as_bytes().to_vec(),
                        fingerprint: fp_lo.as_bytes().to_vec(),
                        count: count_lo,
                    }))
                },
                ReconcileMessage {
                    content: Some(ReconcileContent::RangeFingerprint(RangeFingerprint {
                        start: mid.as_bytes().to_vec(),
                        end: end.as_bytes().to_vec(),
                        fingerprint: fp_hi.as_bytes().to_vec(),
                        count: count_hi,
                    }))
                },
            ],
            need: vec![],
            done: false,
        })
    }

    async fn handle_range_items(
        &self,
        start: &Hash,
        end: &Hash,
        peer_hashes: &[Hash],
        is_request: bool,
    ) -> Result<ProcessResult, ReconcileError<S::Error>> {
        let my_hashes = self.store.hashes_in_range(start, end).await.map_err(ReconcileError::Store)?;

        // Find what we're missing: in peer's set but not ours
        let my_set: std::collections::HashSet<&Hash> = my_hashes.iter().collect();
        let need: Vec<Hash> = peer_hashes.iter()
            .filter(|h| !my_set.contains(h))
            .copied()
            .collect();

        // If it's a request, reply with our items. If it's a reply, we're done.
        let send = if is_request {
            vec![ReconcileMessage {
                content: Some(ReconcileContent::RangeItemsReply(RangeItemsReply {
                    start: start.as_bytes().to_vec(),
                    end: end.as_bytes().to_vec(),
                    hashes: my_hashes.iter().map(|h| h.as_bytes().to_vec()).collect(),
                }))
            }]
        } else {
            vec![ReconcileMessage { content: Some(ReconcileContent::Done(true)) }]
        };

        Ok(ProcessResult {
            send,
            need,
            done: true,
        })
    }
}

/// Compute the byte-wise midpoint between two 32-byte hashes.
/// This is the arithmetic mean of two 256-bit unsigned integers.
/// Optimized using "Hacker's Average": (a & b) + ((a ^ b) >> 1) on u128 chunks.
fn midpoint(a: &Hash, b: &Hash) -> Hash {
    // Process as two 128-bit Big-Endian integers to match Hash lexicographical order
    // (Hash comparison treats bytes[0] as MSB)
    
    // 1. Load inputs as two u128s each (Big Endian)
    // "hi" is the first 16 bytes, "lo" is the last 16 bytes
    let a_hi = u128::from_be_bytes(a.0[0..16].try_into().unwrap());
    let a_lo = u128::from_be_bytes(a.0[16..32].try_into().unwrap());

    let b_hi = u128::from_be_bytes(b.0[0..16].try_into().unwrap());
    let b_lo = u128::from_be_bytes(b.0[16..32].try_into().unwrap());

    // 2. Average without overflow: (a & b) + ((a ^ b) >> 1)
    let mid_hi = (a_hi & b_hi) + ((a_hi ^ b_hi) >> 1);
    let mut mid_lo = (a_lo & b_lo) + ((a_lo ^ b_lo) >> 1);

    // 3. Handle the carry from high to low
    // If the LSB of the High Part's XOR term was 1, it shifts right 
    // to become the MSB of the Low Part (2^128 becomes 2^127).
    if (a_hi ^ b_hi) & 1 == 1 {
        mid_lo |= 1 << 127;
    }

    // 4. Write back to byte array
    let mut result = [0u8; 32];
    result[0..16].copy_from_slice(&mid_hi.to_be_bytes());
    result[16..32].copy_from_slice(&mid_lo.to_be_bytes());

    Hash(result)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// In-memory implementation of RangeStore for testing.
    struct MemStore {
        hashes: BTreeSet<Hash>,
    }

    impl MemStore {
        fn new(hashes: impl IntoIterator<Item = Hash>) -> Self {
            Self { hashes: hashes.into_iter().collect() }
        }
    }

    #[async_trait]
    impl RangeStore for MemStore {
        type Error = std::io::Error;

        async fn count_range(&self, start: &Hash, end: &Hash) -> Result<u64, Self::Error> {
            Ok(self.hashes.range(start..end).count() as u64)
        }

        async fn fingerprint_range(&self, start: &Hash, end: &Hash) -> Result<Hash, Self::Error> {
            let mut fp = [0u8; 32];
            for h in self.hashes.range(start..end) {
                for (i, byte) in h.0.iter().enumerate() {
                    fp[i] ^= byte;
                }
            }
            Ok(Hash(fp))
        }

        async fn hashes_in_range(&self, start: &Hash, end: &Hash) -> Result<Vec<Hash>, Self::Error> {
            Ok(self.hashes.range(start..end).copied().collect())
        }

        async fn table_fingerprint(&self) -> Result<Hash, Self::Error> {
            self.fingerprint_range(&RANGE_MIN, &RANGE_MAX).await
        }
    }

    /// Make a deterministic hash from a single byte.
    fn hash(val: u8) -> Hash {
        Hash(*blake3::hash(&[val]).as_bytes())
    }

    /// Run the full reconciliation protocol between two stores.
    /// Returns (hashes A needs, hashes B needs).
    async fn run_reconciliation(a: &MemStore, b: &MemStore) -> (Vec<Hash>, Vec<Hash>) {
        let ra = Reconciler::new(a);
        let rb = Reconciler::new(b);

        let mut a_needs = Vec::new();
        let mut b_needs = Vec::new();

        // A initiates
        let init = ra.initiate().await.unwrap();
        let mut a_outbox: Vec<ReconcileMessage> = vec![init];
        let mut b_outbox: Vec<ReconcileMessage>;

        for _ in 0..20 {
            // B processes A's messages
            let mut next_b_out = vec![];
            for msg in &a_outbox {
                let result = rb.process(msg).await.unwrap();
                next_b_out.extend(result.send);
                b_needs.extend(result.need);
            }
            b_outbox = next_b_out;

            // A processes B's messages
            let mut next_a_out = vec![];
            for msg in &b_outbox {
                let result = ra.process(msg).await.unwrap();
                next_a_out.extend(result.send);
                a_needs.extend(result.need);
            }
            a_outbox = next_a_out;

            if a_outbox.iter().all(|m| matches!(m.content, Some(ReconcileContent::Done(_)))) && 
               b_outbox.iter().all(|m| matches!(m.content, Some(ReconcileContent::Done(_)))) &&
               !a_outbox.is_empty() && !b_outbox.is_empty() {
                break;
            }
        }

        (a_needs, b_needs)
    }

    #[tokio::test]
    async fn identical_stores() {
        let items: Vec<Hash> = (0..10).map(hash).collect();
        let a = MemStore::new(items.clone());
        let b = MemStore::new(items);

        let (a_needs, b_needs) = run_reconciliation(&a, &b).await;
        assert!(a_needs.is_empty());
        assert!(b_needs.is_empty());
    }

    #[tokio::test]
    async fn empty_stores() {
        let a = MemStore::new(vec![]);
        let b = MemStore::new(vec![]);

        let (a_needs, b_needs) = run_reconciliation(&a, &b).await;
        assert!(a_needs.is_empty());
        assert!(b_needs.is_empty());
    }

    #[tokio::test]
    async fn one_empty_one_populated() {
        let items: Vec<Hash> = (0..5).map(hash).collect();
        let a = MemStore::new(vec![]);
        let b = MemStore::new(items.clone());

        let (a_needs, b_needs) = run_reconciliation(&a, &b).await;

        let needed: BTreeSet<Hash> = a_needs.into_iter().collect();
        let expected: BTreeSet<Hash> = items.into_iter().collect();
        assert_eq!(needed, expected);
        assert!(b_needs.is_empty());
    }

    #[tokio::test]
    async fn partial_overlap() {
        // A has 0..8, B has 5..13
        let a_items: Vec<Hash> = (0..8).map(hash).collect();
        let b_items: Vec<Hash> = (5..13).map(hash).collect();
        let a = MemStore::new(a_items.clone());
        let b = MemStore::new(b_items.clone());

        let (a_needs, b_needs) = run_reconciliation(&a, &b).await;

        // A needs hashes 8..13 (ones B has that A doesn't)
        let a_needed: BTreeSet<Hash> = a_needs.into_iter().collect();
        let a_expected: BTreeSet<Hash> = (8..13).map(hash).collect();
        assert_eq!(a_needed, a_expected);

        // B needs hashes 0..5 (ones A has that B doesn't)
        let b_needed: BTreeSet<Hash> = b_needs.into_iter().collect();
        let b_expected: BTreeSet<Hash> = (0..5).map(hash).collect();
        assert_eq!(b_needed, b_expected);
    }

    #[tokio::test]
    async fn single_difference() {
        let common: Vec<Hash> = (0..20).map(hash).collect();
        let extra = hash(99);

        let a = MemStore::new(common.iter().copied().chain(std::iter::once(extra)));
        let b = MemStore::new(common);

        let (a_needs, b_needs) = run_reconciliation(&a, &b).await;
        assert!(a_needs.is_empty());
        assert_eq!(b_needs.len(), 1);
        assert_eq!(b_needs[0], extra);
    }

    #[tokio::test]
    async fn large_set_few_differences() {
        // 200 common items, 5 unique to each side
        let common: Vec<Hash> = (0u16..200).map(|i| {
            Hash(*blake3::hash(&i.to_le_bytes()).as_bytes())
        }).collect();
        let a_extra: Vec<Hash> = (1000u16..1005).map(|i| {
            Hash(*blake3::hash(&i.to_le_bytes()).as_bytes())
        }).collect();
        let b_extra: Vec<Hash> = (2000u16..2005).map(|i| {
            Hash(*blake3::hash(&i.to_le_bytes()).as_bytes())
        }).collect();

        let a = MemStore::new(common.iter().chain(a_extra.iter()).copied());
        let b = MemStore::new(common.iter().chain(b_extra.iter()).copied());

        let (a_needs, b_needs) = run_reconciliation(&a, &b).await;

        let a_needed: BTreeSet<Hash> = a_needs.into_iter().collect();
        let b_needed: BTreeSet<Hash> = b_needs.into_iter().collect();
        assert_eq!(a_needed, b_extra.into_iter().collect());
        assert_eq!(b_needed, a_extra.into_iter().collect());
    }

    #[test]
    fn midpoint_sanity() {
        let lo = Hash([0x00; 32]);
        let hi = Hash([0xFF; 32]);
        let mid = midpoint(&lo, &hi);
        // Midpoint of 0x00 and 0xFF is 0x7F (with possible rounding)
        // With floor division, (0 + 255) / 2 = 127 = 0x7F
        assert_eq!(mid.0[0], 0x7F);
        assert!(mid.0 > lo.0);
        assert!(mid.0 < hi.0);
    }
}
