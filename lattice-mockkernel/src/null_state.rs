//! NullState — a minimal state machine for tests that only exercise the kernel.
//!
//! NullState implements `StateLogic` with a no-op `apply`. It holds no state —
//! `SystemLayer<NullState>` owns the backend for chain tip tracking and system tables.
//!
//! This lets tests for sync, gossip, gap recovery, and store management create
//! intentions (via `SystemBatch`) and verify convergence (via `table_fingerprint`
//! / `intention_count`) without pulling in kvstore or any real store crate.

use lattice_storage::{StateContext, StateDbError, StateLogic};
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
/// `apply` is a no-op that accepts every operation without touching
/// the redb table. Chain tip tracking is handled by
/// `SystemLayer<NullState>` which owns the backend.
pub struct NullState;

impl From<StateContext<()>> for NullState {
    fn from(_ctx: StateContext<()>) -> Self {
        Self
    }
}

// ---------------------------------------------------------------------------
// StateLogic — no-op apply
// ---------------------------------------------------------------------------

impl StateLogic for NullState {
    type Event = ();

    fn store_type() -> &'static str {
        STORE_TYPE_NULLSTORE
    }

    fn apply(
        _table: &mut redb::Table<&[u8], &[u8]>,
        _op: &lattice_model::Op,
        _dag: &dyn lattice_model::DagQueries,
    ) -> Result<Vec<Self::Event>, StateDbError> {
        Ok(vec![])
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
            .get_message_by_name("lattice.null.WriteRequest")
            .ok_or("WriteRequest not in descriptor pool")?;
        let mut dynamic = DynamicMessage::new(msg_desc);
        // Best-effort decode; if the payload isn't actually WriteRequest that's fine —
        // tests don't inspect decoded payloads.
        use prost::Message;
        let _ = dynamic.merge(payload);
        Ok(dynamic)
    }

    fn method_meta(&self) -> std::collections::HashMap<String, lattice_store_base::MethodMeta> {
        let mut map = std::collections::HashMap::new();
        map.insert(
            "Write".to_string(),
            lattice_store_base::MethodMeta {
                description: "Submit an intention with arbitrary payload".to_string(),
                kind: lattice_store_base::MethodKind::Command,
            },
        );
        map
    }
}

// ---------------------------------------------------------------------------
// CommandHandler — Write submits via StateWriter; queries rejected
// ---------------------------------------------------------------------------

impl CommandHandler for NullState {
    fn handle_command<'a>(
        &'a self,
        writer: &'a dyn lattice_model::StateWriter,
        method_name: &'a str,
        request: DynamicMessage,
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
            match name.as_str() {
                "Write" => {
                    let data: Vec<u8> = request
                        .get_field_by_name("data")
                        .and_then(|v| match v.into_owned() {
                            prost_reflect::Value::Bytes(b) => Some(b.to_vec()),
                            _ => None,
                        })
                        .unwrap_or_default();
                    let payload = crate::wrap_app_data(&data);
                    let hash = writer
                        .submit(payload, vec![])
                        .await
                        .map_err(|e| format!("submit failed: {e:?}"))?;
                    let resp_desc = DESCRIPTOR_POOL
                        .get_message_by_name("lattice.null.WriteResponse")
                        .ok_or("WriteResponse not in descriptor pool")?;
                    let mut resp = DynamicMessage::new(resp_desc);
                    resp.set_field_by_name("hash", prost_reflect::Value::Bytes(hash.as_bytes().to_vec().into()));
                    Ok(resp)
                }
                _ => Err(format!("NullState does not handle command '{name}'").into()),
            }
        })
    }

    fn handle_query<'a>(
        &'a self,
        _dag: &'a dyn lattice_model::DagQueries,
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
            Err(format!("NullState does not handle queries (got '{name}')").into())
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
