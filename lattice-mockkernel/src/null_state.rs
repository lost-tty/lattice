//! NullState — a minimal state machine for tests that only exercise the kernel.
//!
//! NullState implements `StateLogic` with a no-op `mutate`. It holds a
//! `StateBackend` (in-memory redb) so that `SystemLayer<PersistentState<NullState>>`
//! works — SystemLayer uses the backend for chain tip tracking and system tables.
//! NullState itself never reads or writes application data to redb.
//!
//! This lets tests for sync, gossip, gap recovery, and store management create
//! intentions (via `SystemBatch`) and verify convergence (via `table_fingerprint`
//! / `intention_count`) without pulling in kvstore or any real store crate.

use lattice_model::{Op, StoreTypeProvider};
use lattice_storage::{StateBackend, StateDbError, StateFactory, StateLogic};
use lattice_store_base::{CommandHandler, Introspectable, StreamHandler, StreamProvider};
use once_cell::sync::Lazy;
use prost_reflect::{DescriptorPool, DynamicMessage, ServiceDescriptor};

/// Store type constant for NullState, analogous to `STORE_TYPE_KVSTORE`.
pub const STORE_TYPE_NULLSTORE: &str = "core:nullstore";

// ---------------------------------------------------------------------------
// Descriptor pool (compiled from proto/null_store.proto)
// ---------------------------------------------------------------------------

const NULL_DESCRIPTOR_BYTES: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/null_descriptor.bin"));

static DESCRIPTOR_POOL: Lazy<DescriptorPool> = Lazy::new(|| {
    DescriptorPool::decode(NULL_DESCRIPTOR_BYTES).expect("Invalid embedded null_store descriptors")
});

static NULL_SERVICE_DESCRIPTOR: Lazy<ServiceDescriptor> = Lazy::new(|| {
    DESCRIPTOR_POOL
        .get_service_by_name("lattice.null.NullStore")
        .expect("NullStore service definition missing from descriptor pool")
});

// ---------------------------------------------------------------------------
// NullState
// ---------------------------------------------------------------------------

/// A state machine that does nothing.
///
/// `mutate` is a no-op — it silently accepts every operation without touching
/// the redb table. Chain tip tracking, snapshot/restore, and `applied_chaintips`
/// are all handled by the `StateBackend` (via `PersistentState<NullState>`).
///
/// Intended for use as `SystemLayer<PersistentState<NullState>>` through the
/// real kernel, where tests create intentions via `SystemBatch` and verify
/// convergence via `table_fingerprint()` or `intention_count()`.
pub struct NullState {
    backend: StateBackend,
}

// ---------------------------------------------------------------------------
// StateLogic — delegates backend, no-ops everything else
// ---------------------------------------------------------------------------

impl StateLogic for NullState {
    type Updates = ();

    fn backend(&self) -> &StateBackend {
        &self.backend
    }

    fn mutate(
        &self,
        _table: &mut redb::Table<&[u8], &[u8]>,
        _op: &Op,
        _dag: &dyn lattice_model::DagQueries,
    ) -> Result<Self::Updates, StateDbError> {
        Ok(())
    }

    fn notify(&self, _updates: Self::Updates) {}
}

// ---------------------------------------------------------------------------
// StateFactory — construct from a StateBackend
// ---------------------------------------------------------------------------

impl StateFactory for NullState {
    fn create(backend: StateBackend) -> Self {
        Self { backend }
    }
}

// ---------------------------------------------------------------------------
// StoreTypeProvider
// ---------------------------------------------------------------------------

impl StoreTypeProvider for NullState {
    fn store_type() -> &'static str {
        STORE_TYPE_NULLSTORE
    }
}

// ---------------------------------------------------------------------------
// Introspectable — minimal, just enough to satisfy the trait bound
// ---------------------------------------------------------------------------

impl Introspectable for NullState {
    fn service_descriptor(&self) -> ServiceDescriptor {
        NULL_SERVICE_DESCRIPTOR.clone()
    }

    fn decode_payload(
        &self,
        payload: &[u8],
    ) -> Result<DynamicMessage, Box<dyn std::error::Error + Send + Sync>> {
        let msg_desc = DESCRIPTOR_POOL
            .get_message_by_name("lattice.null.NullPayload")
            .ok_or("NullPayload not in descriptor pool")?;
        let mut dynamic = DynamicMessage::new(msg_desc);
        // Best-effort decode; if the payload isn't actually NullPayload that's fine —
        // tests don't inspect decoded payloads.
        use prost::Message;
        let _ = dynamic.merge(payload);
        Ok(dynamic)
    }
}

// ---------------------------------------------------------------------------
// CommandHandler — reject everything; tests use SystemBatch, not commands
// ---------------------------------------------------------------------------

impl CommandHandler for NullState {
    fn handle_command<'a>(
        &'a self,
        _writer: &'a dyn lattice_model::StateWriter,
        method_name: &'a str,
        _request: DynamicMessage,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<DynamicMessage, Box<dyn std::error::Error + Send + Sync>>,
                > + Send
                + 'a,
        >,
    > {
        let name = method_name.to_string();
        Box::pin(async move {
            Err(format!("NullState does not handle commands (got '{name}')").into())
        })
    }
}

// ---------------------------------------------------------------------------
// StreamProvider — no streams
// ---------------------------------------------------------------------------

impl StreamProvider for NullState {
    fn stream_handlers(&self) -> Vec<StreamHandler<Self>> {
        vec![]
    }
}
