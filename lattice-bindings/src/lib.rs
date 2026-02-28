uniffi::setup_scaffolding!();

use prost_reflect::prost::Message;
use prost_reflect::{DescriptorPool, DynamicMessage, Kind, ReflectMessage, Value};
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

// Re-export proto types directly (have UniFFI derives via lattice-api ffi feature)
pub use lattice_api::proto::{
    AuthorState, JoinResponse, NodeStatus, PeerInfo, StoreDetails, StoreMeta, StoreRef,
    WitnessLogEntry,
};

// FFI-compatible event type (uses Vec<u8> for IDs which UniFFI supports)
#[derive(Debug, Clone, uniffi::Enum)]
pub enum BackendEvent {
    StoreReady {
        root_id: Vec<u8>,
        store_id: Vec<u8>,
    },
    JoinFailed {
        root_id: Vec<u8>,
        reason: String,
    },
    SyncResult {
        store_id: Vec<u8>,
        peers_synced: u32,
        entries_sent: u64,
        entries_received: u64,
    },
}

impl From<lattice_runtime::NodeEvent> for BackendEvent {
    fn from(e: lattice_runtime::NodeEvent) -> Self {
        // lattice_runtime::NodeEvent is proto node_event::NodeEvent
        match e {
            lattice_runtime::NodeEvent::StoreReady(inner) => BackendEvent::StoreReady {
                root_id: inner.root_id,
                store_id: inner.store_id,
            },
            lattice_runtime::NodeEvent::JoinFailed(inner) => BackendEvent::JoinFailed {
                root_id: inner.root_id,
                reason: inner.reason,
            },
            lattice_runtime::NodeEvent::SyncResult(inner) => BackendEvent::SyncResult {
                store_id: inner.store_id,
                peers_synced: inner.peers_synced,
                entries_sent: inner.entries_sent,
                entries_received: inner.entries_received,
            },
        }
    }
}

#[derive(thiserror::Error, Debug, uniffi::Error)]
pub enum LatticeError {
    #[error("Not initialized - call start() first")]
    NotInitialized,
    #[error("Invalid UUID: {reason}")]
    InvalidUuid { reason: String },
    #[error("Type not found: {name}")]
    TypeNotFound { name: String },
    #[error("Network error: {message}")]
    Network { message: String },
    #[error("Store error: {message}")]
    Store { message: String },
    #[error("Mesh error: {message}")]
    Mesh { message: String },
    #[error("Internal error: {message}")]
    Internal { message: String },
}

impl LatticeError {
    /// Convert any error to a LatticeError, categorizing by message content
    fn from_backend<E: std::fmt::Display>(e: E) -> Self {
        let msg = e.to_string();
        // Categorize based on error message content
        if msg.contains("timeout") || msg.contains("connection") || msg.contains("network") {
            LatticeError::Network { message: msg }
        } else if msg.contains("store") {
            LatticeError::Store { message: msg }
        } else if msg.contains("mesh") {
            LatticeError::Mesh { message: msg }
        } else {
            LatticeError::Internal { message: msg }
        }
    }
}

// Reflection Types

/// Input values for dynamic method execution
#[derive(uniffi::Enum, Clone, Debug)]
pub enum ArgValue {
    Null,
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Bytes(Vec<u8>),
    List(Vec<ArgValue>),
}

// ... (ReflectValue etc unchanged)

// Update map_arg_to_value to return Option<Value>
fn map_arg_to_value(arg: &ArgValue, kind: Kind) -> Result<Option<Value>, LatticeError> {
    match (arg, kind.clone()) {
        (ArgValue::Null, _) => Ok(None),
        (ArgValue::String(s), Kind::String) => Ok(Some(Value::String(s.clone()))),
        (ArgValue::Int(i), Kind::Int32 | Kind::Sint32 | Kind::Sfixed32) => {
            Ok(Some(Value::I32(*i as i32)))
        }
        (ArgValue::Int(i), Kind::Int64 | Kind::Sint64 | Kind::Sfixed64) => Ok(Some(Value::I64(*i))),
        (ArgValue::Int(i), Kind::Uint32 | Kind::Fixed32) => Ok(Some(Value::U32(*i as u32))),
        (ArgValue::Int(i), Kind::Uint64 | Kind::Fixed64) => Ok(Some(Value::U64(*i as u64))),
        (ArgValue::Float(f), Kind::Float) => Ok(Some(Value::F32(*f as f32))),
        (ArgValue::Float(f), Kind::Double) => Ok(Some(Value::F64(*f))),
        (ArgValue::Bool(b), Kind::Bool) => Ok(Some(Value::Bool(*b))),
        (ArgValue::Bytes(b), Kind::Bytes) => {
            Ok(Some(Value::Bytes(bytes::Bytes::copy_from_slice(b))))
        }
        // Coercions
        (ArgValue::String(s), Kind::Int32) => {
            if s.is_empty() {
                return Ok(None);
            }
            s.parse::<i32>()
                .map(Value::I32)
                .map(Some)
                .map_err(|e| LatticeError::Internal {
                    message: e.to_string(),
                })
        }
        (ArgValue::String(s), Kind::Int64) => {
            if s.is_empty() {
                return Ok(None);
            }
            s.parse::<i64>()
                .map(Value::I64)
                .map(Some)
                .map_err(|e| LatticeError::Internal {
                    message: e.to_string(),
                })
        }
        // Fallback for empty strings on other numeric types to be safe
        (ArgValue::String(s), _) if s.is_empty() => Ok(None),

        _ => Err(LatticeError::Internal {
            message: format!("Type mismatch for arg {:?} vs kind {:?}", arg, kind),
        }),
    }
}

/// Structured result values from dynamic method execution
#[derive(uniffi::Enum, Clone, Debug)]
pub enum ReflectValue {
    Null,
    String(String),
    Int(i64),
    Uint(u64),
    Float(f64),
    Bool(bool),
    Bytes(Vec<u8>),
    List(Vec<ReflectValue>),
    Message(Vec<ReflectField>),
    Enum { name: String, value: i32 },
}

/// A single field within a ReflectValue::Message
#[derive(uniffi::Record, Clone, Debug)]
pub struct ReflectField {
    pub name: String,
    pub value: ReflectValue,
}

/// Field type descriptor with type name for nested types
#[derive(uniffi::Enum, Clone, Debug)]
pub enum FieldType {
    String,
    Int,
    Uint,
    Float,
    Bool,
    Bytes,
    Message { type_name: String },
    Enum { type_name: String },
}

#[derive(uniffi::Record)]
pub struct FieldInfo {
    pub name: String,
    pub field_type: FieldType,
    pub is_repeated: bool,
}

#[derive(uniffi::Record)]
pub struct MethodInfo {
    pub name: String,
    pub input_fields: Vec<FieldInfo>,
    pub return_type: FieldType,
}

#[derive(uniffi::Object)]
pub struct Lattice {
    // Dedicated runtime for async operations
    rt: tokio::runtime::Runtime,
    // Shared state
    runtime: RwLock<Option<lattice_runtime::Runtime>>,
}

#[uniffi::export]
impl Lattice {
    #[uniffi::constructor]
    pub fn new(_data_dir: Option<String>) -> Arc<Self> {
        // Initialize logging
        let _ = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                    tracing_subscriber::EnvFilter::new("info,iroh=warn,iroh_net=warn")
                }),
            )
            .try_init();

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("Failed to create tokio runtime");

        Arc::new(Self {
            rt,
            runtime: RwLock::new(None),
        })
    }

    // Synchronous blocking start
    pub fn start(
        &self,
        data_dir: Option<String>,
        name: Option<String>,
    ) -> Result<(), LatticeError> {
        let mut w = self.rt.block_on(self.runtime.write());
        if w.is_some() {
            return Ok(());
        }

        let mut builder = lattice_runtime::Runtime::builder().with_core_stores();
        if let Some(path) = data_dir {
            builder = builder.data_dir(std::path::PathBuf::from(path));
        }
        if let Some(n) = name {
            builder = builder.with_name(n);
        }

        let rt = self
            .rt
            .block_on(builder.build())
            .map_err(|e| LatticeError::from_backend(e))?;
        *w = Some(rt);
        Ok(())
    }

    pub fn is_started(&self) -> bool {
        self.rt.block_on(self.runtime.read()).is_some()
    }

    pub fn node_id(&self) -> Result<String, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        Ok(hex::encode(r.backend().node_id()))
    }

    pub fn node_status(&self) -> Result<NodeStatus, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        self.rt
            .block_on(r.backend().node_status())
            .map_err(|e| LatticeError::from_backend(e))
    }

    pub fn set_name(&self, name: String) -> Result<(), LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        self.rt
            .block_on(r.backend().node_set_name(&name))
            .map_err(|e| LatticeError::from_backend(e))
    }

    // Store
    pub fn store_create(
        &self,
        parent_id: Option<String>,
        name: Option<String>,
        store_type: String,
    ) -> Result<StoreRef, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;

        let pid = match parent_id {
            Some(s) => Some(Uuid::parse_str(&s).map_err(|e| LatticeError::InvalidUuid {
                reason: e.to_string(),
            })?),
            None => None,
        };

        self.rt
            .block_on(r.backend().store_create(pid, name, &store_type))
            .map_err(|e| LatticeError::from_backend(e))
    }

    pub fn store_list(&self, parent_id: Option<String>) -> Result<Vec<StoreRef>, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;

        let pid = match parent_id {
            Some(s) => Some(Uuid::parse_str(&s).map_err(|e| LatticeError::InvalidUuid {
                reason: e.to_string(),
            })?),
            None => None,
        };

        self.rt
            .block_on(r.backend().store_list(pid))
            .map_err(|e| LatticeError::from_backend(e))
    }

    pub fn store_peer_invite(&self, store_id: String) -> Result<String, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        let id = Uuid::parse_str(&store_id).map_err(|e| LatticeError::InvalidUuid {
            reason: e.to_string(),
        })?;
        self.rt
            .block_on(r.backend().store_peer_invite(id))
            .map_err(|e| LatticeError::from_backend(e))
    }

    pub fn store_join(&self, token: String) -> Result<JoinResponse, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        self.rt
            .block_on(r.backend().store_join(&token))
            .map(|id| JoinResponse {
                store_id: id.as_bytes().to_vec(),
            })
            .map_err(|e| LatticeError::from_backend(e))
    }

    pub fn store_status(&self, store_id: String) -> Result<StoreMeta, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        let id = Uuid::parse_str(&store_id).map_err(|e| LatticeError::InvalidUuid {
            reason: e.to_string(),
        })?;
        self.rt
            .block_on(r.backend().store_status(id))
            .map_err(|e| LatticeError::from_backend(e))
    }

    pub fn store_delete(
        &self,
        parent_store_id: String,
        child_store_id: String,
    ) -> Result<(), LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        let parent_id =
            Uuid::parse_str(&parent_store_id).map_err(|e| LatticeError::InvalidUuid {
                reason: e.to_string(),
            })?;
        let child_id = Uuid::parse_str(&child_store_id).map_err(|e| LatticeError::InvalidUuid {
            reason: e.to_string(),
        })?;
        self.rt
            .block_on(r.backend().store_delete(parent_id, child_id))
            .map_err(|e| LatticeError::from_backend(e))
    }

    pub fn store_sync(&self, store_id: String) -> Result<(), LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        let id = Uuid::parse_str(&store_id).map_err(|e| LatticeError::InvalidUuid {
            reason: e.to_string(),
        })?;
        self.rt
            .block_on(r.backend().store_sync(id))
            .map(|_| ())
            .map_err(|e| LatticeError::from_backend(e))
    }

    pub fn store_witness_log(
        &self,
        store_id: String,
    ) -> Result<Vec<WitnessLogEntry>, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        let id = Uuid::parse_str(&store_id).map_err(|e| LatticeError::InvalidUuid {
            reason: e.to_string(),
        })?;
        self.rt
            .block_on(r.backend().store_witness_log(id))
            .map(|entries| entries.into_iter().map(Into::into).collect())
            .map_err(|e| LatticeError::from_backend(e))
    }

    pub fn store_peers(&self, store_id: String) -> Result<Vec<PeerInfo>, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        let id = Uuid::parse_str(&store_id).map_err(|e| LatticeError::InvalidUuid {
            reason: e.to_string(),
        })?;
        self.rt
            .block_on(r.backend().store_peers(id))
            .map_err(|e| LatticeError::from_backend(e))
    }

    pub fn store_peer_revoke(
        &self,
        store_id: String,
        peer_key: Vec<u8>,
    ) -> Result<(), LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        let id = Uuid::parse_str(&store_id).map_err(|e| LatticeError::InvalidUuid {
            reason: e.to_string(),
        })?;
        self.rt
            .block_on(r.backend().store_peer_revoke(id, &peer_key))
            .map_err(|e| LatticeError::from_backend(e))
    }

    // Dynamic Reflection

    pub fn store_inspect(&self, store_id: String) -> Result<Vec<MethodInfo>, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        let id = Uuid::parse_str(&store_id).map_err(|e| LatticeError::InvalidUuid {
            reason: e.to_string(),
        })?;

        let (descriptor_bytes, service_name) = self
            .rt
            .block_on(r.backend().store_get_descriptor(id))
            .map_err(|e| LatticeError::from_backend(e))?;

        let pool = DescriptorPool::decode(descriptor_bytes.as_slice()).map_err(|e| {
            LatticeError::Internal {
                message: format!("Failed to decode descriptors: {}", e),
            }
        })?;

        let service =
            pool.get_service_by_name(&service_name)
                .ok_or_else(|| LatticeError::Internal {
                    message: format!("Service '{}' not found", service_name),
                })?;

        let mut methods = Vec::new();
        for method in service.methods() {
            let input_desc = method.input();
            let mut fields = Vec::new();

            for field in input_desc.fields() {
                fields.push(FieldInfo {
                    name: field.name().to_string(),
                    field_type: map_kind_to_field_type(field.kind()),
                    is_repeated: field.is_list(),
                });
            }

            // Determine return type from output message
            let output_desc = method.output();
            let return_type = FieldType::Message {
                type_name: output_desc.full_name().to_string(),
            };

            methods.push(MethodInfo {
                name: method.name().to_string(),
                input_fields: fields,
                return_type,
            });
        }

        Ok(methods)
    }

    pub fn store_exec_dynamic(
        &self,
        store_id: String,
        method_name: String,
        args: Vec<ArgValue>,
    ) -> Result<ReflectValue, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        let id = Uuid::parse_str(&store_id).map_err(|e| LatticeError::InvalidUuid {
            reason: e.to_string(),
        })?;

        let (descriptor_bytes, service_name) = self
            .rt
            .block_on(r.backend().store_get_descriptor(id))
            .map_err(|e| LatticeError::from_backend(e))?;

        let pool = DescriptorPool::decode(descriptor_bytes.as_slice())
            .map_err(|e| LatticeError::from_backend(e))?;

        let service =
            pool.get_service_by_name(&service_name)
                .ok_or_else(|| LatticeError::Internal {
                    message: format!("Service '{}' not found", service_name),
                })?;

        let method = service
            .methods()
            .find(|m| m.name().eq_ignore_ascii_case(&method_name))
            .ok_or_else(|| LatticeError::Internal {
                message: format!("Method '{}' not found", method_name),
            })?;

        let input_desc = method.input();
        let mut dynamic_msg = DynamicMessage::new(input_desc.clone());

        // Map positional args to fields
        let fields: Vec<_> = input_desc.fields().collect();
        if args.len() > fields.len() {
            return Err(LatticeError::Internal {
                message: format!(
                    "Too many arguments. Expected {}, got {}",
                    fields.len(),
                    args.len()
                ),
            });
        }

        for (i, arg) in args.iter().enumerate() {
            let field = &fields[i];
            if let Some(value) = map_arg_to_value(arg, field.kind())? {
                dynamic_msg.set_field(field, value);
            }
        }

        let mut payload = Vec::new();
        dynamic_msg
            .encode(&mut payload)
            .map_err(|e| LatticeError::from_backend(e))?;

        // Execute
        let result_bytes = self
            .rt
            .block_on(r.backend().store_exec(id, method.name(), &payload))
            .map_err(|e| LatticeError::from_backend(e))?;

        // Decode result into ReflectValue
        let output_desc = method.output();
        let result_msg = DynamicMessage::decode(output_desc.clone(), result_bytes.as_slice())
            .map_err(|e| LatticeError::Internal {
                message: format!("Failed to decode result: {}", e),
            })?;

        Ok(dynamic_message_to_reflect_value(&result_msg))
    }

    /// Inspect a specific type definition (message or enum) within a store's schema
    pub fn store_inspect_type(
        &self,
        store_id: String,
        type_name: String,
    ) -> Result<Vec<FieldInfo>, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        let id = Uuid::parse_str(&store_id).map_err(|e| LatticeError::InvalidUuid {
            reason: e.to_string(),
        })?;

        let (descriptor_bytes, _) = self
            .rt
            .block_on(r.backend().store_get_descriptor(id))
            .map_err(|e| LatticeError::from_backend(e))?;

        let pool = DescriptorPool::decode(descriptor_bytes.as_slice()).map_err(|e| {
            LatticeError::Internal {
                message: format!("Failed to decode descriptors: {}", e),
            }
        })?;

        let message_desc =
            pool.get_message_by_name(&type_name)
                .ok_or_else(|| LatticeError::TypeNotFound {
                    name: type_name.clone(),
                })?;

        let mut fields = Vec::new();
        for field in message_desc.fields() {
            fields.push(FieldInfo {
                name: field.name().to_string(),
                field_type: map_kind_to_field_type(field.kind()),
                is_repeated: field.is_list(),
            });
        }

        Ok(fields)
    }

    pub fn get_store_descriptor(&self, store_id: String) -> Result<DescriptorInfo, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        let id = Uuid::parse_str(&store_id).map_err(|e| LatticeError::InvalidUuid {
            reason: e.to_string(),
        })?;

        let (bytes, name) = self
            .rt
            .block_on(r.backend().store_get_descriptor(id))
            .map_err(|e| LatticeError::from_backend(e))?;

        Ok(DescriptorInfo {
            descriptor_bytes: bytes,
            service_name: name,
        })
    }

    // ---- Stream operations ----

    /// List available streams for a store
    pub fn store_list_streams(&self, store_id: String) -> Result<Vec<StreamInfo>, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        let id = Uuid::parse_str(&store_id).map_err(|e| LatticeError::InvalidUuid {
            reason: e.to_string(),
        })?;

        self.rt
            .block_on(r.backend().store_list_streams(id))
            .map(|streams| streams.into_iter().map(Into::into).collect())
            .map_err(|e| LatticeError::from_backend(e))
    }

    /// Subscribe to a store stream (e.g., "Watch" with pattern param)
    pub fn store_subscribe(
        &self,
        store_id: String,
        stream_name: String,
        args: Vec<ArgValue>,
    ) -> Result<Arc<StoreStream>, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        let id = Uuid::parse_str(&store_id).map_err(|e| LatticeError::InvalidUuid {
            reason: e.to_string(),
        })?;

        // Get descriptor pool for decoding
        let (descriptor_bytes, _) = self
            .rt
            .block_on(r.backend().store_get_descriptor(id))
            .map_err(|e| LatticeError::from_backend(e))?;
        let pool = DescriptorPool::decode(descriptor_bytes.as_slice()).ok();

        // Get stream info for schema and param building
        let streams = self
            .rt
            .block_on(r.backend().store_list_streams(id))
            .map_err(|e| LatticeError::from_backend(e))?;
        let stream_desc = streams
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(&stream_name))
            .ok_or_else(|| LatticeError::Store {
                message: format!("Stream '{}' not found", stream_name),
            })?;

        // Build params
        let params = self.build_stream_params(stream_desc, &args, pool.as_ref())?;

        // Subscribe
        let stream = self
            .rt
            .block_on(r.backend().store_subscribe(id, &stream_desc.name, &params))
            .map_err(|e| LatticeError::from_backend(e))?;

        // Bridge stream to channel for blocking iteration
        let (tx, rx) = tokio::sync::mpsc::channel(128);
        let rt_handle = self.rt.handle().clone();
        rt_handle.spawn(async move {
            use tokio_stream::StreamExt;
            tokio::pin!(stream);
            while let Some(payload) = stream.next().await {
                if tx.send(payload).await.is_err() {
                    break;
                }
            }
        });

        Ok(Arc::new(StoreStream {
            rx: Arc::new(tokio::sync::Mutex::new(rx)),
            pool,
            event_schema: stream_desc.event_schema.clone(),
        }))
    }

    // Events
    // Note: NOT async — must run on self.rt so tokio::spawn in the backend
    // finds the correct reactor. UniFFI's async executor is separate.
    pub fn subscribe(&self) -> Result<Arc<LatticeEventStream>, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;

        let _guard = self.rt.enter();
        let rx = r
            .backend()
            .subscribe()
            .map_err(|e| LatticeError::from_backend(e))?;

        Ok(Arc::new(LatticeEventStream {
            rx: Arc::new(tokio::sync::Mutex::new(rx)),
        }))
    }
}

impl Lattice {
    fn build_stream_params(
        &self,
        desc: &lattice_store_base::StreamDescriptor,
        args: &[ArgValue],
        pool: Option<&DescriptorPool>,
    ) -> Result<Vec<u8>, LatticeError> {
        let Some(schema) = desc.param_schema.as_ref().filter(|s| !s.is_empty()) else {
            return Ok(vec![]);
        };
        let Some(pool) = pool else { return Ok(vec![]) };
        let msg_desc =
            pool.get_message_by_name(schema)
                .ok_or_else(|| LatticeError::TypeNotFound {
                    name: schema.to_string(),
                })?;

        let mut msg = DynamicMessage::new(msg_desc.clone());
        let fields: Vec<_> = msg_desc.fields().collect();

        for (i, arg) in args.iter().enumerate() {
            if i >= fields.len() {
                break;
            }
            let field = &fields[i];
            if let Some(value) = map_arg_to_value(arg, field.kind())? {
                msg.set_field(field, value);
            }
        }

        let mut buf = Vec::new();
        msg.encode(&mut buf).map_err(|e| LatticeError::Internal {
            message: e.to_string(),
        })?;
        Ok(buf)
    }
}

#[derive(uniffi::Record)]
pub struct DescriptorInfo {
    pub descriptor_bytes: Vec<u8>,
    pub service_name: String,
}

/// Stream metadata for FFI
#[derive(uniffi::Record)]
pub struct StreamInfo {
    pub name: String,
    pub param_schema: Option<String>,
    pub event_schema: Option<String>,
}

impl From<lattice_store_base::StreamDescriptor> for StreamInfo {
    fn from(s: lattice_store_base::StreamDescriptor) -> Self {
        Self {
            name: s.name,
            param_schema: s.param_schema,
            event_schema: s.event_schema,
        }
    }
}

/// Store stream subscription for FFI - provides blocking iteration
#[derive(uniffi::Object)]
pub struct StoreStream {
    rx: Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<Vec<u8>>>>,
    pool: Option<DescriptorPool>,
    event_schema: Option<String>,
}

#[uniffi::export]
impl StoreStream {
    /// Async next() - returns None when stream ends, Error on decode failure
    pub async fn next(&self) -> Result<Option<ReflectValue>, LatticeError> {
        let mut rx = self.rx.lock().await;
        match rx.recv().await {
            Some(payload) => self.decode_event(&payload).map(Some),
            None => Ok(None),
        }
    }

    /// Get raw bytes (for advanced use cases)
    pub async fn next_bytes(&self) -> Option<Vec<u8>> {
        let mut rx = self.rx.lock().await;
        rx.recv().await
    }
}

impl StoreStream {
    fn decode_event(&self, payload: &[u8]) -> Result<ReflectValue, LatticeError> {
        let schema = self
            .event_schema
            .as_ref()
            .ok_or_else(|| LatticeError::Internal {
                message: "No event schema defined".into(),
            })?;
        let pool = self.pool.as_ref().ok_or_else(|| LatticeError::Internal {
            message: "No descriptor pool".into(),
        })?;
        let msg_desc =
            pool.get_message_by_name(schema)
                .ok_or_else(|| LatticeError::TypeNotFound {
                    name: schema.clone(),
                })?;
        let msg =
            DynamicMessage::decode(msg_desc, payload).map_err(|e| LatticeError::Internal {
                message: format!("Decode error: {}", e),
            })?;
        Ok(dynamic_message_to_reflect_value(&msg))
    }
}

#[derive(uniffi::Object)]
pub struct LatticeEventStream {
    rx: Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<lattice_runtime::NodeEvent>>>,
}

#[uniffi::export]
impl LatticeEventStream {
    // Async next()
    pub async fn next(&self) -> Option<BackendEvent> {
        let mut rx = self.rx.lock().await;
        rx.recv().await.map(Into::into)
    }
}

// Helpers for Reflection

fn map_kind_to_field_type(kind: Kind) -> FieldType {
    match kind {
        Kind::Double | Kind::Float => FieldType::Float,
        Kind::Int32
        | Kind::Int64
        | Kind::Sint32
        | Kind::Sint64
        | Kind::Sfixed32
        | Kind::Sfixed64 => FieldType::Int,
        Kind::Uint32 | Kind::Uint64 | Kind::Fixed32 | Kind::Fixed64 => FieldType::Uint,
        Kind::Bool => FieldType::Bool,
        Kind::String => FieldType::String,
        Kind::Bytes => FieldType::Bytes,
        Kind::Message(msg_desc) => FieldType::Message {
            type_name: msg_desc.full_name().to_string(),
        },
        Kind::Enum(enum_desc) => FieldType::Enum {
            type_name: enum_desc.full_name().to_string(),
        },
    }
}

/// Convert a DynamicMessage to a ReflectValue::Message
fn dynamic_message_to_reflect_value(msg: &DynamicMessage) -> ReflectValue {
    dynamic_message_to_reflect_value_depth(msg, 0)
}

fn dynamic_message_to_reflect_value_depth(msg: &DynamicMessage, depth: usize) -> ReflectValue {
    if depth > 16 {
        return ReflectValue::String("<max depth>".to_string());
    }
    let mut fields = Vec::new();
    for field_desc in msg.descriptor().fields() {
        let value = msg.get_field(&field_desc);
        fields.push(ReflectField {
            name: field_desc.name().to_string(),
            value: value_to_reflect_value_with_field(&value, Some(&field_desc), depth + 1),
        });
    }
    ReflectValue::Message(fields)
}

fn value_to_reflect_value_with_field(
    value: &Value,
    field: Option<&prost_reflect::FieldDescriptor>,
    depth: usize,
) -> ReflectValue {
    if depth > 16 {
        return ReflectValue::String("<max depth>".to_string());
    }
    match value {
        Value::Bool(b) => ReflectValue::Bool(*b),
        Value::I32(i) => ReflectValue::Int(*i as i64),
        Value::I64(i) => ReflectValue::Int(*i),
        Value::U32(u) => ReflectValue::Uint(*u as u64),
        Value::U64(u) => ReflectValue::Uint(*u),
        Value::F32(f) => ReflectValue::Float(*f as f64),
        Value::F64(f) => ReflectValue::Float(*f),
        Value::String(s) => ReflectValue::String(s.clone()),
        Value::Bytes(b) => ReflectValue::Bytes(b.to_vec()),
        Value::EnumNumber(n) => {
            let name = field
                .and_then(|f| match f.kind() {
                    prost_reflect::Kind::Enum(e) => e.get_value(*n).map(|v| v.name().to_string()),
                    _ => None,
                })
                .unwrap_or_default();
            ReflectValue::Enum { name, value: *n }
        }
        Value::Message(msg) => dynamic_message_to_reflect_value_depth(msg, depth + 1),
        Value::List(list) => {
            let items: Vec<ReflectValue> = list
                .iter()
                .map(|v| value_to_reflect_value_with_field(v, field, depth + 1))
                .collect();
            ReflectValue::List(items)
        }
        Value::Map(map) => {
            let items: Vec<ReflectValue> = map
                .iter()
                .map(|(k, v)| {
                    ReflectValue::Message(vec![
                        ReflectField {
                            name: "key".to_string(),
                            value: map_key_to_reflect_value(k),
                        },
                        ReflectField {
                            name: "value".to_string(),
                            value: value_to_reflect_value_with_field(v, None, depth + 1),
                        },
                    ])
                })
                .collect();
            ReflectValue::List(items)
        }
    }
}

/// Convert a map key to ReflectValue
fn map_key_to_reflect_value(key: &prost_reflect::MapKey) -> ReflectValue {
    match key {
        prost_reflect::MapKey::Bool(b) => ReflectValue::Bool(*b),
        prost_reflect::MapKey::I32(i) => ReflectValue::Int(*i as i64),
        prost_reflect::MapKey::I64(i) => ReflectValue::Int(*i),
        prost_reflect::MapKey::U32(u) => ReflectValue::Uint(*u as u64),
        prost_reflect::MapKey::U64(u) => ReflectValue::Uint(*u),
        prost_reflect::MapKey::String(s) => ReflectValue::String(s.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_stream(pool: Option<DescriptorPool>, schema: Option<String>) -> StoreStream {
        StoreStream {
            rx: Arc::new(tokio::sync::Mutex::new(tokio::sync::mpsc::channel(1).1)),
            pool,
            event_schema: schema,
        }
    }

    #[tokio::test]
    async fn decode_event_missing_schema_returns_error() {
        let stream = make_test_stream(None, None);
        let result = stream.decode_event(&[1, 2, 3]);
        assert!(matches!(result, Err(LatticeError::Internal { .. })));
    }

    #[tokio::test]
    async fn decode_event_missing_pool_returns_error() {
        let stream = make_test_stream(None, Some("test.Message".to_string()));
        let result = stream.decode_event(&[1, 2, 3]);
        assert!(matches!(result, Err(LatticeError::Internal { .. })));
    }

    #[tokio::test]
    async fn decode_event_type_not_found_returns_error() {
        let pool = DescriptorPool::new();
        let stream = make_test_stream(Some(pool), Some("nonexistent.Message".to_string()));
        let result = stream.decode_event(&[1, 2, 3]);
        assert!(matches!(result, Err(LatticeError::TypeNotFound { .. })));
    }
}
