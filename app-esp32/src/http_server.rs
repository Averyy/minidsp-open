//! HTTP server for ESP32-S3 using picoserve.
//!
//! Serves the same API as the desktop daemon (axum) but using picoserve
//! for zero-heap-allocation HTTP handling.

use log::{info, warn};

/// Run the HTTP server.
///
/// Uses picoserve with a fixed number of connection slots.
/// Same API routes as the desktop daemon.
#[embassy_executor::task]
pub async fn run(device: Option<crate::usb_hid::MiniDspDevice>) {
    info!("HTTP server starting on port 5380");

    // TODO: Implement picoserve routes
    // Routes to implement (same as axum daemon):
    //   GET  /devices
    //   GET  /devices/{id}       (JSON or WebSocket upgrade)
    //   POST /devices/{id}       (set master status)
    //   POST /devices/{id}/config
    //   GET  /devices/{id}/config
    //   POST /devices/{id}/volume/{direction}
    //   POST /devices/{id}/source/{source}
    //   POST /devices/{id}/preset/{preset}
    //   GET  /devices/{id}/display
    //   POST /devices/{id}/display
    //   GET  /devices/{id}/dre
    //   POST /devices/{id}/dre
    //   GET  /devices/{id}/tokens
    //   POST /devices/{id}/preset/copy
    //   GET  /devices/{id}/ws     (raw frame bridge WebSocket)
    //   GET  /llms.txt
    //
    // JSON serialization: serde-json-core with fixed buffers
    // Response buffer: ~2KB per connection
    // Max connections: 4-8 concurrent

    // Placeholder: keep task alive
    loop {
        embassy_time::Timer::after(embassy_time::Duration::from_secs(60)).await;
    }
}
