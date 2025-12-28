//! Sync state for causality tracking and reconciliation

use std::collections::HashMap;

/// Author ID type (32-byte Ed25519 public key)
pub type Author = [u8; 32];

/// Per-author sync information: seq, head hash, max HLC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorInfo {
    pub seq: u64,
    pub hash: [u8; 32],             // Head hash (tip of sigchain)
    pub hlc: Option<(u64, u32)>,    // (wall_time, counter) - max HLC from this author
}

impl AuthorInfo {
    pub fn new(seq: u64, hash: [u8; 32]) -> Self {
        Self { seq, hash, hlc: None }
    }
    
    pub fn with_hlc(seq: u64, hash: [u8; 32], hlc: Option<(u64, u32)>) -> Self {
        Self { seq, hash, hlc }
    }
}

/// Sync state tracking per-author sequence numbers and head hashes.
///
/// Used during reconciliation to identify missing entries between peers.
/// Each author has exactly one head (tip of their sigchain).
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

/// Bidirectional result of comparing two sync states.
/// Captures what both sides are missing relative to each other.
#[derive(Debug, Clone, Default)]
pub struct SyncDiscrepancy {
    /// Entries we need from them
    pub entries_we_need: u64,
    /// Entries they need from us
    pub entries_they_need: u64,
}

impl SyncDiscrepancy {
    /// Returns true if either side has missing entries.
    pub fn is_out_of_sync(&self) -> bool {
        self.entries_we_need > 0 || self.entries_they_need > 0
    }
}

/// Event emitted when sync is needed with a peer.
/// Broadcast by Store when states are out of sync.
#[derive(Debug, Clone)]
pub struct SyncNeeded {
    /// Peer public key bytes
    pub peer: [u8; 32],
    /// Discrepancy between our state and theirs
    pub discrepancy: SyncDiscrepancy,
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
    
    /// Get head hash for an author (returns zero hash if not present).
    pub fn hash(&self, author: &Author) -> [u8; 32] {
        self.authors.get(author).map(|i| i.hash).unwrap_or([0u8; 32])
    }

    /// Set the info for an author.
    pub fn set(&mut self, author: Author, seq: u64, hash: [u8; 32]) {
        self.authors.insert(author, AuthorInfo::new(seq, hash));
    }
    
    /// Set the info for an author with HLC.
    pub fn set_with_hlc(&mut self, author: Author, seq: u64, hash: [u8; 32], hlc: Option<(u64, u32)>) {
        self.authors.insert(author, AuthorInfo::with_hlc(seq, hash, hlc));
    }

    /// Get all authors and their info.
    pub fn authors(&self) -> &HashMap<Author, AuthorInfo> {
        &self.authors
    }
    
    /// Compute the "common HLC" - the minimum of max HLCs across all authors.
    /// This represents the point at which all logs are synchronized.
    /// Returns None if any author has no HLC or there are no authors.
    pub fn common_hlc(&self) -> Option<(u64, u32)> {
        if self.authors.is_empty() {
            return None;
        }
        
        let mut min_hlc: Option<(u64, u32)> = None;
        for info in self.authors.values() {
            match (min_hlc, info.hlc) {
                (None, Some(hlc)) => min_hlc = Some(hlc),
                (Some(current), Some(hlc)) => {
                    // Compare: first by wall_time, then by counter
                    if hlc.0 < current.0 || (hlc.0 == current.0 && hlc.1 < current.1) {
                        min_hlc = Some(hlc);
                    }
                }
                (_, None) => return None, // Author without HLC = no common HLC
            }
        }
        min_hlc
    }

    /// Compute what entries we're missing compared to a peer's state.
    ///
    /// Returns ranges of entries we need from the peer.
    /// Compares hash sets when seq matches to detect forks.
    pub fn diff(&self, peer: &SyncState) -> Vec<MissingRange> {
        let mut missing = Vec::new();

        for (author, peer_info) in peer.authors() {
            let my_seq = self.seq(author);
            let my_hash = self.hash(author);
            
            // We need entries if:
            // 1. Peer's seq is higher than ours, OR
            // 2. Peer's seq equals ours but they have a different hash (divergence)
            let need_entries = if peer_info.seq > my_seq {
                true
            } else if peer_info.seq == my_seq && my_seq > 0 {
                // Same seq - check if hashes differ (shouldn't happen in sigchain)
                peer_info.hash != my_hash
            } else {
                false
            };
            
            if need_entries {
                missing.push(MissingRange {
                    author: *author,
                    from_seq: my_seq,
                    from_hash: my_hash,
                    to_seq: peer_info.seq,
                });
            }
        }

        missing
    }
    
    /// Calculate discrepancy between local and peer state.
    /// 
    /// Returns a bidirectional summary of what each side is missing.
    pub fn calculate_discrepancy(&self, peer: &SyncState) -> SyncDiscrepancy {
        let we_need = self.diff(peer);
        let they_need = peer.diff(self);
        
        // from_seq is "I have up to X", to_seq is "peer has up to Y"
        // Missing entries = Y - X
        let entries_we_need: u64 = we_need.iter().map(|r| r.to_seq - r.from_seq).sum();
        let entries_they_need: u64 = they_need.iter().map(|r| r.to_seq - r.from_seq).sum();
        
        SyncDiscrepancy {
            entries_we_need,
            entries_they_need,
        }
    }

    /// Merge another sync state into this one (takes max seq).
    pub fn merge(&mut self, other: &SyncState) {
        for (author, info) in other.authors() {
            if let Some(my_info) = self.authors.get_mut(author) {
                // Take max seq and associated hash
                if info.seq > my_info.seq {
                    my_info.seq = info.seq;
                    my_info.hash = info.hash;
                    my_info.hlc = info.hlc;
                }
            } else {
                self.authors.insert(*author, info.clone());
            }
        }
    }

    /// Convert to proto message for network transmission
    pub fn to_proto(&self) -> crate::proto::storage::SyncState {
        let authors = self.authors.iter().map(|(author, info)| {
            crate::proto::storage::SyncAuthor {
                author_id: author.to_vec(),
                state: Some(crate::proto::storage::AuthorState {
                    seq: info.seq,
                    hash: info.hash.to_vec(),
                    hlc: info.hlc.map(|(wall_time, counter)| crate::proto::storage::Hlc { wall_time, counter }),
                }),
            }
        }).collect();
        
        // Compute common HLC (min of max HLCs across all authors)
        let common_hlc = self.common_hlc()
            .map(|(wall_time, counter)| crate::proto::storage::Hlc { wall_time, counter });
        
        crate::proto::storage::SyncState {
            authors,
            sender_hlc: None,
            common_hlc,
        }
    }

    /// Create from proto message
    pub fn from_proto(proto: &crate::proto::storage::SyncState) -> Self {
        let mut state = Self::new();
        
        for sync_author in &proto.authors {
            if sync_author.author_id.len() == 32 {
                let mut author = [0u8; 32];
                author.copy_from_slice(&sync_author.author_id);
                
                if let Some(author_state) = &sync_author.state {
                    let mut hash = [0u8; 32];
                    if author_state.hash.len() == 32 {
                        hash.copy_from_slice(&author_state.hash);
                    }
                    let hlc = author_state.hlc.as_ref().map(|h| (h.wall_time, h.counter));
                    state.set_with_hlc(author, author_state.seq, hash, hlc);
                }
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
    
    /// Test calculate_discrepancy counts entries correctly.
    /// from_seq = "I have up to X", to_seq = "peer has up to Y"
    /// Missing entries = Y - X
    #[test]
    fn test_calculate_discrepancy() {
        let mut local = SyncState::new();
        let author_a = [1u8; 32];
        // Local has author A: entries 1..=5
        local.set(author_a, 5, [0xAA; 32]);
        
        let mut peer = SyncState::new();
        // Peer has author A: entries 1..=10 (we're missing 6,7,8,9,10 = 5 entries)
        peer.set(author_a, 10, [0xBB; 32]);
        // Peer has author B: entry 1 (we have nothing, missing 1 entry)
        let author_b = [2u8; 32];
        peer.set(author_b, 1, [0xCC; 32]);
        
        let discrepancy = local.calculate_discrepancy(&peer);
        
        // We need: Author A: 10-5=5, Author B: 1-0=1, Total: 6
        // They need: nothing (they have everything we have and more)
        assert_eq!(discrepancy.entries_we_need, 6, "Should correctly count entries we need");
        assert_eq!(discrepancy.entries_they_need, 0, "Peer has everything we have");
        assert!(discrepancy.is_out_of_sync(), "Should be out of sync");
    }
}
