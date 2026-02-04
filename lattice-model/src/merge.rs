//! Merge strategies for resolving multi-head conflicts.
//!
//! Extension traits that add merge methods to head collections.

use crate::head::Head;

/// Merge strategies for a single key's heads.
pub trait Merge {
    /// Last-Write-Wins: returns value with highest HLC.
    fn lww(&self) -> Option<Vec<u8>>;
    
    /// First-Write-Wins: returns value with lowest HLC.
    fn fww(&self) -> Option<Vec<u8>>;
    
    /// All values sorted by HLC (newest first), excluding tombstones.
    fn all(&self) -> Vec<Vec<u8>>;
    
    /// LWW returning full Head (for when you need metadata).
    fn lww_head(&self) -> Option<&Head>;
    
    /// FWW returning full Head.
    fn fww_head(&self) -> Option<&Head>;
    
    /// All heads sorted by HLC (newest first), INCLUDING tombstones.
    fn all_heads(&self) -> Vec<&Head>;
}

impl Merge for [Head] {
    fn lww(&self) -> Option<Vec<u8>> {
        self.lww_head().map(|h| h.value.clone())
    }
    
    fn fww(&self) -> Option<Vec<u8>> {
        self.fww_head().map(|h| h.value.clone())
    }
    
    fn all(&self) -> Vec<Vec<u8>> {
        self.all_heads().into_iter()
            .filter(|h| !h.tombstone)
            .map(|h| h.value.clone())
            .collect()
    }
    
    fn lww_head(&self) -> Option<&Head> {
        let winner = self.iter()
            .max_by(|a, b| a.hlc.cmp(&b.hlc).then_with(|| a.author.cmp(&b.author)))?;
            
        if winner.tombstone {
            None
        } else {
            Some(winner)
        }
    }
    
    fn fww_head(&self) -> Option<&Head> {
        let winner = self.iter()
            .min_by(|a, b| a.hlc.cmp(&b.hlc).then_with(|| a.author.cmp(&b.author)))?;
            
        if winner.tombstone {
            None
        } else {
            Some(winner)
        }
    }
    
    fn all_heads(&self) -> Vec<&Head> {
        let mut sorted: Vec<_> = self.iter().collect();
        // Newest HLC first
        sorted.sort_by(|a, b| b.hlc.cmp(&a.hlc).then_with(|| b.author.cmp(&a.author)));
        sorted
    }
}

impl Merge for Vec<Head> {
    fn lww(&self) -> Option<Vec<u8>> { self.as_slice().lww() }
    fn fww(&self) -> Option<Vec<u8>> { self.as_slice().fww() }
    fn all(&self) -> Vec<Vec<u8>> { self.as_slice().all() }
    fn lww_head(&self) -> Option<&Head> { self.as_slice().lww_head() }
    fn fww_head(&self) -> Option<&Head> { self.as_slice().fww_head() }
    fn all_heads(&self) -> Vec<&Head> { self.as_slice().all_heads() }
}
