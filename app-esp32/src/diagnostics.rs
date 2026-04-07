//! ESP32 diagnostics: ring buffer logging, system info, runtime log control.
//!
//! Custom `log::Log` implementation writes to both UART (via esp_log_write)
//! and a 4KB circular ring buffer. HTTP endpoints serve the buffer and
//! system info on demand.

use esp_idf_svc::sys::*;
use log::{Level, LevelFilter, Log, Metadata, Record};
use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

/// Ring buffer capacity in bytes (~80–100 log lines at INFO level).
const LOG_BUFFER_CAPACITY: usize = 4096;

// ---------------------------------------------------------------------------
// Ring buffer
// ---------------------------------------------------------------------------

struct LogRingBuffer {
    data: VecDeque<u8>,
}

impl LogRingBuffer {
    fn new() -> Self {
        Self {
            data: VecDeque::with_capacity(LOG_BUFFER_CAPACITY),
        }
    }

    /// Append a line. Evicts oldest complete lines if over capacity.
    fn push_line(&mut self, line: &str) {
        let bytes = line.as_bytes();
        let needed = bytes.len() + 1; // +1 for newline

        // Skip lines that would exceed total buffer capacity
        if needed > LOG_BUFFER_CAPACITY {
            return;
        }

        // Evict oldest lines to make room
        while self.data.len() + needed > LOG_BUFFER_CAPACITY {
            // Remove bytes up to and including the next newline
            if let Some(pos) = self.data.iter().position(|&b| b == b'\n') {
                self.data.drain(..=pos);
            } else {
                self.data.clear();
                break;
            }
        }

        self.data.extend(bytes.iter());
        self.data.push_back(b'\n');
    }

    /// Snapshot the buffer contents as a string.
    fn snapshot(&mut self) -> String {
        let slice = self.data.make_contiguous();
        String::from_utf8_lossy(slice).into_owned()
    }

    fn used(&self) -> usize {
        self.data.len()
    }
}

static RING_BUFFER: OnceLock<Mutex<LogRingBuffer>> = OnceLock::new();

fn ring_buffer() -> &'static Mutex<LogRingBuffer> {
    RING_BUFFER.get_or_init(|| Mutex::new(LogRingBuffer::new()))
}

// ---------------------------------------------------------------------------
// Custom logger
// ---------------------------------------------------------------------------

/// Logger that writes to both ESP-IDF UART and a RAM ring buffer.
struct DiagnosticsLogger {
    level: Mutex<LevelFilter>,
}

impl DiagnosticsLogger {
    fn new() -> Self {
        Self {
            level: Mutex::new(LevelFilter::Info),
        }
    }
}

impl Log for DiagnosticsLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        let max = *self.level.lock().unwrap_or_else(|e| e.into_inner());
        metadata.level() <= max
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }

        let level_char = match record.level() {
            Level::Error => 'E',
            Level::Warn => 'W',
            Level::Info => 'I',
            Level::Debug => 'D',
            Level::Trace => 'V',
        };

        let uptime_ms = unsafe { esp_timer_get_time() / 1000 };
        let tag = record.target();
        let line = format!("{level_char} ({uptime_ms}) {tag}: {}", record.args());

        // Write to ring buffer
        if let Ok(mut buf) = ring_buffer().lock() {
            buf.push_line(&line);
        }

        // Write to UART via ESP-IDF log system
        let esp_level = match record.level() {
            Level::Error => esp_log_level_t_ESP_LOG_ERROR,
            Level::Warn => esp_log_level_t_ESP_LOG_WARN,
            Level::Info => esp_log_level_t_ESP_LOG_INFO,
            Level::Debug => esp_log_level_t_ESP_LOG_DEBUG,
            Level::Trace => esp_log_level_t_ESP_LOG_VERBOSE,
        };

        // Use esp_log_write with "%s" to output pre-formatted text
        // Filter interior NUL bytes (CString::new fails on them)
        let c_tag = std::ffi::CString::new(tag.replace('\0', "")).unwrap_or_default();
        let c_msg = std::ffi::CString::new(line.replace('\0', "")).unwrap_or_default();
        unsafe {
            esp_log_write(
                esp_level,
                c_tag.as_ptr(),
                b"%s\n\0".as_ptr() as *const core::ffi::c_char,
                c_msg.as_ptr(),
            );
        }
    }

    fn flush(&self) {}
}

static LOGGER: OnceLock<DiagnosticsLogger> = OnceLock::new();

/// Initialize the diagnostics logger. Call once at boot, before any log macros.
pub fn init_logger() {
    let logger = LOGGER.get_or_init(DiagnosticsLogger::new);
    // Safety: called once before anything else logs.
    let _ = log::set_logger(logger);
    log::set_max_level(LevelFilter::Info);
}

// ---------------------------------------------------------------------------
// Public API for HTTP handlers
// ---------------------------------------------------------------------------

/// Return the ring buffer contents as a string.
pub fn log_snapshot() -> String {
    ring_buffer()
        .lock()
        .map(|mut buf| buf.snapshot())
        .unwrap_or_default()
}

/// Return ring buffer usage stats.
pub fn log_buffer_stats() -> (usize, usize) {
    let used = ring_buffer()
        .lock()
        .map(|buf| buf.used())
        .unwrap_or(0);
    (used, LOG_BUFFER_CAPACITY)
}

/// Set runtime log level. Returns the new level name.
pub fn set_log_level(level: &str) -> Option<&'static str> {
    let (filter, name) = match level.to_lowercase().as_str() {
        "error" => (LevelFilter::Error, "error"),
        "warn" => (LevelFilter::Warn, "warn"),
        "info" => (LevelFilter::Info, "info"),
        "debug" => (LevelFilter::Debug, "debug"),
        "trace" => (LevelFilter::Trace, "trace"),
        _ => return None,
    };

    if let Some(logger) = LOGGER.get() {
        if let Ok(mut lvl) = logger.level.lock() {
            *lvl = filter;
        }
    }
    log::set_max_level(filter);

    // Also set ESP-IDF native log level for C components
    let c_tag = std::ffi::CString::new("*").unwrap();
    let esp_level = match filter {
        LevelFilter::Off => esp_log_level_t_ESP_LOG_NONE,
        LevelFilter::Error => esp_log_level_t_ESP_LOG_ERROR,
        LevelFilter::Warn => esp_log_level_t_ESP_LOG_WARN,
        LevelFilter::Info => esp_log_level_t_ESP_LOG_INFO,
        LevelFilter::Debug => esp_log_level_t_ESP_LOG_DEBUG,
        LevelFilter::Trace => esp_log_level_t_ESP_LOG_VERBOSE,
    };
    unsafe { esp_log_level_set(c_tag.as_ptr(), esp_level) };

    Some(name)
}

/// Current log level name.
pub fn current_log_level() -> &'static str {
    LOGGER
        .get()
        .and_then(|l| l.level.lock().ok().map(|lvl| *lvl))
        .map(|f| match f {
            LevelFilter::Off => "off",
            LevelFilter::Error => "error",
            LevelFilter::Warn => "warn",
            LevelFilter::Info => "info",
            LevelFilter::Debug => "debug",
            LevelFilter::Trace => "trace",
        })
        .unwrap_or("info")
}

// ---------------------------------------------------------------------------
// System info
// ---------------------------------------------------------------------------

/// Collect and log system diagnostics (called periodically from main loop).
pub fn tick() {
    let heap_free = unsafe { esp_get_free_heap_size() };
    let heap_min = unsafe { esp_get_minimum_free_heap_size() };
    let uptime_ms = unsafe { esp_timer_get_time() / 1000 };
    let uptime_s = uptime_ms / 1000;

    log::info!(
        "heap: {heap_free} free ({heap_min} min), uptime: {}m {}s",
        uptime_s / 60,
        uptime_s % 60
    );
}

/// Full system info for GET /debug/info.
#[derive(serde::Serialize)]
pub struct SystemInfo {
    pub firmware: String,
    pub uptime_secs: u64,
    pub heap_free: u32,
    pub heap_min: u32,
    pub wifi: WifiInfo,
    pub usb: UsbInfo,
    pub log_level: String,
    pub log_buffer_used: usize,
    pub log_buffer_size: usize,
}

#[derive(serde::Serialize)]
pub struct WifiInfo {
    pub connected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ssid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rssi: Option<i8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<u8>,
    #[serde(rename = "lastWifiDisconnectReason")]
    pub last_wifi_disconnect_reason: u32,
}

#[derive(serde::Serialize)]
pub struct UsbInfo {
    pub connected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hw_id: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serial: Option<u32>,
}

impl SystemInfo {
    /// Collect system info by reading ESP-IDF state directly.
    pub fn collect(device: Option<&crate::device::DeviceState>) -> Self {
        let heap_free = unsafe { esp_get_free_heap_size() };
        let heap_min = unsafe { esp_get_minimum_free_heap_size() };
        let uptime_secs = unsafe { (esp_timer_get_time() / 1_000_000) as u64 };

        let wifi = collect_wifi_info();

        let usb = match device {
            Some(dev) => UsbInfo {
                connected: true,
                device: Some(dev.product_name().to_string()),
                hw_id: Some(dev.hw_id().hw_id),
                serial: Some(dev.serial()),
            },
            None => UsbInfo {
                connected: false,
                device: None,
                hw_id: None,
                serial: None,
            },
        };

        let (log_buffer_used, log_buffer_size) = log_buffer_stats();

        SystemInfo {
            firmware: env!("CARGO_PKG_VERSION").to_string(),
            uptime_secs,
            heap_free,
            heap_min,
            wifi,
            usb,
            log_level: current_log_level().to_string(),
            log_buffer_used,
            log_buffer_size,
        }
    }
}

/// Read WiFi connection info from ESP-IDF directly (no wifi handle needed).
fn collect_wifi_info() -> WifiInfo {
    unsafe {
        let mut ap_info: wifi_ap_record_t = core::mem::zeroed();
        let err = esp_wifi_sta_get_ap_info(&mut ap_info);

        if err != ESP_OK as i32 {
            return WifiInfo {
                connected: false,
                ssid: None,
                rssi: None,
                ip: None,
                channel: None,
                last_wifi_disconnect_reason: crate::wifi::last_disconnect_reason(),
            };
        }

        // SSID from ap_info (null-terminated bytes)
        let ssid_bytes = &ap_info.ssid;
        let ssid_len = ssid_bytes.iter().position(|&b| b == 0).unwrap_or(ssid_bytes.len());
        let ssid = String::from_utf8_lossy(&ssid_bytes[..ssid_len]).into_owned();

        // IP from STA netif
        let ip = get_sta_ip();

        WifiInfo {
            connected: true,
            ssid: Some(ssid),
            rssi: Some(ap_info.rssi),
            ip,
            channel: Some(ap_info.primary),
            last_wifi_disconnect_reason: crate::wifi::last_disconnect_reason(),
        }
    }
}

/// Read the STA interface IP address.
fn get_sta_ip() -> Option<String> {
    unsafe {
        let netif = esp_netif_get_handle_from_ifkey(b"WIFI_STA_DEF\0".as_ptr() as _);
        if netif.is_null() {
            return None;
        }
        let mut ip_info: esp_netif_ip_info_t = core::mem::zeroed();
        if esp_netif_get_ip_info(netif, &mut ip_info) != ESP_OK as i32 {
            return None;
        }
        let ip = ip_info.ip.addr;
        if ip == 0 {
            return None;
        }
        Some(format!(
            "{}.{}.{}.{}",
            ip & 0xFF,
            (ip >> 8) & 0xFF,
            (ip >> 16) & 0xFF,
            (ip >> 24) & 0xFF,
        ))
    }
}
