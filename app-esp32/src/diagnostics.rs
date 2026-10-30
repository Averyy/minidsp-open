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
            esp_log_write(esp_level, c_tag.as_ptr(), c"%s\n".as_ptr(), c_msg.as_ptr());
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
    let used = ring_buffer().lock().map(|buf| buf.used()).unwrap_or(0);
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

/// Internal RAM, as opposed to the PSRAM that dominates the total heap. Task
/// stacks, WiFi buffers, lwIP and USB DMA all need it, so it runs out first.
#[derive(serde::Serialize)]
pub struct InternalHeap {
    pub free: usize,
    pub min: usize,
    pub largest_block: usize,
}

pub fn internal_heap() -> InternalHeap {
    unsafe {
        InternalHeap {
            free: heap_caps_get_free_size(MALLOC_CAP_INTERNAL),
            min: heap_caps_get_minimum_free_size(MALLOC_CAP_INTERNAL),
            largest_block: heap_caps_get_largest_free_block(MALLOC_CAP_INTERNAL),
        }
    }
}

/// ESP-IDF heap tracing in leak mode, compiled in only when the build adds
/// `sdkconfig.heaptrace.defaults`. `POST /debug/heaptrace/start`, exercise
/// the board, then `POST /debug/heaptrace/stop` prints every internal-RAM
/// allocation still held, with its call stack, to the console.
#[cfg(esp_idf_heap_tracing_standalone)]
pub mod heap_trace {
    use esp_idf_svc::sys::{
        esp_err_t, heap_caps_malloc, ESP_OK, MALLOC_CAP_8BIT, MALLOC_CAP_SPIRAM,
    };
    use std::sync::OnceLock;

    const STACK_DEPTH: usize = esp_idf_svc::sys::CONFIG_HEAP_TRACING_STACK_DEPTH as usize;

    /// `heap_trace_record_t` from esp_heap_trace.h.
    #[repr(C)]
    struct Record {
        ccount: u32,
        address: *mut core::ffi::c_void,
        size: usize,
        freed: bool,
        alloced_by: [*mut core::ffi::c_void; STACK_DEPTH],
        freed_by: [*mut core::ffi::c_void; STACK_DEPTH],
        // tailq_list, plus slist_hashmap with CONFIG_HEAP_TRACE_HASH_MAP;
        // heap_trace_get copies the real size, which is never larger.
        _links: [usize; 3],
    }

    extern "C" {
        fn heap_trace_init_standalone(
            record_buffer: *mut core::ffi::c_void,
            num_records: usize,
        ) -> esp_err_t;
        fn heap_trace_start(mode: u32) -> esp_err_t;
        fn heap_trace_stop() -> esp_err_t;
        fn heap_trace_get_count() -> usize;
        fn heap_trace_get(index: usize, record: *mut Record) -> esp_err_t;
    }

    const HEAP_TRACE_LEAKS: u32 = 1;
    const RECORDS: usize = 400;
    // Bigger than heap_trace_record_t up to CONFIG_HEAP_TRACING_STACK_DEPTH 13.
    const RECORD_BYTES: usize = 128;

    static INIT: OnceLock<esp_err_t> = OnceLock::new();

    fn check(call: &str, err: esp_err_t) -> Result<(), String> {
        if err == ESP_OK as esp_err_t {
            Ok(())
        } else {
            Err(format!("{call} failed: {err}"))
        }
    }

    pub fn start() -> Result<(), String> {
        // The records live in PSRAM: an internal buffer this size would itself
        // starve the internal heap being measured.
        let init = *INIT.get_or_init(|| unsafe {
            let buffer =
                heap_caps_malloc(RECORDS * RECORD_BYTES, MALLOC_CAP_SPIRAM | MALLOC_CAP_8BIT);
            if buffer.is_null() {
                return -1;
            }
            heap_trace_init_standalone(buffer, RECORDS)
        });
        check("heap_trace_init_standalone", init)?;
        check("heap_trace_start", unsafe {
            heap_trace_start(HEAP_TRACE_LEAKS)
        })
    }

    /// Stop tracing and log each internal allocation still held. IDF's own
    /// heap_trace_dump prints every record inside a critical section, which
    /// at 115200 baud outlasts the interrupt watchdog; heap_trace_get holds
    /// the lock for one copy only.
    pub fn stop_and_dump() -> Result<(), String> {
        check("heap_trace_stop", unsafe { heap_trace_stop() })?;
        // SOC_DRAM_LOW..SOC_DRAM_HIGH on the ESP32-S3.
        let internal = 0x3FC8_8000..0x3FD0_0000usize;
        let (mut held, mut bytes) = (0usize, 0usize);
        for index in 0..unsafe { heap_trace_get_count() } {
            let mut record = core::mem::MaybeUninit::<Record>::zeroed();
            check("heap_trace_get", unsafe {
                heap_trace_get(index, record.as_mut_ptr())
            })?;
            let record = unsafe { record.assume_init() };
            if record.freed || !internal.contains(&(record.address as usize)) {
                continue;
            }
            held += 1;
            bytes += record.size;
            let callers = record
                .alloced_by
                .iter()
                .take_while(|pc| !pc.is_null())
                .map(|pc| format!("{pc:p}"))
                .collect::<Vec<_>>()
                .join(" ");
            log::info!(
                "heap trace: {} bytes @ {:p} caller {callers}",
                record.size,
                record.address
            );
        }
        log::info!("heap trace: {held} internal allocations still held, {bytes} bytes");
        Ok(())
    }
}

/// A FreeRTOS task's unused stack (its high-water mark), in bytes.
#[derive(serde::Serialize)]
pub struct TaskStack {
    pub task_number: u32,
    pub name: String,
    pub free_bytes: u32,
}

static BOOT_TASK_NAMES: OnceLock<std::collections::BTreeMap<u32, String>> = OnceLock::new();

fn task_snapshot() -> Vec<TaskStatus_t> {
    unsafe {
        let capacity = uxTaskGetNumberOfTasks() as usize + 4;
        let mut tasks: Vec<TaskStatus_t> = Vec::with_capacity(capacity);
        let filled = uxTaskGetSystemState(
            tasks.as_mut_ptr(),
            capacity as UBaseType_t,
            core::ptr::null_mut(),
        );
        tasks.set_len(filled as usize);
        tasks
    }
}

/// Called once at ready, before any transient tasks can exit.
pub fn capture_boot_task_names() {
    BOOT_TASK_NAMES.get_or_init(|| {
        task_snapshot()
            .iter()
            .map(|task| {
                // pcTaskName points into a TCB freed when its task exits. Only read it
                // at boot's stable task set; runtime snapshots use copied task numbers.
                let name = unsafe {
                    core::slice::from_raw_parts(
                        task.pcTaskName.cast::<u8>(),
                        CONFIG_FREERTOS_MAX_TASK_NAME_LEN as usize,
                    )
                };
                let len = name.iter().position(|&b| b == 0).unwrap_or(name.len());
                (
                    task.xTaskNumber,
                    String::from_utf8_lossy(&name[..len]).into_owned(),
                )
            })
            .collect()
    });
}

pub fn task_stacks() -> Vec<TaskStack> {
    let names = BOOT_TASK_NAMES.get();
    let mut stacks: Vec<TaskStack> = task_snapshot()
        .iter()
        .map(|task| TaskStack {
            task_number: task.xTaskNumber,
            name: names
                .and_then(|names| names.get(&task.xTaskNumber))
                .cloned()
                .unwrap_or_else(|| format!("task-{}", task.xTaskNumber)),
            free_bytes: task.usStackHighWaterMark,
        })
        .collect();
    stacks.sort_by_key(|task| task.task_number);
    stacks
}

/// Log every task's unused stack, to size the stacks from real numbers.
pub fn log_task_stacks() {
    let stacks = task_stacks()
        .iter()
        .map(|task| format!("{}={}", task.name, task.free_bytes))
        .collect::<Vec<_>>()
        .join(" ");
    log::info!("task stack free bytes: {stacks}");
}

/// Restart the minimum-free tracking from now, so `internal_heap().min` shows
/// the low point of whatever runs next instead of the low point since boot.
pub fn restart_heap_minimum() -> bool {
    unsafe {
        // Fails harmlessly when no window is open yet.
        heap_caps_monitor_local_minimum_free_size_stop();
        heap_caps_monitor_local_minimum_free_size_start() == ESP_OK as esp_err_t
    }
}

/// Debug-level internal RAM checkpoint, for narrowing down a drain.
pub fn trace_internal_heap(stage: &str) {
    if log::log_enabled!(log::Level::Debug) {
        let internal = internal_heap();
        log::debug!(
            "internal heap {stage}: {} free ({} min), {} largest",
            internal.free,
            internal.min,
            internal.largest_block
        );
    }
}

/// Log internal RAM at a startup step, so a step that eats it shows up.
pub fn log_internal_heap(stage: &str) {
    let internal = internal_heap();
    log::info!(
        "internal heap {stage}: {} free, {} largest",
        internal.free,
        internal.largest_block
    );
}

/// Collect and log system diagnostics (called periodically from main loop).
pub fn tick() {
    let heap_free = unsafe { esp_get_free_heap_size() };
    let heap_min = unsafe { esp_get_minimum_free_heap_size() };
    let internal = internal_heap();
    let uptime_ms = unsafe { esp_timer_get_time() / 1000 };
    let uptime_s = uptime_ms / 1000;

    log::info!(
        "heap: {heap_free} free ({heap_min} min), internal: {} free ({} min, {} largest), uptime: {}m {}s",
        internal.free,
        internal.min,
        internal.largest_block,
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
    pub internal_heap: InternalHeap,
    pub task_stacks: Vec<TaskStack>,
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
            internal_heap: internal_heap(),
            task_stacks: task_stacks(),
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

        if err != ESP_OK {
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
        let ssid_len = ssid_bytes
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(ssid_bytes.len());
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
        let netif = esp_netif_get_handle_from_ifkey(c"WIFI_STA_DEF".as_ptr());
        if netif.is_null() {
            return None;
        }
        let mut ip_info: esp_netif_ip_info_t = core::mem::zeroed();
        if esp_netif_get_ip_info(netif, &mut ip_info) != ESP_OK {
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
