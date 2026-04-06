//! ESP32 diagnostics: ring buffer logging, system info, debug endpoints.

use log::info;

/// Ring buffer for log capture (4KB).
const LOG_BUFFER_SIZE: usize = 4096;

/// Run the diagnostics task.
///
/// Provides:
/// - GET /debug/log — ring buffer snapshot
/// - GET /debug/info — JSON system diagnostics (heap, WiFi, USB, uptime)
/// - POST /debug/level — runtime log level change
/// - GET /api/system/status — data partition state
#[embassy_executor::task]
pub async fn run() {
    info!("diagnostics task started");

    // The diagnostics endpoints are served by the HTTP server.
    // This task manages the ring buffer and system state collection.

    // TODO: Implement custom log::Log that writes to ring buffer + UART
    // TODO: Periodic system info collection (heap free, WiFi RSSI, USB state)

    loop {
        embassy_time::Timer::after(embassy_time::Duration::from_secs(30)).await;
        // Collect system stats periodically
        unsafe {
            let heap_free = esp_idf_svc::sys::esp_get_free_heap_size();
            info!("diagnostics: heap_free={heap_free}");
        }
    }
}
