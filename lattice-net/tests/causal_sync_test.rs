use lattice_node::{NodeBuilder, Node, DataDir, Invite, NodeEvent};
use lattice_net::{LatticeEndpoint, NetworkService, ToLattice};
use lattice_kvstore_client::KvStoreExt;
use lattice_model::Uuid;
use std::sync::Arc;
use tokio::time::{timeout, Duration, sleep};
use futures_util::StreamExt;
use lattice_kernel::proto::weaver::WitnessContent;
use prost::Message;

async fn create_node(dir: &std::path::Path) -> (Arc<Node>, Arc<NetworkService>) {
    let (net_tx, net_rx) = NetworkService::create_net_channel();
    let data_dir = DataDir::new(dir.to_path_buf());
    
    use lattice_node::{STORE_TYPE_KVSTORE, direct_opener};
    
    let mut builder = NodeBuilder::new(data_dir.clone())
        .with_net_tx(net_tx);

    builder = builder.with_opener(STORE_TYPE_KVSTORE, |registry| {
        type PersistentKvState = lattice_systemstore::SystemLayer<lattice_storage::PersistentState<lattice_kvstore::KvState>>;
        direct_opener::<PersistentKvState>(registry)
    });

    let node = builder.build().unwrap();
    let node = Arc::new(node);
    
    let endpoint = LatticeEndpoint::new(node.signing_key().clone()).await.unwrap();
    let net = NetworkService::new_with_provider(node.clone(), endpoint, net_rx).await.unwrap();
    
    (node, net)
}

#[tokio::test]
async fn test_sync_preserves_causal_order_in_witness_log() {
    let _ = tracing_subscriber::fmt::try_init(); 
    use lattice_node::STORE_TYPE_KVSTORE;
    
    let base_dir = std::env::temp_dir().join(format!("lattice_causal_{}", Uuid::new_v4()));
    std::fs::create_dir_all(&base_dir).unwrap();
    let path_a = base_dir.join("node_a");
    let path_b = base_dir.join("node_b");
    
    // 1. Setup Node A and Node B
    let (node_a, net_a) = create_node(&path_a).await;
    let (node_b, _net_b) = create_node(&path_b).await;
    
    // Create Root Store on A
    let root_id = node_a.create_store(None, Some("root".into()), STORE_TYPE_KVSTORE).await.unwrap();
    let root_handle_a = node_a.store_manager().get_handle(&root_id).unwrap();
    
    // Let's use `put` to create a natural chain. 
    // They are guaranteed to be sequentially causally linked within the same store IF written to the SAME key.
    root_handle_a.put(b"key1".to_vec(), b"val1".to_vec()).await.unwrap();
    root_handle_a.put(b"key1".to_vec(), b"val2".to_vec()).await.unwrap();
    root_handle_a.put(b"key1".to_vec(), b"val3".to_vec()).await.unwrap();
    
    // Get the witness log from A
    let a_provider = root_handle_a.as_sync_provider();
    let mut log_a = Vec::new();
    let mut a_stream = a_provider.scan_witness_log(None, 100);
    while let Some(Ok(record)) = a_stream.next().await {
        log_a.push(record);
    }
    
    // The store creation actually injects a few initial intentions (e.g. node info, ACLs).
    // The last 3 are the ones we just created in a linear chain.
    let len_a = log_a.len();
    assert!(len_a >= 3);
    
    // Get the exact hashes in causal order
    let hash1 = WitnessContent::decode(log_a[len_a - 3].content.as_slice()).unwrap().intention_hash;
    let hash2 = WitnessContent::decode(log_a[len_a - 2].content.as_slice()).unwrap().intention_hash;
    let hash3 = WitnessContent::decode(log_a[len_a - 1].content.as_slice()).unwrap().intention_hash;

    // 2. Node B joins Root
    let invite_code = node_a.store_manager().create_invite(root_id, node_b.node_id()).await.unwrap();
    let invite = Invite::parse(&invite_code).unwrap();
    
    let mut rx_b = node_b.subscribe();
    node_b.join(net_a.endpoint().public_key().to_lattice(), root_id, invite.secret).unwrap();
    
    // Wait for StoreReady
    let root_handle_b = timeout(Duration::from_secs(10), async {
        loop {
            if let Some(h) = node_b.store_manager().get_handle(&root_id) {
                return h;
            }
            if let Ok(NodeEvent::StoreReady { store_id }) = rx_b.recv().await {
                if store_id == root_id {
                    return node_b.store_manager().get_handle(&root_id).unwrap();
                }
            }
        }
    }).await.expect("Failed to get root handle on Node B");

    // Wait for Sync
    timeout(Duration::from_secs(10), async {
        loop {
            let root_a_fp = a_provider.table_fingerprint().await.expect("Failed to get Node A fingerprint");
            let root_b_fp = root_handle_b.as_sync_provider().table_fingerprint().await.expect("Failed to get Node B fingerprint");
            if root_a_fp == root_b_fp {
                break;
            }
            sleep(Duration::from_millis(50)).await;
        }
    }).await.expect("Node B failed to sync (fingerprint mismatch)");

    // 3. Verify Witness Log on Node B matches the causal order
    let b_provider = root_handle_b.as_sync_provider();
    let mut log_b = Vec::new();
    let mut b_stream = b_provider.scan_witness_log(None, 100);
    while let Some(Ok(record)) = b_stream.next().await {
        log_b.push(record);
    }
    
    // Find the relative positions of our causally-linked intentions in B's log
    let mut index1 = None;
    let mut index2 = None;
    let mut index3 = None;

    for (i, record) in log_b.iter().enumerate() {
        let content = WitnessContent::decode(record.content.as_slice()).unwrap();
        let h = content.intention_hash.as_slice();
        if h == hash1.as_slice() { index1 = Some(i); }
        if h == hash2.as_slice() { index2 = Some(i); }
        if h == hash3.as_slice() { index3 = Some(i); }
    }

    let i1 = index1.expect("hash1 not found in Node B log");
    let i2 = index2.expect("hash2 not found in Node B log");
    let i3 = index3.expect("hash3 not found in Node B log");

    // The core assertion: causal dependencies force the execution (and thus witness) order,
    // regardless of what order the network sync streams the bytes.
    assert!(i1 < i2, "hash1 MUST be applied before hash2 (causal chain)");
    assert!(i2 < i3, "hash2 MUST be applied before hash3 (causal chain)");


    // Cleanup
    let _ = std::fs::remove_dir_all(&base_dir);
}

