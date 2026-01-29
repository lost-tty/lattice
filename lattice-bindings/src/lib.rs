uniffi::setup_scaffolding!();

use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;
use prost_reflect::{DescriptorPool, DynamicMessage, Value, Kind};
use prost_reflect::prost::Message;

/// Format Vec<u8> as UUID string (for IDs)
fn format_uuid(bytes: &[u8]) -> String {
    Uuid::from_slice(bytes)
        .map(|u| u.to_string())
        .unwrap_or_else(|_| hex::encode(bytes))
}

/// Convert String to Option<String> (empty = None)
fn opt_string(s: String) -> Option<String> {
    if s.is_empty() { None } else { Some(s) }
}

#[derive(thiserror::Error, Debug, uniffi::Error)]
pub enum LatticeError {
    #[error("Runtime error: {0}")]
    Runtime(String),
    #[error("Not initialized - call start() first")]
    NotInitialized,
}

// Data Types

#[derive(uniffi::Record)]
pub struct MeshInfo {
    pub id: String,
    pub alias: String,
    pub peer_count: u32,
    pub store_count: u32,
}

#[derive(uniffi::Record)]
pub struct StoreInfo {
    pub id: String,
    pub name: Option<String>,
    pub store_type: String,
    pub archived: bool,
    pub details: Option<StoreDetails>,  // Populated by store_status, None for store_list
}

#[derive(uniffi::Record)]
pub struct NodeStatus {
    pub public_key: Vec<u8>,
    pub display_name: Option<String>,
    pub data_path: String,
    pub mesh_count: u32,
    pub peer_count: u32,
}

#[derive(uniffi::Record)]
pub struct StoreDetails {
    pub author_count: u32,
    pub log_file_count: u32,
    pub log_bytes: u64,
    pub orphan_count: u32,
}

#[derive(uniffi::Record)]
pub struct JoinResponse {
    pub mesh_id: String,
}

#[derive(uniffi::Record)]
pub struct PeerInfo {
    pub public_key: Vec<u8>,
    pub status: String,
    pub online: bool,
    pub name: Option<String>,
    pub last_seen_ms: Option<u64>,
}

#[derive(uniffi::Record)]
pub struct HistoryEntry {
    pub seq: u64,
    pub author: Vec<u8>,
    pub payload: Vec<u8>,
    pub timestamp: u64,
    pub hash: Vec<u8>,
    pub prev_hash: Vec<u8>,
    pub causal_deps: Vec<Vec<u8>>,
    pub summary: String,
}

#[derive(uniffi::Record)]
pub struct AuthorState {
    pub public_key: Vec<u8>,
    pub seq: u64,
    pub hash: Vec<u8>,
}

#[derive(uniffi::Record)]
pub struct CleanupResult {
    pub orphans_removed: u32,
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

#[derive(uniffi::Enum)]
pub enum BackendEvent {
    MeshReady { mesh_id: String },
    StoreReady { mesh_id: String, store_id: String },
    JoinFailed { mesh_id: String, reason: String },
    SyncResult { store_id: String, peers_synced: u32, entries_sent: u64, entries_received: u64 },
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
        
        let rt = self.rt.block_on(builder.build()).map_err(|e| LatticeError::Runtime(e.to_string()))?;
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
            .map(Into::into)
            .map_err(|e| LatticeError::Runtime(e.to_string()))
    }

    pub fn set_name(&self, name: String) -> Result<(), LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        self.rt.block_on(r.backend().node_set_name(&name)).map_err(|e| LatticeError::Runtime(e.to_string()))
    }

    // Mesh
    pub fn mesh_list(&self) -> Result<Vec<MeshInfo>, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        self.rt.block_on(r.backend().mesh_list())
            .map(|list| list.into_iter().map(Into::into).collect())
            .map_err(|e| LatticeError::Runtime(e.to_string()))
    }

    pub fn mesh_create(&self) -> Result<MeshInfo, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        self.rt.block_on(r.backend().mesh_create())
            .map(Into::into)
            .map_err(|e| LatticeError::Runtime(e.to_string()))
    }

    pub fn mesh_status(&self, mesh_id: String) -> Result<MeshInfo, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        let id = Uuid::parse_str(&mesh_id).map_err(|e| LatticeError::Runtime(e.to_string()))?;
        self.rt.block_on(r.backend().mesh_status(id))
            .map(Into::into)
            .map_err(|e| LatticeError::Runtime(e.to_string()))
    }

    pub fn mesh_join(&self, token: String) -> Result<JoinResponse, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        self.rt.block_on(r.backend().mesh_join(&token))
            .map(|id| JoinResponse { mesh_id: id.to_string() })
            .map_err(|e| LatticeError::Runtime(e.to_string()))
    }
    
    pub fn mesh_invite(&self, mesh_id: String) -> Result<String, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        let id = Uuid::parse_str(&mesh_id).map_err(|e| LatticeError::Runtime(e.to_string()))?;
        self.rt.block_on(r.backend().mesh_invite(id))
            .map_err(|e| LatticeError::Runtime(e.to_string()))
    }

    pub fn mesh_peers(&self, mesh_id: String) -> Result<Vec<PeerInfo>, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        let id = Uuid::parse_str(&mesh_id).map_err(|e| LatticeError::Runtime(e.to_string()))?;
        self.rt.block_on(r.backend().mesh_peers(id))
            .map(|list| list.into_iter().map(Into::into).collect())
            .map_err(|e| LatticeError::Runtime(e.to_string()))
    }

    pub fn mesh_revoke(&self, mesh_id: String, peer_key: Vec<u8>) -> Result<(), LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        let id = Uuid::parse_str(&mesh_id).map_err(|e| LatticeError::Runtime(e.to_string()))?;
        self.rt.block_on(r.backend().mesh_revoke(id, &peer_key))
             .map_err(|e| LatticeError::Runtime(e.to_string()))
    }

    // Store
    pub fn store_create(&self, mesh_id: String, name: Option<String>, store_type: String) -> Result<StoreInfo, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        let id = Uuid::parse_str(&mesh_id).map_err(|e| LatticeError::Runtime(e.to_string()))?;
        self.rt.block_on(r.backend().store_create(id, name, &store_type))
            .map(Into::into)
            .map_err(|e| LatticeError::Runtime(e.to_string()))
    }

    pub fn store_list(&self, mesh_id: String) -> Result<Vec<StoreInfo>, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        let id = Uuid::parse_str(&mesh_id).map_err(|e| LatticeError::Runtime(e.to_string()))?;
        self.rt.block_on(r.backend().store_list(id))
            .map(|list| list.into_iter().map(Into::into).collect())
            .map_err(|e| LatticeError::Runtime(e.to_string()))
    }

    pub fn store_status(&self, store_id: String) -> Result<StoreInfo, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        let id = Uuid::parse_str(&store_id).map_err(|e| LatticeError::Runtime(e.to_string()))?;
        self.rt.block_on(r.backend().store_status(id))
            .map(Into::into)
            .map_err(|e| LatticeError::Runtime(e.to_string()))
    }

    pub fn store_delete(&self, store_id: String) -> Result<(), LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        let id = Uuid::parse_str(&store_id).map_err(|e| LatticeError::Runtime(e.to_string()))?;
        self.rt.block_on(r.backend().store_delete(id))
            .map_err(|e| LatticeError::Runtime(e.to_string()))
    }

    pub fn store_sync(&self, store_id: String) -> Result<(), LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        let id = Uuid::parse_str(&store_id).map_err(|e| LatticeError::Runtime(e.to_string()))?;
        self.rt.block_on(r.backend().store_sync(id))
            .map(|_| ())
            .map_err(|e| LatticeError::Runtime(e.to_string()))
    }

    pub fn store_history(&self, store_id: String) -> Result<Vec<HistoryEntry>, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        let id = Uuid::parse_str(&store_id).map_err(|e| LatticeError::Runtime(e.to_string()))?;
        self.rt.block_on(r.backend().store_history(id))
            .map(|list| list.into_iter().map(Into::into).collect())
            .map_err(|e| LatticeError::Runtime(e.to_string()))
    }
    
    pub fn store_author_state(&self, store_id: String, author: Option<Vec<u8>>) -> Result<Vec<AuthorState>, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        let id = Uuid::parse_str(&store_id).map_err(|e| LatticeError::Runtime(e.to_string()))?;
        self.rt.block_on(r.backend().store_author_state(id, author.as_deref()))
            .map(|list| list.into_iter().map(Into::into).collect())
            .map_err(|e| LatticeError::Runtime(e.to_string()))
    }

    pub fn store_orphan_cleanup(&self, store_id: String) -> Result<CleanupResult, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        let id = Uuid::parse_str(&store_id).map_err(|e| LatticeError::Runtime(e.to_string()))?;
        self.rt.block_on(r.backend().store_orphan_cleanup(id))
            .map(|orphans_removed| CleanupResult { orphans_removed })
            .map_err(|e| LatticeError::Runtime(e.to_string()))
    }

    // Dynamic Reflection
    
    pub fn store_inspect(&self, store_id: String) -> Result<Vec<MethodInfo>, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        let id = Uuid::parse_str(&store_id).map_err(|e| LatticeError::Runtime(e.to_string()))?;
        
        let (descriptor_bytes, service_name) = self.rt.block_on(r.backend().store_get_descriptor(id))
             .map_err(|e| LatticeError::Runtime(e.to_string()))?;

        let pool = DescriptorPool::decode(descriptor_bytes.as_slice())
            .map_err(|e| LatticeError::Runtime(format!("Failed to decode descriptors: {}", e)))?;
            
        let service = pool.get_service_by_name(&service_name)
            .ok_or_else(|| LatticeError::Runtime(format!("Service '{}' not found", service_name)))?;
            
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
        let id = Uuid::parse_str(&store_id).map_err(|e| LatticeError::Runtime(e.to_string()))?;
        
        let (descriptor_bytes, service_name) = self.rt.block_on(r.backend().store_get_descriptor(id))
             .map_err(|e| LatticeError::Runtime(e.to_string()))?;

        let pool = DescriptorPool::decode(descriptor_bytes.as_slice())
            .map_err(|e| LatticeError::Runtime(e.to_string()))?;
            
        let service = pool.get_service_by_name(&service_name)
            .ok_or_else(|| LatticeError::Runtime(format!("Service '{}' not found", service_name)))?;
            
        let method = service.methods().find(|m| m.name().eq_ignore_ascii_case(&method_name))
            .ok_or_else(|| LatticeError::Runtime(format!("Method '{}' not found", method_name)))?;
            
        let input_desc = method.input();
        let mut dynamic_msg = DynamicMessage::new(input_desc.clone());
        
        // Map positional args to fields
        let fields: Vec<_> = input_desc.fields().collect();
        if args.len() > fields.len() {
             return Err(LatticeError::Runtime(format!("Too many arguments. Expected {}, got {}", fields.len(), args.len())));
        }
        
        for (i, arg) in args.iter().enumerate() {
            let field = &fields[i];
            let value = map_arg_to_value(arg, field.kind())?;
            dynamic_msg.set_field(field, value);
        }
        
        let mut payload = Vec::new();
        dynamic_msg.encode(&mut payload).map_err(|e| LatticeError::Runtime(e.to_string()))?;
        
        // Execute
        let result_bytes = self.rt.block_on(r.backend().store_exec(id, method.name(), &payload))
            .map_err(|e| LatticeError::Runtime(e.to_string()))?;
            
        Ok(result_bytes)
    }

    pub fn get_store_descriptor(&self, store_id: String) -> Result<DescriptorInfo, LatticeError> {
        let r_guard = self.rt.block_on(self.runtime.read());
        let r = r_guard.as_ref().ok_or(LatticeError::NotInitialized)?;
        let id = Uuid::parse_str(&store_id).map_err(|e| LatticeError::Runtime(e.to_string()))?;
        
        let (bytes, name) = self.rt.block_on(r.backend().store_get_descriptor(id))
             .map_err(|e| LatticeError::Runtime(e.to_string()))?;
             
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
            r.backend().subscribe().map_err(|e| LatticeError::Runtime(e.to_string()))
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
    rx: Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<lattice_runtime::BackendEvent>>>,
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
        (ArgValue::String(s), Kind::Int32) => s.parse::<i32>().map(Value::I32).map_err(|e| LatticeError::Runtime(e.to_string())),
        (ArgValue::String(s), Kind::Int64) => s.parse::<i64>().map(Value::I64).map_err(|e| LatticeError::Runtime(e.to_string())),
        _ => Err(LatticeError::Runtime(format!("Type mismatch for arg {:?} vs kind {:?}", arg, kind))),
    }
}

// Conversions (same as before)

impl From<lattice_runtime::NodeStatus> for NodeStatus {
    fn from(s: lattice_runtime::NodeStatus) -> Self {
        Self {
            public_key: s.public_key,
            display_name: opt_string(s.display_name),
            data_path: s.data_path,
            mesh_count: s.mesh_count,
            peer_count: s.peer_count,
        }
    }
}

impl From<lattice_runtime::MeshInfo> for MeshInfo {
    fn from(m: lattice_runtime::MeshInfo) -> Self {
        let id_str = format_uuid(&m.id);
        let alias = id_str.get(..8).unwrap_or(&id_str).to_string();
        Self {
            id: id_str,
            alias,
            peer_count: m.peer_count,
            store_count: m.store_count,
        }
    }
}

impl From<lattice_runtime::StoreInfo> for StoreInfo {
    fn from(s: lattice_runtime::StoreInfo) -> Self {
        Self {
            id: format_uuid(&s.id),
            name: opt_string(s.name),
            store_type: s.store_type,
            archived: s.archived,
            details: s.details.map(Into::into),
        }
    }
}

impl From<lattice_runtime::StoreDetails> for StoreDetails {
    fn from(d: lattice_runtime::StoreDetails) -> Self {
        Self {
            author_count: d.author_count,
            log_file_count: d.log_file_count,
            log_bytes: d.log_bytes,
            orphan_count: d.orphan_count,
        }
    }
}

impl From<lattice_runtime::PeerInfo> for PeerInfo {
    fn from(p: lattice_runtime::PeerInfo) -> Self {
        Self {
            public_key: p.public_key,
            status: p.status,
            online: p.online,
            name: opt_string(p.name),
            last_seen_ms: if p.last_seen_ms > 0 { Some(p.last_seen_ms) } else { None },
        }
    }
}

impl From<lattice_runtime::HistoryEntry> for HistoryEntry {
    fn from(e: lattice_runtime::HistoryEntry) -> Self {
        Self {
            seq: e.seq,
            author: e.author,
            payload: e.payload,
            timestamp: e.timestamp,
            hash: e.hash,
            prev_hash: e.prev_hash,
            causal_deps: e.causal_deps,
            summary: e.summary,
        }
    }
}

impl From<lattice_runtime::AuthorState> for AuthorState {
    fn from(a: lattice_runtime::AuthorState) -> Self {
        Self {
            public_key: a.public_key,
            seq: a.seq,
            hash: a.hash,
        }
    }
}

impl From<lattice_runtime::BackendEvent> for BackendEvent {
    fn from(e: lattice_runtime::BackendEvent) -> Self {
        match e {
            lattice_runtime::BackendEvent::MeshReady { mesh_id } => 
                BackendEvent::MeshReady { mesh_id: mesh_id.to_string() },
            lattice_runtime::BackendEvent::StoreReady { mesh_id, store_id } => 
                BackendEvent::StoreReady { mesh_id: mesh_id.to_string(), store_id: store_id.to_string() },
            lattice_runtime::BackendEvent::JoinFailed { mesh_id, reason } => 
                BackendEvent::JoinFailed { mesh_id: mesh_id.to_string(), reason },
            lattice_runtime::BackendEvent::SyncResult { store_id, peers_synced, entries_sent, entries_received } => 
                BackendEvent::SyncResult { 
                    store_id: store_id.to_string(), 
                    peers_synced, 
                    entries_sent, 
                    entries_received 
                },
        }
    }
}
