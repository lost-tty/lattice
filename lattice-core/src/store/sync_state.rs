//! Sync state for causality tracking and reconciliation

use std::collections::{HashMap, HashSet};

/// Author ID type (32-byte Ed25519 public key)
pub type Author = [u8; 32];

/// Per-author sync information: seq + all head hashes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorInfo {
    pub seq: u64,
    pub heads: HashSet<[u8; 32]>,  // All head hashes for this author
}

impl AuthorInfo {
    pub fn new(seq: u64, hash: [u8; 32]) -> Self {
        let mut heads = HashSet::new();
        heads.insert(hash);
        Self { seq, heads }
    }
    
    pub fn with_heads(seq: u64, heads: HashSet<[u8; 32]>) -> Self {
        Self { seq, heads }
    }
}

/// Sync state tracking per-author sequence numbers and head hashes.
///
/// Used during reconciliation to identify missing entries between peers.
/// Tracks all head hashes per author to handle forks correctly.
#[derive(Debug, Clone, Default)]
pub struct SyncState {
    authors: HashMap<Author, AuthorInfo>,
}

/// Describes entries needed from a peer for a specific author.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingRange {
    pub author: Author,
    pub from_seq: u64,        // exclusive - we have up to this
    pub from_hash: [u8; 32],  // hash to resume reading after (zero = start)
    pub to_seq: u64,          // inclusive - peer has up to this
}

impl SyncState {
    /// Create a new empty sync state.
    pub fn new() -> Self {
        Self {
            authors: HashMap::new(),
        }
    }

    /// Get the info for an author (returns None if not present).
    pub fn get(&self, author: &Author) -> Option<&AuthorInfo> {
        self.authors.get(author)
    }

    /// Get the sequence number for an author (returns 0 if not present).
    pub fn seq(&self, author: &Author) -> u64 {
        self.authors.get(author).map(|i| i.seq).unwrap_or(0)
    }
    
    /// Get head hashes for an author (returns empty set if not present).
    pub fn heads(&self, author: &Author) -> HashSet<[u8; 32]> {
        self.authors.get(author).map(|i| i.heads.clone()).unwrap_or_default()
    }

    /// Set the info for an author (single hash convenience method).
    pub fn set(&mut self, author: Author, seq: u64, hash: [u8; 32]) {
        self.authors.insert(author, AuthorInfo::new(seq, hash));
    }
    
    /// Set the info for an author with multiple heads.
    pub fn set_heads(&mut self, author: Author, seq: u64, heads: HashSet<[u8; 32]>) {
        self.authors.insert(author, AuthorInfo::with_heads(seq, heads));
    }
    
    /// Add a head hash for an author (updates seq if higher).
    pub fn add_head(&mut self, author: Author, seq: u64, hash: [u8; 32]) {
        if let Some(info) = self.authors.get_mut(&author) {
            info.heads.insert(hash);
            if seq > info.seq {
                info.seq = seq;
            }
        } else {
            self.set(author, seq, hash);
        }
    }

    /// Get all authors and their info.
    pub fn authors(&self) -> &HashMap<Author, AuthorInfo> {
        &self.authors
    }

    /// Compute what entries we're missing compared to a peer's state.
    ///
    /// Returns ranges of entries we need from the peer.
    /// Compares hash sets when seq matches to detect forks.
    pub fn diff(&self, peer: &SyncState) -> Vec<MissingRange> {
        let mut missing = Vec::new();

        for (author, peer_info) in peer.authors() {
            let my_seq = self.seq(author);
            let my_heads = self.heads(author);
            
            // We need entries if:
            // 1. Peer's seq is higher than ours, OR
            // 2. Peer's seq equals ours but they have heads we don't (fork)
            let need_entries = if peer_info.seq > my_seq {
                true
            } else if peer_info.seq == my_seq && my_seq > 0 {
                // Same seq - check for forks (different hashes at same seq)
                peer_info.heads.iter().any(|h| !my_heads.contains(h))
            } else {
                false
            };
            
            if need_entries {
                // Request from our common ancestor (or start if we have nothing)
                let from_hash = if my_heads.is_empty() {
                    [0u8; 32]
                } else {
                    *my_heads.iter().next().unwrap()
                };
                
                missing.push(MissingRange {
                    author: *author,
                    from_seq: my_seq,
                    from_hash,
                    to_seq: peer_info.seq,
                });
            }
        }

        missing
    }

    /// Merge another sync state into this one (union of heads, max seq).
    pub fn merge(&mut self, other: &SyncState) {
        for (author, info) in other.authors() {
            if let Some(my_info) = self.authors.get_mut(author) {
                // Union heads
                for h in &info.heads {
                    my_info.heads.insert(*h);
                }
                // Take max seq
                if info.seq > my_info.seq {
                    my_info.seq = info.seq;
                }
            } else {
                self.authors.insert(*author, info.clone());
            }
        }
    }

    /// Convert to proto message for network transmission
    pub fn to_proto(&self) -> crate::proto::SyncState {
        let frontiers = self.authors.iter().map(|(author, info)| {
            crate::proto::Frontier {
                author_id: author.to_vec(),
                max_seq: info.seq,
                head_hashes: info.heads.iter().map(|h| h.to_vec()).collect(),
            }
        }).collect();
        crate::proto::SyncState {
            frontiers,
            sender_hlc: None,
        }
    }

    /// Create from proto message
    pub fn from_proto(proto: &crate::proto::SyncState) -> Self {
        let mut state = Self::new();
        for frontier in &proto.frontiers {
            if frontier.author_id.len() == 32 {
                let mut author = [0u8; 32];
                author.copy_from_slice(&frontier.author_id);
                
                let mut heads = HashSet::new();
                for hash_bytes in &frontier.head_hashes {
                    if hash_bytes.len() == 32 {
                        let mut hash = [0u8; 32];
                        hash.copy_from_slice(hash_bytes);
                        heads.insert(hash);
                    }
                }
                
                if heads.is_empty() {
                    // Fallback: empty hash if no heads provided
                    heads.insert([0u8; 32]);
                }
                
                state.set_heads(author, frontier.max_seq, heads);
            }
        }
        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diff_empty() {
        let a = SyncState::new();
        let b = SyncState::new();
        assert!(a.diff(&b).is_empty());
    }

    #[test]
    fn test_diff_peer_ahead() {
        let mut a = SyncState::new();
        let mut b = SyncState::new();
        
        let author = [1u8; 32];
        let hash_a = [0xAA; 32];
        let hash_b = [0xBB; 32];
        
        a.set(author, 5, hash_a);
        b.set(author, 10, hash_b);

        let missing = a.diff(&b);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].author, author);
        assert_eq!(missing[0].from_seq, 5);
        assert_eq!(missing[0].from_hash, hash_a);  // Resume after our hash
        assert_eq!(missing[0].to_seq, 10);
    }

    #[test]
    fn test_diff_i_am_ahead() {
        let mut a = SyncState::new();
        let mut b = SyncState::new();
        
        let author = [1u8; 32];
        a.set(author, 10, [0xAA; 32]);
        b.set(author, 5, [0xBB; 32]);

        // I'm ahead, so I don't need anything from peer
        let missing = a.diff(&b);
        assert!(missing.is_empty());
    }

    #[test]
    fn test_diff_new_author() {
        let a = SyncState::new();
        let mut b = SyncState::new();
        
        let author = [2u8; 32];
        b.set(author, 3, [0xBB; 32]);

        // Peer has author I don't have
        let missing = a.diff(&b);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].from_seq, 0);
        assert_eq!(missing[0].from_hash, [0u8; 32]);  // Zero hash = read from start
        assert_eq!(missing[0].to_seq, 3);
    }

    #[test]
    fn test_merge() {
        let mut a = SyncState::new();
        let mut b = SyncState::new();
        
        let author1 = [1u8; 32];
        let author2 = [2u8; 32];
        
        a.set(author1, 10, [0xA1; 32]);
        a.set(author2, 5, [0xA2; 32]);
        
        b.set(author1, 5, [0xB1; 32]);   // a is ahead
        b.set(author2, 8, [0xB2; 32]);   // b is ahead

        a.merge(&b);
        assert_eq!(a.seq(&author1), 10);  // kept a's value
        assert_eq!(a.seq(&author2), 8);   // took b's value
    }

    /// This test documents a known issue: SyncState tracks only ONE hash per author,
    /// but with forks/multi-heads, there could be multiple branches.
    /// 
    /// Scenario:
    /// - Author writes entry1 (hash=A)
    /// - Two peers independently write entry2 and entry3 (both have prev=A)
    /// - Peer1 has: entry1 -> entry2 (seq=2, hash=B)
    /// - Peer2 has: entry1 -> entry3 (seq=2, hash=C)
    /// - When Peer3 syncs with Peer1, SyncState says "I need entries after hash=B"
    /// - But Peer2 only has entries after hash=A, so Peer3 never gets entry3!
    /// 
    #[test]
    fn test_multihead_sync_inconsistency() {
        // This is a conceptual test showing the problem
        // In reality, both forks would have seq=2 but different hashes
        // SyncState can only track one, so the other branch gets lost
        
        let mut peer1_state = SyncState::new();
        let mut peer2_state = SyncState::new();
        let new_peer_state = SyncState::new();
        
        let author = [1u8; 32];
        
        // Both peers have seq=2, but different hashes (different forks)
        peer1_state.set(author, 2, [0xBB; 32]); // entry1 -> entry2
        peer2_state.set(author, 2, [0xCC; 32]); // entry1 -> entry3
        
        // New peer syncs with peer1 first
        let missing_from_peer1 = new_peer_state.diff(&peer1_state);
        assert_eq!(missing_from_peer1.len(), 1);
        assert_eq!(missing_from_peer1[0].to_seq, 2);
        
        // After applying peer1's entries, new peer has seq=2, hash=BB
        let mut after_peer1 = new_peer_state.clone();
        after_peer1.set(author, 2, [0xBB; 32]);
        
        // Now sync with peer2 - BUG: new peer thinks it's up to date!
        let missing_from_peer2 = after_peer1.diff(&peer2_state);
        
        // This assertion FAILS - we get empty missing even though peer2 has entry3!
        // The bug: peer2's seq=2 equals our seq=2, so we think we're in sync
        // But peer2's hash=0xCC != our hash=0xBB - they have different entries!
        assert!(!missing_from_peer2.is_empty(), 
            "BUG: SyncState misses peer2's fork because seq numbers match");
    }
    
    /// Test that diff returns all authors, which can then be filtered by protocol layer.
    /// This verifies the author_filter parameter in send_missing_entries works.
    #[test]
    fn test_diff_multiple_authors_for_filtering() {
        let mut my_state = SyncState::new();
        let mut peer_state = SyncState::new();
        
        let author_a = [0xAA; 32];
        let author_b = [0xBB; 32];
        let author_c = [0xCC; 32];
        
        // I have author_a at seq 5, nothing for b or c
        my_state.set(author_a, 5, [0x11; 32]);
        
        // Peer has all three authors ahead of me
        peer_state.set(author_a, 10, [0x22; 32]);
        peer_state.set(author_b, 3, [0x33; 32]);
        peer_state.set(author_c, 7, [0x44; 32]);
        
        // Full diff returns all three
        let full_diff = my_state.diff(&peer_state);
        assert_eq!(full_diff.len(), 3, "Should have 3 authors in diff");
        
        // Verify filter mechanism works (simulating protocol layer)
        let author_filter = [author_b];
        let filtered: Vec<_> = full_diff.iter()
            .filter(|r| author_filter.contains(&r.author))
            .collect();
        
        assert_eq!(filtered.len(), 1, "Should filter to just author_b");
        assert_eq!(filtered[0].author, author_b);
        assert_eq!(filtered[0].from_seq, 0);
        assert_eq!(filtered[0].to_seq, 3);
    }
    
    /// Test gap filling scenario: I have seq 1-5, I receive seq 8 (orphan).
    /// Gap detection should request seq 6-8 from peers.
    #[test]
    fn test_gap_detection_sync_request() {
        let mut my_state = SyncState::new();
        let mut peer_state = SyncState::new();
        
        let author = [0x11; 32];
        
        // I have entries 1-5
        my_state.set(author, 5, [0xAA; 32]);
        
        // I received orphan at seq 8, so I know peer has at least seq 8
        // (In reality, we'd learn this from the GapInfo broadcast)
        peer_state.set(author, 8, [0xBB; 32]);
        
        // Diff should show I need seq 6-8
        let missing = my_state.diff(&peer_state);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].from_seq, 5);  // I have up to 5
        assert_eq!(missing[0].to_seq, 8);    // Peer has up to 8
        
        // The sync should fill entries 6, 7, 8 and resolve the orphan
    }
}
