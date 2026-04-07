//! Shared daemon state — device registry, broadcast channels.

use minidsp::client::{MiniDspClient, SavedPeqState};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Shared daemon state accessible by all HTTP/WS/TCP handlers.
pub struct DaemonState {
    pub devices: RwLock<HashMap<usize, Arc<MiniDspClient>>>,
    saved_peq: RwLock<HashMap<usize, SavedPeqState>>,
}

impl DaemonState {
    pub fn new() -> Self {
        Self {
            devices: RwLock::new(HashMap::new()),
            saved_peq: RwLock::new(HashMap::new()),
        }
    }

    pub async fn add_device(&self, id: usize, client: Arc<MiniDspClient>) {
        if let Some(saved) = self.saved_peq.read().await.get(&id).cloned() {
            client.set_saved_measurement_state(Some(saved)).await;
        }
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

    pub async fn set_saved_peq(&self, id: usize, saved: SavedPeqState) {
        self.saved_peq.write().await.insert(id, saved);
    }

    pub async fn get_saved_peq(&self, id: usize) -> Option<SavedPeqState> {
        self.saved_peq.read().await.get(&id).cloned()
    }

    pub async fn clear_saved_peq(&self, id: usize) {
        self.saved_peq.write().await.remove(&id);
    }
}

impl Default for DaemonState {
    fn default() -> Self {
        Self::new()
    }
}
