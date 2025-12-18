//! Cryptographic SigChain (append-only signed log)

/// An append-only log where each entry is cryptographically signed
/// and hash-linked to the previous entry.
pub struct SigChain {
    // TODO: entries, hash chain
}

impl SigChain {
    /// Create a new empty sigchain.
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for SigChain {
    fn default() -> Self {
        Self::new()
    }
}
