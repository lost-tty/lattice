//! Sync state for causality tracking and reconciliation

use std::collections::HashMap;

/// Author ID type (32-byte Ed25519 public key)
pub type Author = [u8; 32];

/// Per-author sync information (seq + hash for resume).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorInfo {
    pub seq: u64,
    pub hash: [u8; 32],
}

/// Sync state tracking per-author sequence numbers and hashes.
///
/// Used during reconciliation to identify missing entries between peers.
/// Each author's highest seen sequence number and hash is tracked.
#[derive(Debug, Clone, Default)]
pub struct SyncState {
    authors: HashMap<Author, AuthorInfo>,
}

/// Describes entries needed from a peer for a specific author.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingRange {
    pub author: Author,
    pub from_seq: u64,        // exclusive - we have up to this
    pub from_hash: [u8; 32],  // hash to resume reading after
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

    /// Set the info for an author.
    pub fn set(&mut self, author: Author, seq: u64, hash: [u8; 32]) {
        self.authors.insert(author, AuthorInfo { seq, hash });
    }

    /// Get all authors and their info.
    pub fn authors(&self) -> &HashMap<Author, AuthorInfo> {
        &self.authors
    }

    /// Compute what entries we're missing compared to a peer's state.
    ///
    /// Returns ranges of entries we need from the peer.
    /// Each range includes the hash to resume reading after.
    pub fn diff(&self, peer: &SyncState) -> Vec<MissingRange> {
        let mut missing = Vec::new();

        for (author, peer_info) in peer.authors() {
            let my_seq = self.seq(author);
            if peer_info.seq > my_seq {
                // We need entries from my_seq+1 to peer_info.seq
                // Use our hash (or zero if we have nothing) as resume point
                let from_hash = self.get(author)
                    .map(|i| i.hash)
                    .unwrap_or([0u8; 32]);
                
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

    /// Merge another sync state into this one (take max seq per author).
    pub fn merge(&mut self, other: &SyncState) {
        for (author, info) in other.authors() {
            let my_seq = self.seq(author);
            if info.seq > my_seq {
                self.set(*author, info.seq, info.hash);
            }
        }
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
}
