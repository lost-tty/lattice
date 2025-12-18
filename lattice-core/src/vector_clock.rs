//! Vector clocks for causality tracking

use std::collections::HashMap;

/// A vector clock for tracking "how much" of each node's log has been seen.
///
/// Used during reconciliation to identify missing entries between peers.
pub struct VectorClock {
    clocks: HashMap<[u8; 32], u64>,
}

impl VectorClock {
    /// Create a new empty vector clock.
    pub fn new() -> Self {
        Self {
            clocks: HashMap::new(),
        }
    }

    /// Get the clock value for a node (returns 0 if not present).
    pub fn get(&self, node_id: &[u8; 32]) -> u64 {
        self.clocks.get(node_id).copied().unwrap_or(0)
    }

    /// Set the clock value for a node.
    pub fn set(&mut self, node_id: [u8; 32], value: u64) {
        self.clocks.insert(node_id, value);
    }

    /// Increment the clock for a node.
    pub fn increment(&mut self, node_id: [u8; 32]) {
        let current = self.get(&node_id);
        self.set(node_id, current + 1);
    }
}

impl Default for VectorClock {
    fn default() -> Self {
        Self::new()
    }
}
