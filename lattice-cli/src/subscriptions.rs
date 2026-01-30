//! Subscription registry for managing active streams

use std::collections::HashMap;
use std::sync::RwLock;
use tokio::sync::mpsc;

pub type SubId = u64;

/// Registry of active subscriptions
pub struct SubscriptionRegistry {
    next_id: RwLock<SubId>,
    // sub_id -> (display_name, stream_name, cancel_tx)
    subs: RwLock<HashMap<SubId, (String, String, mpsc::Sender<()>)>>,
}

impl Default for SubscriptionRegistry {
    fn default() -> Self { Self::new() }
}

impl SubscriptionRegistry {
    pub fn new() -> Self {
        Self { next_id: RwLock::new(1), subs: RwLock::new(HashMap::new()) }
    }
    
    pub fn register(&self, name: String, _store_id: uuid::Uuid, stream_name: String, cancel_tx: mpsc::Sender<()>, _handle: tokio::task::JoinHandle<()>) -> SubId {
        let mut id_guard = self.next_id.write().unwrap();
        let id = *id_guard;
        *id_guard += 1;
        self.subs.write().unwrap().insert(id, (name, stream_name, cancel_tx));
        id
    }
    
    pub async fn stop(&self, id: SubId) -> Result<(), &'static str> {
        let (_, _, tx) = self.subs.write().unwrap().remove(&id).ok_or("Subscription not found")?;
        let _ = tx.send(()).await;
        Ok(())
    }
    
    pub async fn stop_all(&self) {
        let subs: Vec<_> = self.subs.write().unwrap().drain().collect();
        for (_, (_, _, tx)) in subs {
            let _ = tx.send(()).await;
        }
    }
    
    pub fn list(&self) -> Vec<(SubId, String, String)> {
        self.subs.read().unwrap().iter().map(|(&id, (name, stream, _))| (id, name.clone(), stream.clone())).collect()
    }
}
