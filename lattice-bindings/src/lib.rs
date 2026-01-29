uniffi::setup_scaffolding!();

use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;
use prost_reflect::{DescriptorPool, DynamicMessage, Value, Kind};
use prost_reflect::prost::Message;

// Re-export proto types directly (have UniFFI derives via lattice-api ffi feature)
pub use lattice_api::proto::{
    NodeStatus, MeshInfo, StoreInfo, StoreDetails, PeerInfo, 
    HistoryEntry, AuthorState, CleanupResult, JoinResponse,
};

// FFI-compatible event type (uses Vec<u8> for IDs which UniFFI supports)
#[derive(Debug, Clone, uniffi::Enum)]
pub enum BackendEvent {
    MeshReady { mesh_id: Vec<u8> },
    StoreReady { mesh_id: Vec<u8>, store_id: Vec<u8> },
    JoinFailed { mesh_id: Vec<u8>, reason: String },
    SyncResult { store_id: Vec<u8>, peers_synced: u32, entries_sent: u64, entries_received: u64 },
}

impl From<lattice_runtime::NodeEvent> for BackendEvent {
    fn from(e: lattice_runtime::NodeEvent) -> Self {
        // lattice_runtime::NodeEvent is proto node_event::NodeEvent
        match e {
            lattice_runtime::NodeEvent::MeshReady(inner) => 
                BackendEvent::MeshReady { mesh_id: inner.mesh_id },
            lattice_runtime::NodeEvent::StoreReady(inner) => 
                BackendEvent::StoreReady { mesh_id: inner.mesh_id, store_id: inner.store_id },
            lattice_runtime::NodeEvent::JoinFailed(inner) => 
                BackendEvent::JoinFailed { mesh_id: inner.mesh_id, reason: inner.reason },
            lattice_runtime::NodeEvent::SyncResult(inner) => 
                BackendEvent::SyncResult { 
                    store_id: inner.store_id, 
                    peers_synced: inner.peers_synced, 
                    entries_sent: inner.entries_sent, 
                    entries_received: inner.entries_received 
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

#[derive(uniffi::Enum, Clone, Debug)]
pub enum ArgValue {
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Bytes(Vec<u8>),
    List(Vec<ArgValue>),
    // Simplified map support? Or omit for now.
    // Map(Vec<ArgValue>, Vec<ArgValue>) 
}

#[derive(uniffi::Enum)]
pub enum FieldType {
    String,
    Int,
    Float,
    Bool,
    Bytes,
    List, // logic will need to check element type
    Message, // Nested message?
    Enum,
}

#[derive(uniffi::Record)]
pub struct FieldInfo {
    pub name: String,
    pub type_kind: FieldType,
    pub type_name: String, // e.g. "int32" or "MyEnum"
    pub is_repeated: bool,
}

#[derive(uniffi::Record)]
pub struct MethodInfo {
    pub name: String,
    pub input_fields: Vec<FieldInfo>,
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
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,iroh=warn,iroh_net=warn"))
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
    pub fn start(&self, data_dir: Option<String>, name: Option<String>) -> Result<(), LatticeError> {
        let mut w = self.rt.block_on(self.runtime.write());
        if w.is_some() {
            return Ok(());
        }
        
        let mut builder = lattice_runtime::Runtime::builder();
        if let Some(path) = data_dir {
            builder = builder.data_dir(std::path::PathBuf::from(path));
        }
        if let Some(n) = name {
            builder = builder.with_name(n);
        }
        
        let rt = self.rt.block_on(builder.build()).map_err(|e| LatticeError::from_backend(e))?;
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
        self.rt.block_on(r.backend().node_status())
            .map_err(|e| LatticeError::from_backend(e))
    }

    pub fn set_name(&self, name: String) -> Result<(), LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        self.rt.block_on(r.backend().node_set_name(&name)).map_err(|e| LatticeError::from_backend(e))
    }

    // Mesh
    pub fn mesh_list(&self) -> Result<Vec<MeshInfo>, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        self.rt.block_on(r.backend().mesh_list())
            .map_err(|e| LatticeError::from_backend(e))
    }

    pub fn mesh_create(&self) -> Result<MeshInfo, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        self.rt.block_on(r.backend().mesh_create())
            .map_err(|e| LatticeError::from_backend(e))
    }

    pub fn mesh_status(&self, mesh_id: String) -> Result<MeshInfo, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        let id = Uuid::parse_str(&mesh_id).map_err(|e| LatticeError::InvalidUuid { reason: e.to_string() })?;
        self.rt.block_on(r.backend().mesh_status(id))
            .map_err(|e| LatticeError::from_backend(e))
    }

    pub fn mesh_join(&self, token: String) -> Result<JoinResponse, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        self.rt.block_on(r.backend().mesh_join(&token))
            .map(|id| JoinResponse { mesh_id: id.as_bytes().to_vec() })
            .map_err(|e| LatticeError::from_backend(e))
    }
    
    pub fn mesh_invite(&self, mesh_id: String) -> Result<String, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        let id = Uuid::parse_str(&mesh_id).map_err(|e| LatticeError::InvalidUuid { reason: e.to_string() })?;
        self.rt.block_on(r.backend().mesh_invite(id))
            .map_err(|e| LatticeError::from_backend(e))
    }

    pub fn mesh_peers(&self, mesh_id: String) -> Result<Vec<PeerInfo>, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        let id = Uuid::parse_str(&mesh_id).map_err(|e| LatticeError::InvalidUuid { reason: e.to_string() })?;
        self.rt.block_on(r.backend().mesh_peers(id))
            .map_err(|e| LatticeError::from_backend(e))
    }

    pub fn mesh_revoke(&self, mesh_id: String, peer_key: Vec<u8>) -> Result<(), LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        let id = Uuid::parse_str(&mesh_id).map_err(|e| LatticeError::InvalidUuid { reason: e.to_string() })?;
        self.rt.block_on(r.backend().mesh_revoke(id, &peer_key))
             .map_err(|e| LatticeError::from_backend(e))
    }

    // Store
    pub fn store_create(&self, mesh_id: String, name: Option<String>, store_type: String) -> Result<StoreInfo, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        let id = Uuid::parse_str(&mesh_id).map_err(|e| LatticeError::InvalidUuid { reason: e.to_string() })?;
        self.rt.block_on(r.backend().store_create(id, name, &store_type))
            .map(Into::into)
            .map_err(|e| LatticeError::from_backend(e))
    }

    pub fn store_list(&self, mesh_id: String) -> Result<Vec<StoreInfo>, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        let id = Uuid::parse_str(&mesh_id).map_err(|e| LatticeError::InvalidUuid { reason: e.to_string() })?;
        self.rt.block_on(r.backend().store_list(id))
            .map(|list| list.into_iter().map(Into::into).collect())
            .map_err(|e| LatticeError::from_backend(e))
    }

    pub fn store_status(&self, store_id: String) -> Result<StoreInfo, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        let id = Uuid::parse_str(&store_id).map_err(|e| LatticeError::InvalidUuid { reason: e.to_string() })?;
        self.rt.block_on(r.backend().store_status(id))
            .map(Into::into)
            .map_err(|e| LatticeError::from_backend(e))
    }

    pub fn store_delete(&self, store_id: String) -> Result<(), LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        let id = Uuid::parse_str(&store_id).map_err(|e| LatticeError::InvalidUuid { reason: e.to_string() })?;
        self.rt.block_on(r.backend().store_delete(id))
            .map_err(|e| LatticeError::from_backend(e))
    }

    pub fn store_sync(&self, store_id: String) -> Result<(), LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        let id = Uuid::parse_str(&store_id).map_err(|e| LatticeError::InvalidUuid { reason: e.to_string() })?;
        self.rt.block_on(r.backend().store_sync(id))
            .map(|_| ())
            .map_err(|e| LatticeError::from_backend(e))
    }

    pub fn store_history(&self, store_id: String) -> Result<Vec<HistoryEntry>, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        let id = Uuid::parse_str(&store_id).map_err(|e| LatticeError::InvalidUuid { reason: e.to_string() })?;
        self.rt.block_on(r.backend().store_history(id))
            .map_err(|e| LatticeError::from_backend(e))
    }
    
    pub fn store_author_state(&self, store_id: String, author: Option<Vec<u8>>) -> Result<Vec<AuthorState>, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        let id = Uuid::parse_str(&store_id).map_err(|e| LatticeError::InvalidUuid { reason: e.to_string() })?;
        self.rt.block_on(r.backend().store_author_state(id, author.as_deref()))
            .map_err(|e| LatticeError::from_backend(e))
    }

    pub fn store_orphan_cleanup(&self, store_id: String) -> Result<CleanupResult, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        let id = Uuid::parse_str(&store_id).map_err(|e| LatticeError::InvalidUuid { reason: e.to_string() })?;
        self.rt.block_on(r.backend().store_orphan_cleanup(id))
            .map(|orphans_removed| CleanupResult { orphans_removed })
            .map_err(|e| LatticeError::from_backend(e))
    }

    // Dynamic Reflection
    
    pub fn store_inspect(&self, store_id: String) -> Result<Vec<MethodInfo>, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        let id = Uuid::parse_str(&store_id).map_err(|e| LatticeError::InvalidUuid { reason: e.to_string() })?;
        
        let (descriptor_bytes, service_name) = self.rt.block_on(r.backend().store_get_descriptor(id))
             .map_err(|e| LatticeError::from_backend(e))?;

        let pool = DescriptorPool::decode(descriptor_bytes.as_slice())
            .map_err(|e| LatticeError::Internal { message: format!("Failed to decode descriptors: {}", e) })?;
            
        let service = pool.get_service_by_name(&service_name)
            .ok_or_else(|| LatticeError::Internal { message: format!("Service '{}' not found", service_name) })?;
            
        let mut methods = Vec::new();
        for method in service.methods() {
            let input_desc = method.input();
            let mut fields = Vec::new();
            
            for field in input_desc.fields() {
                fields.push(FieldInfo {
                    name: field.name().to_string(),
                    type_kind: map_kind_to_field_type(field.kind()),
                    type_name: format!("{:?}", field.kind()), 
                    is_repeated: field.is_list(),
                });
            }
            
            methods.push(MethodInfo {
                name: method.name().to_string(),
                input_fields: fields,
            });
        }
        
        Ok(methods)
    }
    
    pub fn store_exec_dynamic(&self, store_id: String, method_name: String, args: Vec<ArgValue>) -> Result<Vec<u8>, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        let id = Uuid::parse_str(&store_id).map_err(|e| LatticeError::InvalidUuid { reason: e.to_string() })?;
        
        let (descriptor_bytes, service_name) = self.rt.block_on(r.backend().store_get_descriptor(id))
             .map_err(|e| LatticeError::from_backend(e))?;

        let pool = DescriptorPool::decode(descriptor_bytes.as_slice())
            .map_err(|e| LatticeError::from_backend(e))?;
            
        let service = pool.get_service_by_name(&service_name)
            .ok_or_else(|| LatticeError::Internal { message: format!("Service '{}' not found", service_name) })?;
            
        let method = service.methods().find(|m| m.name().eq_ignore_ascii_case(&method_name))
            .ok_or_else(|| LatticeError::Internal { message: format!("Method '{}' not found", method_name) })?;
            
        let input_desc = method.input();
        let mut dynamic_msg = DynamicMessage::new(input_desc.clone());
        
        // Map positional args to fields
        let fields: Vec<_> = input_desc.fields().collect();
        if args.len() > fields.len() {
             return Err(LatticeError::Internal { message: format!("Too many arguments. Expected {}, got {}", fields.len(), args.len()) });
        }
        
        for (i, arg) in args.iter().enumerate() {
            let field = &fields[i];
            let value = map_arg_to_value(arg, field.kind())?;
            dynamic_msg.set_field(field, value);
        }
        
        let mut payload = Vec::new();
        dynamic_msg.encode(&mut payload).map_err(|e| LatticeError::from_backend(e))?;
        
        // Execute
        let result_bytes = self.rt.block_on(r.backend().store_exec(id, method.name(), &payload))
            .map_err(|e| LatticeError::from_backend(e))?;
            
        Ok(result_bytes)
    }

    pub fn get_store_descriptor(&self, store_id: String) -> Result<DescriptorInfo, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        let id = Uuid::parse_str(&store_id).map_err(|e| LatticeError::InvalidUuid { reason: e.to_string() })?;
        
        let (bytes, name) = self.rt.block_on(r.backend().store_get_descriptor(id))
             .map_err(|e| LatticeError::from_backend(e))?;
             
        Ok(DescriptorInfo { descriptor_bytes: bytes, service_name: name })
    }

    // Events are special - they should return a blocking stream iterator?
    // UniFFI iterators?
    // Or we keep EventStream but it has blocking `next`?
    pub fn subscribe(&self) -> Result<Arc<LatticeEventStream>, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        
        // This must be async to get receiver
        // But backend().subscribe() returns sync? No, async.
        let rx = self.rt.block_on(async {
            r.backend().subscribe().map_err(|e| LatticeError::from_backend(e))
        })?;
        
        Ok(Arc::new(LatticeEventStream {
            rx: Arc::new(tokio::sync::Mutex::new(rx)),
            rt: self.rt.handle().clone(),
        }))
    }
}

#[derive(uniffi::Record)]
pub struct DescriptorInfo {
    pub descriptor_bytes: Vec<u8>,
    pub service_name: String,
}

#[derive(uniffi::Object)]
pub struct LatticeEventStream {
    rx: Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<lattice_runtime::NodeEvent>>>,
    rt: tokio::runtime::Handle,
}

#[uniffi::export]
impl LatticeEventStream {
    // Blocking next()
    pub fn next(&self) -> Option<BackendEvent> {
        self.rt.block_on(async {
            let mut rx = self.rx.lock().await;
            rx.recv().await.map(Into::into)
        })
    }
}

// Helpers for Reflection

fn map_kind_to_field_type(kind: Kind) -> FieldType {
    match kind {
        Kind::Double | Kind::Float => FieldType::Float,
        Kind::Int32 | Kind::Int64 | Kind::Sint32 | Kind::Sint64 | Kind::Sfixed32 | Kind::Sfixed64 => FieldType::Int,
        Kind::Uint32 | Kind::Uint64 | Kind::Fixed32 | Kind::Fixed64 => FieldType::Int, // Treat huge uints as Int (Swift handles Int64)
        Kind::Bool => FieldType::Bool,
        Kind::String => FieldType::String,
        Kind::Bytes => FieldType::Bytes,
        Kind::Message(_) => FieldType::Message,
        Kind::Enum(_) => FieldType::Enum,
    }
}

fn map_arg_to_value(arg: &ArgValue, kind: Kind) -> Result<Value, LatticeError> {
    match (arg, kind.clone()) {
        (ArgValue::String(s), Kind::String) => Ok(Value::String(s.clone())),
        (ArgValue::Int(i), Kind::Int32 | Kind::Sint32 | Kind::Sfixed32) => Ok(Value::I32(*i as i32)),
        (ArgValue::Int(i), Kind::Int64 | Kind::Sint64 | Kind::Sfixed64) => Ok(Value::I64(*i)),
        (ArgValue::Int(i), Kind::Uint32 | Kind::Fixed32) => Ok(Value::U32(*i as u32)),
        (ArgValue::Int(i), Kind::Uint64 | Kind::Fixed64) => Ok(Value::U64(*i as u64)),
        (ArgValue::Float(f), Kind::Float) => Ok(Value::F32(*f as f32)),
        (ArgValue::Float(f), Kind::Double) => Ok(Value::F64(*f)),
        (ArgValue::Bool(b), Kind::Bool) => Ok(Value::Bool(*b)),
        (ArgValue::Bytes(b), Kind::Bytes) => Ok(Value::Bytes(bytes::Bytes::copy_from_slice(b))),
        // Basic coercions using CLI-like logic if mismatched
        (ArgValue::String(s), Kind::Int32) => s.parse::<i32>().map(Value::I32).map_err(|e| LatticeError::Internal { message: e.to_string() }),
        (ArgValue::String(s), Kind::Int64) => s.parse::<i64>().map(Value::I64).map_err(|e| LatticeError::Internal { message: e.to_string() }),
        _ => Err(LatticeError::Internal { message: format!("Type mismatch for arg {:?} vs kind {:?}", arg, kind) }),
    }
}
