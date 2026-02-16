use lattice_node::{Node, NodeBuilder, STORE_TYPE_KVSTORE, STORE_TYPE_LOGSTORE, direct_opener, StoreHandle, PeerManager};
use lattice_node::data_dir::DataDir;
use lattice_model::Uuid;
use std::sync::Arc;
use std::time::Duration;
use prost::Message; // For decode

// ==================== Test Helpers ====================

fn test_node_builder(data_dir: DataDir) -> NodeBuilder {
    type PersistentKvState = lattice_systemstore::SystemLayer<lattice_storage::PersistentState<lattice_kvstore::KvState>>;
    type PersistentLogState = lattice_systemstore::SystemLayer<lattice_storage::PersistentState<lattice_logstore::LogState>>;

    NodeBuilder::new(data_dir)
        .with_opener(STORE_TYPE_KVSTORE, |registry| direct_opener::<PersistentKvState>(registry))
        .with_opener(STORE_TYPE_LOGSTORE, |registry| direct_opener::<PersistentLogState>(registry))
}

struct TestCtx {
    _tmp: tempfile::TempDir,
    node: Node,
}

impl TestCtx {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = DataDir::new(tmp.path().to_path_buf());
        let node = test_node_builder(DataDir::new(data_dir.base())).build().unwrap();
        Self { _tmp: tmp, node }
    }

    fn sm(&self) -> &Arc<lattice_node::StoreManager> {
        self.node.store_manager()
    }
}

// Open a store as a replica (initially empty) without network join
async fn open_replica(ctx: &TestCtx, store_id: Uuid, store_type: &str) -> Arc<dyn StoreHandle> {
    // 1. Open (creates backend/state if missing by default for direct_opener)
    let handle = ctx.sm().open(store_id, store_type).expect("open replica");
    
    // 2. Create PeerManager
    let sys = handle.clone().as_system().expect("system store required");
    let pm = PeerManager::new(sys).await.expect("create peer manager");
    
    // 3. Register
    ctx.sm().register(store_id, handle.clone(), store_type, pm).expect("register replica");
    
    // 4. Start Watching
    ctx.sm().start_watching(store_id).expect("start watching replica");
    
    handle
}

// Manually sync intentions from src to dst (Simulating network sync)
async fn sync_stores(src: &Arc<dyn StoreHandle>, dst: &Arc<dyn StoreHandle>, _desc: &str) {
    let src_inspector = src.as_inspector();
    let dst_sync = dst.as_sync_provider();

    // Get all executed intentions from source log
    let log = src_inspector.witness_log().await;

    for (_, record) in log {
        // Fetch full intention content - record.content is WitnessContent protobuf
        let content = lattice_proto::weaver::WitnessContent::decode(record.content.as_slice())
            .expect("decode witness content");
        
        let intents = src_inspector.get_intention(content.intention_hash).await.expect("fetch intention");
        for intent in intents {
            // Ingest into destination
            // ingest_intention is idempotent (checks if already exists)
            match dst_sync.ingest_intention(intent).await {
                Ok(_) => {},
                Err(e) => {
                    // Ignore "AlreadyExists" or similar errors for simple convergence test
                    if !e.to_string().contains("already exists") {
                        // eprintln!("Ingest error: {}", e);
                    }
                }
            }
        }
    }
}

#[tokio::test]
async fn test_multi_author_convergence() {
    // 1. Setup 3 nodes
    let ctx1 = TestCtx::new();
    let ctx2 = TestCtx::new();
    let ctx3 = TestCtx::new();

    let _node1_id = ctx1.node.node_id();
    let node2_pub = ctx2.node.node_id();
    let node3_pub = ctx3.node.node_id();

    // 2. Create Root Store on Node 1 (Shared Store)
    let store_id = ctx1.node.create_store(None, Some("convergence-root".into()), STORE_TYPE_KVSTORE).await.unwrap();
    let handle1 = ctx1.sm().get_handle(&store_id).expect("handle1");

    // 3. Manually Initialize Replicas on Node 2 and 3 (Empty State)
    let handle2 = open_replica(&ctx2, store_id, STORE_TYPE_KVSTORE).await;
    let handle3 = open_replica(&ctx3, store_id, STORE_TYPE_KVSTORE).await;
    
    // 4. Register Peers Manually
    // Node 1 adds Node 2 and 3 as active peers so they are authorized.
    {
        let sys = handle1.clone().as_system().unwrap();
        lattice_systemstore::SystemBatch::new(sys.as_ref())
            .set_status(node2_pub, lattice_model::PeerStatus::Active)
            .set_status(node3_pub, lattice_model::PeerStatus::Active)
            .commit().await.unwrap();
    }

    // 5. Initial Sync: Propagate Node 1's history (Creation, Peer Adds) to 2 and 3
    sync_stores(&handle1, &handle2, "1->2 init").await;
    sync_stores(&handle1, &handle3, "1->3 init").await;
    
    // Sync back so everyone knows everyone is active (gossip simulation)
    sync_stores(&handle1, &handle2, "1->2 status").await;
    sync_stores(&handle1, &handle3, "1->3 status").await;

    // 6. Concurrent Writes: Each node Adds a unique Child Store
    let child_id_1 = Uuid::new_v4();
    let child_id_2 = Uuid::new_v4();
    let child_id_3 = Uuid::new_v4();

    println!("Node 1 writing child {}", child_id_1);
    {
        let sys = handle1.clone().as_system().unwrap();
        lattice_systemstore::SystemBatch::new(sys.as_ref())
            .add_child(child_id_1, "child-1".into(), STORE_TYPE_KVSTORE)
            .commit().await.unwrap();
    }
    
    println!("Node 2 writing child {}", child_id_2);
    {
        let sys = handle2.clone().as_system().unwrap();
        lattice_systemstore::SystemBatch::new(sys.as_ref())
            .add_child(child_id_2, "child-2".into(), STORE_TYPE_KVSTORE)
            .commit().await.unwrap();
    }

    println!("Node 3 writing child {}", child_id_3);
    {
        let sys = handle3.clone().as_system().unwrap();
        lattice_systemstore::SystemBatch::new(sys.as_ref())
            .add_child(child_id_3, "child-3".into(), STORE_TYPE_KVSTORE)
            .commit().await.unwrap();
    }

    // 7. Full Convergence Sync (Gossip - All to All)
    println!("Syncing...");
    let handles = vec![handle1.clone(), handle2.clone(), handle3.clone()];
    
    // Round 1: Everyone shares their own ops
    for i in 0..3 {
        for j in 0..3 {
            if i != j {
                sync_stores(&handles[i], &handles[j], &format!("{}->{}", i+1, j+1)).await;
            }
        }
    }
    // Round 2: Propagate relayed ops (ensure full convergence)
    for i in 0..3 {
        for j in 0..3 {
            if i != j {
                sync_stores(&handles[i], &handles[j], &format!("{}->{}", i+1, j+1)).await;
            }
        }
    }

    // 8. Verify Convergence
    println!("Verifying...");
    let expected_children = vec![child_id_1, child_id_2, child_id_3];
    
    for (idx, handle) in handles.iter().enumerate() {
        // Poll for async processing completion
        for _ in 0..20 {
            let sys = handle.clone().as_system().unwrap();
            let children = sys.get_children().unwrap(); 
            if expected_children.iter().all(|id| children.iter().any(|c| c.id == *id)) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        let sys = handle.clone().as_system().unwrap();
        let children = sys.get_children().unwrap();
        
        println!("Node {} children: {:?}", idx+1, children.iter().map(|c| c.id).collect::<Vec<_>>());
        
        for child_id in &expected_children {
             assert!(children.iter().any(|c| c.id == *child_id), 
                "Node {} missing child {}. Has: {:?}", idx + 1, child_id, children.iter().map(|c| c.id).collect::<Vec<_>>());
        }
    }
}
