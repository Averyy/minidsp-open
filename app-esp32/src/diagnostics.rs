//! ESP32 diagnostics: system info logging.
//!
//! Periodically collects and logs system stats (heap, uptime).
//! Debug endpoints are served by the HTTP server.

use log::info;

/// Collect and log system diagnostics (called periodically from main loop).
pub fn tick() {
    let heap_free = unsafe { esp_idf_svc::sys::esp_get_free_heap_size() };
    let heap_min = unsafe { esp_idf_svc::sys::esp_get_minimum_free_heap_size() };
    let uptime_ms = unsafe { esp_idf_svc::sys::esp_timer_get_time() / 1000 };
    let uptime_s = uptime_ms / 1000;

    info!(
        "heap: {heap_free} free ({heap_min} min), uptime: {}m {}s",
        uptime_s / 60,
        uptime_s % 60
    );
}

/// System info struct for /debug/info endpoint.
#[derive(serde::Serialize)]
pub struct SystemInfo {
    pub heap_free: u32,
    pub heap_min: u32,
    pub uptime_ms: u64,
}

impl SystemInfo {
    pub fn collect() -> Self {
        unsafe {
            Self {
                heap_free: esp_idf_svc::sys::esp_get_free_heap_size(),
                heap_min: esp_idf_svc::sys::esp_get_minimum_free_heap_size(),
                uptime_ms: (esp_idf_svc::sys::esp_timer_get_time() / 1000) as u64,
            }
        }
    }
}
