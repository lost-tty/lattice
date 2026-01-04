//! Merge strategies for resolving multi-head conflicts.
//!
//! Extension traits that add merge methods to head collections:
//!
//! ```rust,ignore
//! use lattice_core::merge::Merge;
//!
//! // Single key
//! let value = store.get(key).await?.lww();
//!
//! // List of keys
//! let entries = store.list().await?.lww();
//! ```

use crate::Head;

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
    
    /// All heads sorted by HLC (newest first), excluding tombstones.
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
        self.all_heads().into_iter().map(|h| h.value.clone()).collect()
    }
    
    fn lww_head(&self) -> Option<&Head> {
        self.iter()
            .filter(|h| !h.tombstone)
            .max_by(|a, b| a.hlc.cmp(&b.hlc).then_with(|| a.author.cmp(&b.author)))
    }
    
    fn fww_head(&self) -> Option<&Head> {
        self.iter()
            .filter(|h| !h.tombstone)
            .min_by(|a, b| a.hlc.cmp(&b.hlc).then_with(|| a.author.cmp(&b.author)))
    }
    
    fn all_heads(&self) -> Vec<&Head> {
        let mut sorted: Vec<_> = self.iter().filter(|h| !h.tombstone).collect();
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

/// Merge strategies for lists of (key, heads) pairs.
pub trait MergeList {
    /// LWW merge each key, dropping tombstoned keys.
    fn lww(&self) -> Vec<(Vec<u8>, Vec<u8>)>;
    
    /// FWW merge each key, dropping tombstoned keys.
    fn fww(&self) -> Vec<(Vec<u8>, Vec<u8>)>;
}

impl MergeList for [(Vec<u8>, Vec<Head>)] {
    fn lww(&self) -> Vec<(Vec<u8>, Vec<u8>)> {
        self.iter()
            .filter_map(|(k, heads)| heads.lww().map(|v| (k.clone(), v)))
            .collect()
    }
    
    fn fww(&self) -> Vec<(Vec<u8>, Vec<u8>)> {
        self.iter()
            .filter_map(|(k, heads)| heads.fww().map(|v| (k.clone(), v)))
            .collect()
    }
}

impl MergeList for Vec<(Vec<u8>, Vec<Head>)> {
    fn lww(&self) -> Vec<(Vec<u8>, Vec<u8>)> { self.as_slice().lww() }
    fn fww(&self) -> Vec<(Vec<u8>, Vec<u8>)> { self.as_slice().fww() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_model::types::{Hash, PubKey};
    use lattice_model::hlc::HLC;

    fn make_head(hlc: u64, value: &[u8], tombstone: bool) -> Head {
        Head {
            value: value.to_vec(),
            hlc: HLC::new(hlc, 0),
            author: PubKey::default(),
            hash: Hash::default(),
            tombstone,
        }
    }

    #[test]
    fn test_lww_picks_highest_hlc() {
        let heads = vec![
            make_head(100, b"old", false),
            make_head(200, b"new", false),
            make_head(150, b"mid", false),
        ];
        assert_eq!(heads.lww(), Some(b"new".to_vec()));
    }

    #[test]
    fn test_lww_skips_tombstones() {
        let heads = vec![
            make_head(100, b"old", false),
            make_head(200, b"deleted", true),
        ];
        assert_eq!(heads.lww(), Some(b"old".to_vec()));
    }

    #[test]
    fn test_fww_picks_lowest_hlc() {
        let heads = vec![
            make_head(100, b"first", false),
            make_head(200, b"second", false),
        ];
        assert_eq!(heads.fww(), Some(b"first".to_vec()));
    }

    #[test]
    fn test_all_sorted_newest_first() {
        let heads = vec![
            make_head(100, b"old", false),
            make_head(200, b"new", false),
            make_head(150, b"mid", false),
        ];
        assert_eq!(heads.all(), vec![b"new".to_vec(), b"mid".to_vec(), b"old".to_vec()]);
    }

    #[test]
    fn test_list_lww() {
        let list = vec![
            (b"key1".to_vec(), vec![make_head(100, b"v1", false)]),
            (b"key2".to_vec(), vec![make_head(200, b"v2", true)]), // tombstone
            (b"key3".to_vec(), vec![make_head(300, b"v3", false)]),
        ];
        let merged = list.lww();
        assert_eq!(merged.len(), 2); // key2 dropped
        assert_eq!(merged[0], (b"key1".to_vec(), b"v1".to_vec()));
        assert_eq!(merged[1], (b"key3".to_vec(), b"v3".to_vec()));
    }
}
