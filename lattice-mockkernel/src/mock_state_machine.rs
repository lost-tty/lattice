//! Kernel-level mock state machines for tests that use `OpenedStore` directly.
//!
//! `NullStateMachine` — no-op apply, for tests that only exercise the kernel
//! (witness log, sync, streaming) without caring about state.
//!
//! `TrackingStateMachine` — records every applied op hash, for tests that
//! need to assert which ops were projected onto state.

use lattice_model::types::Hash;
use lattice_model::{Op, StateMachine, StoreIdentity, StoreMeta};
use std::sync::{Arc, RwLock};

// ---------------------------------------------------------------------------
// NullStateMachine — no-op
// ---------------------------------------------------------------------------

/// Kernel-level no-op state machine. `apply` always succeeds and does nothing.
#[derive(Clone, Default)]
pub struct NullStateMachine;

impl StateMachine for NullStateMachine {
    type Error = std::io::Error;

    fn store_type() -> &'static str {
        "test:null"
    }

    fn apply(&self, _op: &Op, _dag: &dyn lattice_model::DagQueries) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl StoreIdentity for NullStateMachine {
    fn store_meta(&self) -> StoreMeta {
        StoreMeta::default()
    }
}

// ---------------------------------------------------------------------------
// TrackingStateMachine — records applied op hashes
// ---------------------------------------------------------------------------

/// Kernel-level state machine that records every applied op hash.
#[derive(Clone)]
pub struct TrackingStateMachine {
    applied: Arc<RwLock<Vec<Hash>>>,
}

impl TrackingStateMachine {
    pub fn new() -> Self {
        Self {
            applied: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Check whether a specific op was applied.
    pub fn has_applied(&self, hash: Hash) -> bool {
        self.applied.read().unwrap().contains(&hash)
    }

    /// Return a snapshot of all applied op hashes (in order).
    pub fn applied_ops(&self) -> Vec<Hash> {
        self.applied.read().unwrap().clone()
    }
}

impl Default for TrackingStateMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl StateMachine for TrackingStateMachine {
    type Error = std::io::Error;

    fn store_type() -> &'static str {
        "test:tracking"
    }

    fn apply(&self, op: &Op, _dag: &dyn lattice_model::DagQueries) -> Result<(), Self::Error> {
        self.applied.write().unwrap().push(op.id());
        Ok(())
    }
}

impl StoreIdentity for TrackingStateMachine {
    fn store_meta(&self) -> StoreMeta {
        StoreMeta::default()
    }
}
