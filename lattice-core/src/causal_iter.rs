//! Causal Entry Iterator - yields entries in HLC (causal) order
//!
//! Implements merge-sort streaming across multiple author queues using a min-heap,
//! ensuring entries are returned in correct causal order for sync.
//! Complexity: O(N log K) where N = total entries, K = number of authors.

use crate::proto::{Entry, SignedEntry};
use prost::Message;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, VecDeque};

/// A heap entry that wraps an author queue index and the HLC of its front entry.
/// Uses Reverse for min-heap behavior (lowest HLC first).
struct HeapEntry {
    hlc: (u64, u32),
    queue_idx: usize,
}

impl PartialEq for HeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.hlc == other.hlc
    }
}

impl Eq for HeapEntry {}

impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse order for min-heap (BinaryHeap is max-heap by default)
        other.hlc.cmp(&self.hlc)
    }
}

/// Iterator that yields SignedEntry in HLC (causal) order.
/// 
/// Takes multiple VecDeques (one per author) and yields entries
/// from lowest to highest HLC, ensuring causal ordering for sync.
/// Uses a min-heap for O(log K) per-entry overhead instead of O(K) linear scan.
pub struct CausalEntryIter {
    queues: Vec<VecDeque<SignedEntry>>,
    heap: BinaryHeap<HeapEntry>,
}

impl CausalEntryIter {
    /// Create a new iterator from a list of entry queues (one per author)
    pub fn new(queues: Vec<VecDeque<SignedEntry>>) -> Self {
        let mut heap = BinaryHeap::with_capacity(queues.len());
        
        // Initialize heap with the front entry from each non-empty queue
        for (idx, queue) in queues.iter().enumerate() {
            if let Some(entry) = queue.front() {
                heap.push(HeapEntry {
                    hlc: Self::get_hlc(entry),
                    queue_idx: idx,
                });
            }
        }
        
        Self { queues, heap }
    }
    
    /// Extract HLC (wall_time, counter) from a SignedEntry
    fn get_hlc(entry: &SignedEntry) -> (u64, u32) {
        Entry::decode(&entry.entry_bytes[..])
            .ok()
            .and_then(|e| e.timestamp)
            .map(|t| (t.wall_time, t.counter))
            .unwrap_or((0, 0))
    }
}

impl Iterator for CausalEntryIter {
    type Item = SignedEntry;
    
    fn next(&mut self) -> Option<Self::Item> {
        // Pop the queue with lowest HLC
        let HeapEntry { queue_idx, .. } = self.heap.pop()?;
        
        // Remove entry from that queue
        let entry = self.queues[queue_idx].pop_front()?;
        
        // If queue still has entries, push its new front back to heap
        if let Some(next_entry) = self.queues[queue_idx].front() {
            self.heap.push(HeapEntry {
                hlc: Self::get_hlc(next_entry),
                queue_idx,
            });
        }
        
        Some(entry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hlc::HLC;
    use crate::clock::MockClock;
    use crate::node_identity::NodeIdentity;
    use crate::signed_entry::EntryBuilder;
    
    fn make_entry(node: &NodeIdentity, seq: u64, clock_ms: u64) -> SignedEntry {
        let clock = MockClock::new(clock_ms);
        EntryBuilder::new(seq, HLC::now_with_clock(&clock))
            .store_id(vec![0u8; 16])
            .prev_hash(vec![0u8; 32])
            .put(b"/test".to_vec(), format!("seq{}", seq).into_bytes())
            .sign(node)
    }
    
    #[test]
    fn test_empty_iter() {
        let iter = CausalEntryIter::new(vec![]);
        assert_eq!(iter.count(), 0);
    }
    
    #[test]
    fn test_single_queue() {
        let node = NodeIdentity::generate();
        let entries: VecDeque<_> = vec![
            make_entry(&node, 1, 1000),
            make_entry(&node, 2, 2000),
        ].into();
        
        let iter = CausalEntryIter::new(vec![entries]);
        let result: Vec<_> = iter.collect();
        assert_eq!(result.len(), 2);
    }
    
    #[test]
    fn test_merge_multiple_queues() {
        let node_a = NodeIdentity::generate();
        let node_b = NodeIdentity::generate();
        
        // Author A: entries at time 1000, 3000
        let queue_a: VecDeque<_> = vec![
            make_entry(&node_a, 1, 1000),
            make_entry(&node_a, 2, 3000),
        ].into();
        
        // Author B: entries at time 2000
        let queue_b: VecDeque<_> = vec![
            make_entry(&node_b, 1, 2000),
        ].into();
        
        let iter = CausalEntryIter::new(vec![queue_a, queue_b]);
        let result: Vec<_> = iter.collect();
        
        // Should be in HLC order: 1000, 2000, 3000
        assert_eq!(result.len(), 3);
        
        // Verify order by checking HLC values
        let hlcs: Vec<_> = result.iter()
            .map(|e| CausalEntryIter::get_hlc(e))
            .collect();
        assert_eq!(hlcs[0].0, 1000);
        assert_eq!(hlcs[1].0, 2000);
        assert_eq!(hlcs[2].0, 3000);
    }
    
    #[test]
    fn test_many_authors() {
        // Test with 10 authors to verify heap behavior
        let nodes: Vec<_> = (0..10).map(|_| NodeIdentity::generate()).collect();
        let queues: Vec<VecDeque<_>> = nodes.iter().enumerate().map(|(i, node)| {
            vec![make_entry(node, 1, (i * 100 + 50) as u64)].into()
        }).collect();
        
        let iter = CausalEntryIter::new(queues);
        let result: Vec<_> = iter.collect();
        
        assert_eq!(result.len(), 10);
        
        // Verify strictly increasing HLC order
        let hlcs: Vec<_> = result.iter()
            .map(|e| CausalEntryIter::get_hlc(e).0)
            .collect();
        for window in hlcs.windows(2) {
            assert!(window[0] < window[1], "HLCs should be strictly increasing");
        }
    }
}

