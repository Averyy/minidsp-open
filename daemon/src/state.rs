//! Shared daemon state — device registry, broadcast channels.

use minidsp::client::MiniDspClient;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Shared daemon state accessible by all HTTP/WS/TCP handlers.
pub struct DaemonState {
    pub devices: RwLock<HashMap<usize, Arc<MiniDspClient>>>,
}

impl DaemonState {
    pub fn new() -> Self {
        Self {
            devices: RwLock::new(HashMap::new()),
        }
    }

    pub async fn add_device(&self, id: usize, client: Arc<MiniDspClient>) {
        self.devices.write().await.insert(id, client);
    }

    pub async fn remove_device(&self, id: usize) {
        self.devices.write().await.remove(&id);
    }

    pub async fn get_device(&self, id: usize) -> Option<Arc<MiniDspClient>> {
        self.devices.read().await.get(&id).cloned()
    }

    pub async fn device_count(&self) -> usize {
        self.devices.read().await.len()
    }
}

impl Default for DaemonState {
    fn default() -> Self {
        Self::new()
    }
}
