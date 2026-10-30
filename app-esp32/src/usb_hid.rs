//! USB Host HID transport for ESP32-S3.
//!
//! Uses esp_idf_sys FFI to the ESP-IDF USB Host library (raw API, not
//! the managed HID component). Manages device enumeration, HID interface
//! claiming, and 64-byte interrupt transfers.
//!
//! Architecture:
//! - Background std::thread runs usb_host_lib_handle_events() loop
//! - UsbHidBus::command() does blocking OUT+IN transfers with Condvar sync
//! - Internal Mutex serializes all USB operations
//!
//! Hotplug:
//! - client_event_callback sets DEVICE_CONNECTED flag on NEW_DEV / DEV_GONE
//! - command() returns Disconnected if device is gone
//! - reconnect() waits for a new device and re-initializes handles
//!
//! Hardware notes:
//! - DevKitC-1 does NOT supply 5V on the OTG port. The connected device
//!   must be self-powered (e.g. simulator via UART) or use a powered hub.
//! - interface_claim() does NOT send SET_INTERFACE — fine for HID (no alt settings).
//! - interface_release() can fail while URBs are in-flight — cleanup drains and retries.

use esp_idf_sys::*;
use log::{debug, error, info, warn};
use minidsp_protocol::device::probe::is_official_usb_device;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU16, AtomicU32, AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;
/// USB HID class code.
const USB_CLASS_HID: u8 = 0x03;
/// USB descriptor types.
const DESC_TYPE_INTERFACE: u8 = 4;
const DESC_TYPE_ENDPOINT: u8 = 5;
/// Endpoint direction mask.
const EP_DIR_IN: u8 = 0x80;
/// USB report size on the wire (no report ID for report ID 0).
const USB_REPORT_SIZE: usize = 64;
/// Transfer timeout.
const XFER_TIMEOUT: Duration = Duration::from_secs(5);
const USB_EVENT_THREAD_STACK_BYTES: usize = 8 * 1024;
const ROOT_PORT_POWER_OFF_SETTLE: Duration = Duration::from_millis(250);
const ROOT_PORT_POWER_ON_SETTLE: Duration = Duration::from_millis(250);

#[derive(Debug)]
pub enum UsbHidError {
    Init(String),
    DeviceNotFound,
    Transfer(String),
    Timeout,
    Disconnected,
    Protocol(String),
}

impl std::fmt::Display for UsbHidError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Init(s) => write!(f, "USB init: {s}"),
            Self::DeviceNotFound => write!(f, "no MiniDSP device found"),
            Self::Transfer(s) => write!(f, "USB transfer: {s}"),
            Self::Timeout => write!(f, "USB timeout"),
            Self::Disconnected => write!(f, "USB disconnected"),
            Self::Protocol(s) => write!(f, "protocol: {s}"),
        }
    }
}

/// Transfer completion synchronization (shared between callback and waiter).
struct XferSync {
    done: AtomicBool,
    status: AtomicI32,
    session_generation: AtomicU8,
    mutex: Mutex<()>,
    condvar: Condvar,
}

impl XferSync {
    fn new() -> Self {
        Self {
            done: AtomicBool::new(false),
            status: AtomicI32::new(-1),
            session_generation: AtomicU8::new(0),
            mutex: Mutex::new(()),
            condvar: Condvar::new(),
        }
    }

    fn reset(&self) {
        self.done.store(false, Ordering::SeqCst);
        self.status.store(-1, Ordering::SeqCst);
    }

    fn prepare_for_submit(&self, session_generation: u8) {
        self.reset();
        self.session_generation
            .store(session_generation, Ordering::SeqCst);
    }

    fn session_generation(&self) -> u8 {
        self.session_generation.load(Ordering::SeqCst)
    }

    fn signal(&self, status: i32) {
        self.status.store(status, Ordering::SeqCst);
        self.done.store(true, Ordering::SeqCst);
        let _lock = self.mutex.lock().unwrap_or_else(|e| e.into_inner());
        self.condvar.notify_one();
    }

    fn wait(&self, timeout: Duration) -> Result<i32, UsbHidError> {
        if crate::auth::on_main_task() {
            let deadline = std::time::Instant::now() + timeout;
            loop {
                crate::auth::maintenance();
                let guard = self.mutex.lock().unwrap_or_else(|e| e.into_inner());
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                let result = self
                    .condvar
                    .wait_timeout_while(guard, remaining.min(Duration::from_millis(100)), |_| {
                        !self.done.load(Ordering::SeqCst)
                    })
                    .map_err(|_| UsbHidError::Transfer("condvar poisoned".into()))?;
                if self.done.load(Ordering::SeqCst) {
                    return Ok(self.status.load(Ordering::SeqCst));
                }
                drop(result);
                if std::time::Instant::now() >= deadline {
                    return Err(UsbHidError::Timeout);
                }
            }
        }
        let guard = self.mutex.lock().unwrap_or_else(|e| e.into_inner());
        let result = self
            .condvar
            .wait_timeout_while(guard, timeout, |_| !self.done.load(Ordering::SeqCst))
            .map_err(|_| UsbHidError::Transfer("condvar poisoned".into()))?;

        if result.1.timed_out() {
            return Err(UsbHidError::Timeout);
        }
        Ok(self.status.load(Ordering::SeqCst))
    }
}

/// Global callback state for USB host events (C callbacks can't capture closures).
struct CallbackState {
    new_dev_addr: Mutex<Option<u8>>,
    dev_event: (Mutex<bool>, Condvar),
    /// Set to true when device is connected, false on DEV_GONE.
    device_present: AtomicBool,
    /// esp_timer_get_time()/1000 when the latest attach was observed.
    last_attach_ms: AtomicU32,
    /// Incremented for each NEW_DEV so reconnect logic can notice a fresh attach
    /// even if another thread already consumed or cleared the queued address.
    attach_generation: AtomicU32,
    connection_epoch: AtomicU32,
    /// Snapshot of attach_generation at the most recent DEV_GONE. Cleanup uses
    /// this to avoid wiping a newer attach that raced with teardown.
    last_disconnect_attach_generation: AtomicU32,
    /// Incremented whenever the HID bus installs a fresh logical session.
    session_generation: AtomicU8,
    /// Transfer completion diagnostics for distinguishing dead callbacks from stalled endpoints.
    out_submit_count: AtomicU32,
    in_submit_count: AtomicU32,
    out_completion_count: AtomicU32,
    in_completion_count: AtomicU32,
    last_xfer_ep: AtomicU8,
    last_xfer_status: AtomicI32,
    last_xfer_ms: AtomicU32,
}

static CB_STATE: OnceLock<CallbackState> = OnceLock::new();
static GLOBAL_BUS: OnceLock<Arc<UsbHidBus>> = OnceLock::new();
/// esp_timer_get_time()/1000 when an endpoint halt started; 0 when none is in
/// progress. The host library waits for the halt with no timeout.
static HALT_STARTED_MS: AtomicU32 = AtomicU32::new(0);

/// When the endpoint halt in progress started (ms since boot), if one is.
pub fn halt_started_ms() -> Option<u32> {
    match HALT_STARTED_MS.load(Ordering::SeqCst) {
        0 => None,
        started => Some(started),
    }
}

fn cb_state() -> &'static CallbackState {
    CB_STATE.get_or_init(|| CallbackState {
        new_dev_addr: Mutex::new(None),
        dev_event: (Mutex::new(false), Condvar::new()),
        device_present: AtomicBool::new(false),
        last_attach_ms: AtomicU32::new(0),
        attach_generation: AtomicU32::new(0),
        connection_epoch: AtomicU32::new(0),
        last_disconnect_attach_generation: AtomicU32::new(0),
        session_generation: AtomicU8::new(0),
        out_submit_count: AtomicU32::new(0),
        in_submit_count: AtomicU32::new(0),
        out_completion_count: AtomicU32::new(0),
        in_completion_count: AtomicU32::new(0),
        last_xfer_ep: AtomicU8::new(0),
        last_xfer_status: AtomicI32::new(-1),
        last_xfer_ms: AtomicU32::new(0),
    })
}

#[derive(Clone, Copy, Debug)]
struct TransferStats {
    out_completion_count: u32,
    in_completion_count: u32,
    last_xfer_ep: u8,
    last_xfer_status: i32,
    last_xfer_age_ms: Option<u32>,
}

fn transfer_stats_snapshot() -> TransferStats {
    let cb = cb_state();
    let now_ms = (unsafe { esp_timer_get_time() } / 1000) as u32;
    let last_xfer_ms = cb.last_xfer_ms.load(Ordering::SeqCst);
    let last_xfer_age_ms = if last_xfer_ms == 0 || now_ms < last_xfer_ms {
        None
    } else {
        Some(now_ms - last_xfer_ms)
    };

    TransferStats {
        out_completion_count: cb.out_completion_count.load(Ordering::SeqCst),
        in_completion_count: cb.in_completion_count.load(Ordering::SeqCst),
        last_xfer_ep: cb.last_xfer_ep.load(Ordering::SeqCst),
        last_xfer_status: cb.last_xfer_status.load(Ordering::SeqCst),
        last_xfer_age_ms,
    }
}

/// USB HID bus — thread-safe handle for communicating with a MiniDSP device.
pub struct UsbHidBus {
    client_hdl: usb_host_client_handle_t,
    dev_hdl: Mutex<usb_device_handle_t>,
    out_xfer: Mutex<*mut usb_transfer_t>,
    in_xfer: Mutex<*mut usb_transfer_t>,
    ep_in: AtomicU8,
    ep_out: AtomicU8,
    hid_intf: Mutex<Option<u8>>,
    session_ready: AtomicBool,
    installed_epoch: AtomicU32,
    lock: Mutex<()>,
    out_sync: XferSync,
    in_sync: XferSync,
    vid: AtomicU16,
    pid: AtomicU16,
}

// Safety: all USB operations are serialized by self.lock. Raw pointers are only
// accessed while holding the lock, from the same thread that submitted the transfer.
unsafe impl Send for UsbHidBus {}
unsafe impl Sync for UsbHidBus {}

impl UsbHidBus {
    fn preserve_session_on_response_timeout(payload: &[u8]) -> bool {
        matches!(
            payload.first().copied(),
            Some(minidsp_protocol::commands::CMD_SET_SOURCE)
        )
    }

    fn begin_new_session(&self) -> u8 {
        let cb = cb_state();
        let generation = cb
            .session_generation
            .fetch_add(1, Ordering::SeqCst)
            .wrapping_add(1);
        cb.out_submit_count.store(0, Ordering::SeqCst);
        cb.in_submit_count.store(0, Ordering::SeqCst);
        cb.out_completion_count.store(0, Ordering::SeqCst);
        cb.in_completion_count.store(0, Ordering::SeqCst);
        cb.last_xfer_ep.store(0, Ordering::SeqCst);
        cb.last_xfer_status.store(-1, Ordering::SeqCst);
        cb.last_xfer_ms.store(0, Ordering::SeqCst);
        generation
    }

    fn current_session_generation(&self) -> u8 {
        cb_state().session_generation.load(Ordering::SeqCst)
    }

    fn submit_count_for_ep(&self, ep: u8) -> u32 {
        let cb = cb_state();
        if ep & EP_DIR_IN != 0 {
            cb.in_submit_count.load(Ordering::SeqCst)
        } else {
            cb.out_submit_count.load(Ordering::SeqCst)
        }
    }

    fn completion_count_for_ep(&self, ep: u8) -> u32 {
        let cb = cb_state();
        if ep & EP_DIR_IN != 0 {
            cb.in_completion_count.load(Ordering::SeqCst)
        } else {
            cb.out_completion_count.load(Ordering::SeqCst)
        }
    }

    fn bump_submit_count_for_ep(&self, ep: u8) -> u32 {
        let cb = cb_state();
        if ep & EP_DIR_IN != 0 {
            cb.in_submit_count.fetch_add(1, Ordering::SeqCst) + 1
        } else {
            cb.out_submit_count.fetch_add(1, Ordering::SeqCst) + 1
        }
    }

    fn endpoint_has_inflight_transfer(&self, ep: u8) -> bool {
        self.submit_count_for_ep(ep) > self.completion_count_for_ep(ep)
    }

    fn wait_for_endpoint_retirement(
        &self,
        ep: u8,
        target_completion: u32,
        timeout: Duration,
    ) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if self.completion_count_for_ep(ep) >= target_completion {
                // Let usb_host_client_handle_events() finish updating num_urb_inflight
                // before interface_release() inspects endpoint state.
                std::thread::sleep(Duration::from_millis(20));
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_for_endpoint_quiesce(&self, ep: u8, timeout: Duration, reason: &str) -> bool {
        if ep == 0 {
            return true;
        }
        let submitted = self.submit_count_for_ep(ep);
        let completed = self.completion_count_for_ep(ep);
        if submitted <= completed {
            return true;
        }

        debug!(
            "USB endpoint 0x{ep:02X}: waiting for callbacks to retire after {reason} (submitted={submitted}, completed={completed})"
        );
        self.wait_for_endpoint_retirement(ep, submitted, timeout)
    }

    fn wait_for_inflight_endpoints_to_quiesce(&self, timeout: Duration, reason: &str) -> bool {
        let ep_out = self.ep_out.load(Ordering::SeqCst);
        let ep_in = self.ep_in.load(Ordering::SeqCst);

        unsafe {
            let unblock = usb_host_client_unblock(self.client_hdl);
            if unblock != ESP_OK {
                warn!("USB client unblock failed during {reason}: 0x{unblock:04x}");
            }
        }

        let out_quiesced = self.wait_for_endpoint_quiesce(ep_out, timeout, reason);
        let in_quiesced = self.wait_for_endpoint_quiesce(ep_in, timeout, reason);
        if !out_quiesced || !in_quiesced {
            warn!(
                "USB endpoints did not quiesce after {reason} (ep_out submitted={}, completed={}; ep_in submitted={}, completed={})",
                self.submit_count_for_ep(ep_out),
                self.completion_count_for_ep(ep_out),
                self.submit_count_for_ep(ep_in),
                self.completion_count_for_ep(ep_in),
            );
        }
        out_quiesced && in_quiesced
    }

    fn retire_endpoint_if_needed(
        &self,
        dev_hdl: usb_device_handle_t,
        ep: u8,
        reason: &str,
    ) -> bool {
        const RETIRE_TIMEOUT: Duration = Duration::from_secs(2);

        if dev_hdl.is_null() || ep == 0 {
            return true;
        }

        let had_inflight = self.endpoint_has_inflight_transfer(ep);
        let target_completion = self.submit_count_for_ep(ep);
        if !had_inflight {
            return true;
        }

        debug!(
            "USB endpoint 0x{ep:02X}: retiring inflight URB for {reason} (submitted={target_completion}, completed={})",
            self.completion_count_for_ep(ep)
        );

        unsafe {
            HALT_STARTED_MS.store(
                ((esp_timer_get_time() / 1000) as u32).max(1),
                Ordering::SeqCst,
            );
            let halt = usb_host_endpoint_halt(dev_hdl, ep);
            HALT_STARTED_MS.store(0, Ordering::SeqCst);
            if halt != ESP_OK && halt != ESP_ERR_INVALID_STATE && halt != ESP_ERR_NOT_FOUND {
                warn!("USB endpoint 0x{ep:02X} halt failed during {reason}: 0x{halt:04x}");
            }
            let flush = usb_host_endpoint_flush(dev_hdl, ep);
            if flush != ESP_OK && flush != ESP_ERR_INVALID_STATE && flush != ESP_ERR_NOT_FOUND {
                warn!("USB endpoint 0x{ep:02X} flush failed during {reason}: 0x{flush:04x}");
            }
            let unblock = usb_host_client_unblock(self.client_hdl);
            if unblock != ESP_OK {
                warn!("USB client unblock failed during {reason}: 0x{unblock:04x}");
            }
        }

        if !self.wait_for_endpoint_retirement(ep, target_completion, RETIRE_TIMEOUT) {
            warn!(
                "USB endpoint 0x{ep:02X} did not retire after {reason} (submitted={}, completed={})",
                self.submit_count_for_ep(ep),
                self.completion_count_for_ep(ep)
            );
            return false;
        }
        unsafe {
            let clear = usb_host_endpoint_clear(dev_hdl, ep);
            if clear != ESP_OK {
                warn!("USB endpoint 0x{ep:02X} clear failed during {reason}: 0x{clear:04x}");
            }
            clear == ESP_OK
        }
    }

    fn retire_inflight_endpoints(&self, dev_hdl: usb_device_handle_t, reason: &str) -> bool {
        let ep_out = self.ep_out.load(Ordering::SeqCst);
        let ep_in = self.ep_in.load(Ordering::SeqCst);
        let out_retired = self.retire_endpoint_if_needed(dev_hdl, ep_out, reason);
        let in_retired = self.retire_endpoint_if_needed(dev_hdl, ep_in, reason);
        out_retired && in_retired
    }

    fn clear_transfers(&self) {
        let mut out = self.out_xfer.lock().unwrap();
        if !(*out).is_null() {
            unsafe { usb_host_transfer_free(*out) };
            *out = ptr::null_mut();
        }

        let mut inp = self.in_xfer.lock().unwrap();
        if !(*inp).is_null() {
            unsafe { usb_host_transfer_free(*inp) };
            *inp = ptr::null_mut();
        }
    }

    fn clear_session_state(&self) {
        *self.dev_hdl.lock().unwrap() = ptr::null_mut();
        self.clear_transfers();
        self.ep_in.store(0, Ordering::SeqCst);
        self.ep_out.store(0, Ordering::SeqCst);
        *self.hid_intf.lock().unwrap() = None;
        self.session_ready.store(false, Ordering::SeqCst);
        self.vid.store(0, Ordering::SeqCst);
        self.pid.store(0, Ordering::SeqCst);
    }

    fn install_open_device(
        &self,
        device: (usb_device_handle_t, u16, u16, u8, u8, u8),
    ) -> Result<(u16, u16), UsbHidError> {
        let (dev_hdl, vid, pid, hid_intf, ep_in, ep_out) = device;
        self.begin_new_session();
        let epoch = cb_state()
            .connection_epoch
            .fetch_add(1, Ordering::SeqCst)
            .wrapping_add(1);
        self.installed_epoch.store(epoch, Ordering::SeqCst);

        *self.dev_hdl.lock().unwrap() = dev_hdl;
        self.ep_in.store(ep_in, Ordering::SeqCst);
        self.ep_out.store(ep_out, Ordering::SeqCst);
        *self.hid_intf.lock().unwrap() = Some(hid_intf);
        let allocated = (|| {
            *self.out_xfer.lock().unwrap() = alloc_transfer(dev_hdl, ep_out)?;
            *self.in_xfer.lock().unwrap() = alloc_transfer(dev_hdl, ep_in)?;
            Ok::<(), UsbHidError>(())
        })();
        if let Err(err) = allocated {
            if let Err(cleanup_err) = self.cleanup_device() {
                warn!("USB cleanup after transfer allocation failed: {cleanup_err}");
            }
            return Err(err);
        }

        self.vid.store(vid, Ordering::SeqCst);
        self.pid.store(pid, Ordering::SeqCst);

        self.session_ready.store(true, Ordering::SeqCst);
        info!("USB HID bus ready");
        Ok((vid, pid))
    }

    fn submit_out_transfer(
        &self,
        out_xfer: *mut usb_transfer_t,
        out_buf: &[u8; USB_REPORT_SIZE],
        payload: &[u8],
        timeout: Duration,
    ) -> Result<(), UsbHidError> {
        let ep_out = unsafe { (*out_xfer).bEndpointAddress };
        self.out_sync
            .prepare_for_submit(self.current_session_generation());
        unsafe {
            ptr::copy_nonoverlapping(out_buf.as_ptr(), (*out_xfer).data_buffer, USB_REPORT_SIZE);
            (*out_xfer).num_bytes = USB_REPORT_SIZE as i32;
            (*out_xfer).context = &self.out_sync as *const XferSync as *mut core::ffi::c_void;
            esp_ok(usb_host_transfer_submit(out_xfer))
                .map_err(|e| UsbHidError::Transfer(format!("submit OUT: {e}")))?;
        }
        self.bump_submit_count_for_ep(ep_out);
        debug!("USB OUT submitted for payload: {:02x?}", payload);
        let status = self.out_sync.wait(timeout).map_err(|e| {
            let stats = transfer_stats_snapshot();
            warn!(
                "USB OUT wait failed (out_cb={}, in_cb={}, last_ep=0x{:02X}, last_status={}, last_age_ms={})",
                stats.out_completion_count,
                stats.in_completion_count,
                stats.last_xfer_ep,
                stats.last_xfer_status,
                stats
                    .last_xfer_age_ms
                    .map(|ms| ms.to_string())
                    .unwrap_or_else(|| "n/a".into())
            );
            debug!("USB OUT wait failed for {:02x?}: {}", payload, e);
            crate::usb_dwc::log_controller_state("OUT timeout");
            if let Err(cleanup_err) = self.cleanup_device() {
                warn!("USB cleanup after OUT timeout failed: {cleanup_err}");
            }
            e
        })?;
        if status != 0 {
            warn!("USB OUT completion status {status}");
            debug!("USB OUT completion status {status} for {:02x?}", payload);
            return Err(UsbHidError::Transfer(format!("OUT status: {status}")));
        }
        debug!("USB OUT completed for payload: {:02x?}", payload);
        Ok(())
    }

    /// Initialize USB host stack and background threads. Always succeeds even
    /// without a device connected. Call `wait_for_device()` to detect a device.
    pub fn init() -> Result<Self, UsbHidError> {
        // Ensure callback state is initialized
        cb_state();

        // Install USB host library
        unsafe {
            let config = usb_host_config_t {
                skip_phy_setup: false,
                intr_flags: ESP_INTR_FLAG_LEVEL1 as i32,
                ..core::mem::zeroed()
            };
            esp_ok(usb_host_install(&config))
                .map_err(|e| UsbHidError::Init(format!("usb_host_install: {e}")))?;
        }
        info!("USB host library installed");

        // Start background event task. Both USB event loops only pump the
        // host library and used ~2.4 KB of stack; internal RAM is scarce.
        std::thread::Builder::new()
            .name("usb_events".into())
            .stack_size(USB_EVENT_THREAD_STACK_BYTES)
            .spawn(usb_host_event_loop)
            .map_err(|e| UsbHidError::Init(format!("spawn event task: {e}")))?;

        // Register USB host client
        let client_hdl = unsafe {
            let mut hdl: usb_host_client_handle_t = ptr::null_mut();
            let config = usb_host_client_config_t {
                is_synchronous: false,
                max_num_event_msg: 5,
                // No opt-in DEV_REMOVED events: DEV_GONE (for devices this
                // client opened) is all the disconnect handling relies on.
                flags: Default::default(),
                __bindgen_anon_1: usb_host_client_config_t__bindgen_ty_2 {
                    async_: usb_host_client_config_t__bindgen_ty_2__bindgen_ty_1 {
                        client_event_callback: Some(client_event_callback),
                        callback_arg: ptr::null_mut(),
                    },
                },
            };

            esp_ok(usb_host_client_register(&config, &mut hdl))
                .map_err(|e| UsbHidError::Init(format!("client_register: {e}")))?;
            hdl
        };
        info!("USB host client registered");

        // Start client event handling thread
        let client_hdl_copy = client_hdl as usize;
        std::thread::Builder::new()
            .name("usb_client".into())
            .stack_size(USB_EVENT_THREAD_STACK_BYTES)
            .spawn(move || usb_client_event_loop(client_hdl_copy as usb_host_client_handle_t))
            .map_err(|e| UsbHidError::Init(format!("spawn client task: {e}")))?;

        // Give the client event loop time to process any already-connected devices.
        // ESP-IDF fires NEW_DEV for pre-connected devices only after
        // usb_host_client_handle_events() processes the pending event queue.
        std::thread::sleep(Duration::from_millis(200));

        // Check if any devices are already connected via the address list
        unsafe {
            let mut num_devs: i32 = 0;
            let mut dev_addrs = [0u8; 8];
            let err = usb_host_device_addr_list_fill(
                dev_addrs.len() as i32,
                dev_addrs.as_mut_ptr(),
                &mut num_devs,
            );
            if err == ESP_OK && num_devs > 0 {
                info!("USB: {num_devs} device(s) already connected at boot");
                let cb = cb_state();
                for &addr in dev_addrs.iter().take(num_devs as usize) {
                    if addr > 0 {
                        let (lock, cvar) = &cb.dev_event;
                        let mut guard = lock.lock().unwrap();
                        *cb.new_dev_addr.lock().unwrap() = Some(addr);
                        cb.connection_epoch.fetch_add(1, Ordering::SeqCst);
                        cb.device_present.store(true, Ordering::SeqCst);
                        cb.last_attach_ms
                            .store((esp_timer_get_time() / 1000) as u32, Ordering::SeqCst);
                        *guard = true;
                        cvar.notify_all();
                        info!("USB: pre-connected device at addr={addr}");
                        break;
                    }
                }
            }
        }

        Ok(UsbHidBus {
            client_hdl,
            dev_hdl: Mutex::new(ptr::null_mut()),
            out_xfer: Mutex::new(ptr::null_mut()),
            in_xfer: Mutex::new(ptr::null_mut()),
            ep_in: AtomicU8::new(0),
            ep_out: AtomicU8::new(0),
            hid_intf: Mutex::new(None),
            session_ready: AtomicBool::new(false),
            installed_epoch: AtomicU32::new(0),
            lock: Mutex::new(()),
            out_sync: XferSync::new(),
            in_sync: XferSync::new(),
            vid: AtomicU16::new(0),
            pid: AtomicU16::new(0),
        })
    }

    /// Wait for a MiniDSP device and open it. Blocks up to `timeout`.
    /// Returns the VID and PID of the found device.
    pub fn wait_for_device(&self, timeout: Duration) -> Result<(u16, u16), UsbHidError> {
        let _lock = self.lock.lock().unwrap();
        let attach_generation_at_start = cb_state().attach_generation.load(Ordering::SeqCst);

        // Close old device if any (required for USB host to detect reconnection)
        self.cleanup_device()?;

        let require_new_event = !cb_state().device_present.load(Ordering::SeqCst);
        if !require_new_event {
            if let Some(device) = try_open_present_device(self.client_hdl, "scan")? {
                return self.install_open_device(device);
            }
        } else {
            debug!("USB wait_for_device: requiring a fresh NEW_DEV event before reopening");
        }

        let device = wait_and_open_device(
            self.client_hdl,
            timeout,
            require_new_event,
            attach_generation_at_start,
        )?;
        self.install_open_device(device)
    }

    pub fn has_present_device(&self) -> Result<bool, UsbHidError> {
        Ok(!list_present_device_addrs()?.is_empty())
    }

    pub fn reopen_present_device(&self) -> Result<Option<(u16, u16)>, UsbHidError> {
        let _lock = self.lock.lock().unwrap();
        self.cleanup_device()?;
        match try_open_present_device(self.client_hdl, "reopen")? {
            Some(device) => self.install_open_device(device).map(Some),
            None => Ok(None),
        }
    }

    pub fn recycle_root_port(&self, reason: &str) -> Result<(), UsbHidError> {
        let _lock = self.lock.lock().unwrap();

        if let Err(err) = self.cleanup_device() {
            warn!("USB cleanup before root-port recycle failed ({reason}): {err}");
        }

        info!("USB: powering root port off ({reason})");
        unsafe {
            let err = usb_host_lib_set_root_port_power(false);
            if err != ESP_OK && err != ESP_ERR_INVALID_STATE {
                return Err(UsbHidError::Transfer(format!(
                    "root-port power off failed during {reason}: 0x{err:04x}"
                )));
            }
        }
        std::thread::sleep(ROOT_PORT_POWER_OFF_SETTLE);

        info!("USB: powering root port on ({reason})");
        unsafe {
            esp_ok(usb_host_lib_set_root_port_power(true)).map_err(|e| {
                UsbHidError::Transfer(format!("root-port power on failed during {reason}: {e}"))
            })?;
        }
        std::thread::sleep(ROOT_PORT_POWER_ON_SETTLE);
        Ok(())
    }

    /// True if the USB device is currently connected.
    pub fn is_connected(&self) -> bool {
        self.session_ready.load(Ordering::SeqCst)
            && self.installed_epoch.load(Ordering::SeqCst) == self.connection_epoch()
            && cb_state().device_present.load(Ordering::SeqCst)
            && !self.dev_hdl.lock().unwrap().is_null()
    }

    pub fn cleanup_after_expected_disconnect(&self, reason: &str) {
        let cb = cb_state();
        let disconnect_attach_generation =
            cb.last_disconnect_attach_generation.load(Ordering::SeqCst);
        let (lock, _) = &cb.dev_event;
        let mut guard = lock.lock().unwrap();
        if cb.attach_generation.load(Ordering::SeqCst) == disconnect_attach_generation {
            cb.device_present.store(false, Ordering::SeqCst);
            cb.last_attach_ms.store(0, Ordering::SeqCst);
            *cb.new_dev_addr.lock().unwrap() = None;
            *guard = false;
        } else {
            debug!(
                "USB cleanup after {reason}: preserving newer attach state (disconnect_attach_gen={}, current_attach_gen={})",
                disconnect_attach_generation,
                cb.attach_generation.load(Ordering::SeqCst)
            );
        }
        drop(guard);
        let _lock = self.lock.lock().unwrap();
        if let Err(err) = self.cleanup_device() {
            warn!("USB cleanup after expected disconnect ({reason}) failed: {err}");
        }
    }

    pub fn last_attach_age(&self) -> Option<Duration> {
        let attached_at = cb_state().last_attach_ms.load(Ordering::SeqCst);
        if attached_at == 0 {
            return None;
        }
        let now = (unsafe { esp_timer_get_time() } / 1000) as u32;
        if now < attached_at {
            return None;
        }
        Some(Duration::from_millis((now - attached_at) as u64))
    }

    /// USB Vendor ID of the connected device (0 if none).
    pub fn vid(&self) -> u16 {
        self.vid.load(Ordering::SeqCst)
    }

    /// USB Product ID of the connected device (0 if none).
    pub fn pid(&self) -> u16 {
        self.pid.load(Ordering::SeqCst)
    }

    /// Send a command payload and receive the response. Blocking, thread-safe.
    ///
    /// The payload is the raw command bytes (e.g., `[0x31]` for ReadHardwareId).
    /// This method handles frame encoding, USB transfer, and frame decoding.
    pub fn connection_epoch(&self) -> u32 {
        cb_state().connection_epoch.load(Ordering::SeqCst)
    }

    pub fn command_at_epoch(&self, payload: &[u8], epoch: u32) -> Result<Vec<u8>, UsbHidError> {
        self.command_with_timeout_at_epoch(payload, XFER_TIMEOUT, epoch)
    }

    pub fn command_with_timeout_at_epoch(
        &self,
        payload: &[u8],
        timeout: Duration,
        expected_epoch: u32,
    ) -> Result<Vec<u8>, UsbHidError> {
        let _lock = self.lock.lock().unwrap();
        if expected_epoch != self.connection_epoch() {
            return Err(UsbHidError::Disconnected);
        }
        let command_epoch = self.connection_epoch();
        debug!("USB command start: {:02x?}", payload);

        // Check connection under lock to avoid TOCTOU race
        if !self.is_connected() {
            warn!("USB command rejected while disconnected");
            debug!("USB command rejected while disconnected: {:02x?}", payload);
            return Err(UsbHidError::Disconnected);
        }

        let dev_hdl = *self.dev_hdl.lock().unwrap();
        let out_xfer = *self.out_xfer.lock().unwrap();
        let in_xfer = *self.in_xfer.lock().unwrap();
        if dev_hdl.is_null() || out_xfer.is_null() || in_xfer.is_null() {
            return Err(UsbHidError::Disconnected);
        }

        // Encode frame: [length, payload, checksum] padded to 64 bytes with 0xFF
        let frame = minidsp_protocol::packet::encode_frame(payload);
        let mut out_buf = [0xFF_u8; USB_REPORT_SIZE];
        let copy_len = frame.len().min(USB_REPORT_SIZE);
        out_buf[..copy_len].copy_from_slice(&frame[..copy_len]);

        self.submit_out_transfer(out_xfer, &out_buf, payload, timeout)?;

        // Receive loop: retry IN transfers on undecodable packets (stale data,
        // garbled frames) within the original timeout window, matching the
        // desktop HID transport behavior.
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                self.handle_response_timeout(payload);
                return Err(UsbHidError::Timeout);
            }

            // Submit IN transfer
            self.in_sync
                .prepare_for_submit(self.current_session_generation());
            let ep_in = unsafe { (*in_xfer).bEndpointAddress };
            unsafe {
                (*in_xfer).num_bytes = USB_REPORT_SIZE as i32;
                (*in_xfer).context = &self.in_sync as *const XferSync as *mut core::ffi::c_void;
                esp_ok(usb_host_transfer_submit(in_xfer))
                    .map_err(|e| UsbHidError::Transfer(format!("submit IN: {e}")))?;
            }
            self.bump_submit_count_for_ep(ep_in);
            debug!("USB IN submitted for payload: {:02x?}", payload);
            let status = self.in_sync.wait(remaining).map_err(|e| {
                let stats = transfer_stats_snapshot();
                warn!(
                    "USB IN wait failed (out_cb={}, in_cb={}, last_ep=0x{:02X}, last_status={}, last_age_ms={})",
                    stats.out_completion_count,
                    stats.in_completion_count,
                    stats.last_xfer_ep,
                    stats.last_xfer_status,
                    stats
                        .last_xfer_age_ms
                        .map(|ms| ms.to_string())
                        .unwrap_or_else(|| "n/a".into())
                );
                debug!("USB IN wait failed for {:02x?}: {}", payload, e);
                if matches!(e, UsbHidError::Timeout) {
                    self.handle_response_timeout(payload);
                } else if let Err(cleanup_err) = self.cleanup_device() {
                    warn!("USB cleanup after IN wait failed: {cleanup_err}");
                }
                e
            })?;
            if status != 0 {
                warn!("USB IN completion status {status}");
                debug!("USB IN completion status {status} for {:02x?}", payload);
                return Err(UsbHidError::Transfer(format!("IN status: {status}")));
            }
            debug!("USB IN completed for payload: {:02x?}", payload);

            // Read response from IN transfer buffer
            let actual_len = unsafe { (*in_xfer).actual_num_bytes as usize };
            let mut resp_buf = vec![0u8; actual_len.min(USB_REPORT_SIZE)];
            unsafe {
                ptr::copy_nonoverlapping(
                    (*in_xfer).data_buffer,
                    resp_buf.as_mut_ptr(),
                    resp_buf.len(),
                );
            }

            if resp_buf.len() >= 4 {
                debug!(
                    "USB IN: {} bytes, full packet: {:02x?}",
                    resp_buf.len(),
                    &resp_buf[..resp_buf.len().min(USB_REPORT_SIZE)]
                );
            }

            // Try to decode the HID response frame
            if let Some(decoded) = minidsp_protocol::packet::decode_hid_response(&resp_buf) {
                debug!("USB command decoded response: {:02x?}", decoded);

                if minidsp_protocol::commands::is_source_changed_notification_for_request(
                    payload, &decoded,
                ) {
                    debug!(
                        "USB HID: accepting AudioSrcChanged notification as set_source completion: {:02x?}",
                        decoded
                    );
                    if command_epoch != self.connection_epoch() || !self.is_connected() {
                        return Err(UsbHidError::Disconnected);
                    }
                    return Ok(decoded);
                }

                // Skip stale frames whose opcode doesn't match the request. The
                // 2x4 HD emits AudioSrcChanged / flash-notification frames that
                // can arrive before the real ack for set_source / set_config.
                // Mirrors the desktop queue's skip-stale loop in minidsp/src/queue.rs.
                if let (Some(req_op), Some(resp_op)) =
                    (payload.first().copied(), decoded.first().copied())
                {
                    if !minidsp_protocol::commands::matches_response(req_op, resp_op) {
                        debug!(
                            "USB HID: skipping stale response op=0x{resp_op:02x} for request op=0x{req_op:02x}, frame: {:02x?}",
                            decoded
                        );
                        continue;
                    }
                }
                if command_epoch != self.connection_epoch() || !self.is_connected() {
                    return Err(UsbHidError::Disconnected);
                }
                return Ok(decoded);
            }

            // Undecodable packet — log and retry within the timeout window
            let hex: String = resp_buf
                .iter()
                .take(16)
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(" ");
            warn!(
                "USB HID: undecodable response ({} bytes: {hex}), retrying",
                resp_buf.len()
            );
        }
    }

    fn handle_response_timeout(&self, payload: &[u8]) {
        let dev_hdl = *self.dev_hdl.lock().unwrap();
        if Self::preserve_session_on_response_timeout(payload)
            && self.retire_inflight_endpoints(dev_hdl, "response timeout")
        {
            warn!(
                "USB preserving session after response timeout for payload {:02x?}; caller will verify command outcome",
                payload
            );
        } else if let Err(err) = self.cleanup_device() {
            warn!("USB cleanup after response timeout failed: {err}");
        }
    }

    /// Best-effort cleanup of the current device (release interface, close).
    fn cleanup_device(&self) -> Result<(), UsbHidError> {
        const RELEASE_RETRY_DELAY: Duration = Duration::from_millis(20);
        const RELEASE_RETRIES: usize = 150;
        const CLOSE_SETTLE_DELAY: Duration = Duration::from_millis(10);
        const DEV_GONE_DRAIN_TIMEOUT: Duration = Duration::from_secs(3);

        self.session_ready.store(false, Ordering::SeqCst);
        let dev_hdl = *self.dev_hdl.lock().unwrap();
        if dev_hdl.is_null() {
            self.clear_session_state();
            return Ok(());
        }
        let hid_intf = *self.hid_intf.lock().unwrap();
        let device_present = cb_state().device_present.load(Ordering::SeqCst);
        let retired = if device_present {
            self.retire_inflight_endpoints(dev_hdl, "cleanup")
        } else {
            debug!("USB cleanup: waiting for endpoint callbacks after DEV_GONE");
            self.wait_for_inflight_endpoints_to_quiesce(
                DEV_GONE_DRAIN_TIMEOUT,
                "cleanup after DEV_GONE",
            )
        };
        if !retired {
            return Err(UsbHidError::Transfer(
                "cleanup transfers did not retire or endpoint clear failed".into(),
            ));
        }

        let mut release_err = ESP_OK;
        let mut result = Ok(());
        unsafe {
            if let Some(hid_intf) = hid_intf {
                for attempt in 0..RELEASE_RETRIES {
                    let err = usb_host_interface_release(self.client_hdl, dev_hdl, hid_intf);
                    if err == ESP_OK || err == ESP_ERR_NOT_FOUND {
                        release_err = ESP_OK;
                        *self.hid_intf.lock().unwrap() = None;
                        if attempt > 0 {
                            info!(
                                "USB interface_release({hid_intf}) succeeded after {} attempt(s)",
                                attempt + 1
                            );
                        }
                        break;
                    }
                    release_err = err;
                    if err == ESP_ERR_INVALID_STATE {
                        if device_present {
                            self.retire_inflight_endpoints(dev_hdl, "cleanup retry");
                        } else {
                            self.wait_for_inflight_endpoints_to_quiesce(
                                Duration::from_millis(100),
                                "cleanup retry after DEV_GONE",
                            );
                        }
                        let unblock = usb_host_client_unblock(self.client_hdl);
                        if unblock != ESP_OK {
                            warn!(
                                "USB client unblock failed during cleanup retry: 0x{unblock:04x}"
                            );
                        }
                    }
                    if attempt + 1 < RELEASE_RETRIES {
                        std::thread::sleep(RELEASE_RETRY_DELAY);
                    }
                }
            }

            if release_err != ESP_OK {
                warn!("USB interface_release({hid_intf:?}) failed: 0x{release_err:04x}");
                result = Err(UsbHidError::Transfer(format!(
                    "cleanup interface release failed: 0x{release_err:04x}"
                )));
            } else {
                let err = usb_host_device_close(self.client_hdl, dev_hdl);
                if err != ESP_OK {
                    warn!("USB device_close failed: 0x{err:04x}");
                    result = Err(UsbHidError::Transfer(format!(
                        "cleanup device close failed: 0x{err:04x}"
                    )));
                }
            }
        }
        // Keep failed cleanup state so the next reconnect can retry safely.
        if result.is_ok() {
            self.clear_session_state();
            std::thread::sleep(CLOSE_SETTLE_DELAY);
        }
        result
    }
}

pub fn register_global_bus(bus: Arc<UsbHidBus>) {
    let _ = GLOBAL_BUS.set(bus);
}

pub fn recycle_root_port_global(reason: &str) -> Result<(), UsbHidError> {
    GLOBAL_BUS
        .get()
        .ok_or_else(|| UsbHidError::Init("USB HID bus not initialized".into()))?
        .recycle_root_port(reason)
}

impl Drop for UsbHidBus {
    fn drop(&mut self) {
        let _ = self.cleanup_device();
    }
}

// -- Shared device open logic --

/// Wait for a MiniDSP USB device and open it. Returns handles and endpoint info.
fn wait_and_open_device(
    client_hdl: usb_host_client_handle_t,
    timeout: Duration,
    require_new_event: bool,
    attach_generation_at_start: u32,
) -> Result<(usb_device_handle_t, u16, u16, u8, u8, u8), UsbHidError> {
    const ADDR_SCAN_INTERVAL: Duration = Duration::from_millis(500);

    info!("waiting for official MiniDSP USB HID device...");

    let cb = cb_state();
    let deadline = std::time::Instant::now() + timeout;
    let mut saw_new_event = !require_new_event;

    // Loop: wait for device events, try to open each one, skip non-MiniDSP devices
    loop {
        crate::auth::maintenance();
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(UsbHidError::DeviceNotFound);
        }

        if !saw_new_event
            && cb.attach_generation.load(Ordering::SeqCst) != attach_generation_at_start
        {
            debug!("USB wait_for_device: attach generation advanced without a queued address");
            saw_new_event = true;
        }

        if saw_new_event {
            if let Some(device) = try_open_present_device(client_hdl, "wait")? {
                return Ok(device);
            }
        }

        // Wait for a device address under the dev_event lock (callback sets both
        // new_dev_addr and dev_event under this same lock, so no race possible).
        let dev_addr = {
            let (lock, cvar) = &cb.dev_event;
            let guard = lock.lock().unwrap();
            let wait_for = remaining.min(ADDR_SCAN_INTERVAL);
            // Use the condvar predicate to check for a queued address.
            // This handles both "already queued" and "need to wait" cases.
            let result = cvar
                .wait_timeout_while(guard, wait_for, |_| {
                    cb.new_dev_addr.lock().unwrap().is_none()
                })
                .unwrap();
            if result.1.timed_out() {
                continue;
            }
            match cb.new_dev_addr.lock().unwrap().take() {
                Some(addr) => addr,
                None => continue,
            }
        };
        saw_new_event = true;
        if let Some(device) =
            open_device_at_addr_when_settled(client_hdl, dev_addr, "new-dev event")?
        {
            return Ok(device);
        }
    } // end loop
}

type OpenedDevice = (usb_device_handle_t, u16, u16, u8, u8, u8);

fn try_open_present_device(
    client_hdl: usb_host_client_handle_t,
    reason: &str,
) -> Result<Option<OpenedDevice>, UsbHidError> {
    let addrs = list_present_device_addrs()?;
    if addrs.is_empty() {
        debug!("USB: no devices present in host addr list ({reason})");
        return Ok(None);
    }

    info!("USB: host addr list ({reason}) = {addrs:?}");
    for addr in addrs {
        if let Some(device) = open_device_at_addr_when_settled(client_hdl, addr, reason)? {
            return Ok(Some(device));
        }
    }
    Ok(None)
}

fn wait_for_attach_settle(reason: &str) {
    const OPEN_SETTLE: Duration = Duration::from_secs(20);

    let attached_at = cb_state().last_attach_ms.load(Ordering::SeqCst);
    if attached_at == 0 {
        return;
    }

    let now = (unsafe { esp_timer_get_time() } / 1000) as u32;
    if now < attached_at {
        return;
    }

    let age = Duration::from_millis((now - attached_at) as u64);
    if age >= OPEN_SETTLE {
        return;
    }

    let wait = OPEN_SETTLE - age;
    info!("USB: waiting {wait:?} before opening device ({reason})");
    crate::auth::responsive_sleep(wait);
}

fn open_device_at_addr_when_settled(
    client_hdl: usb_host_client_handle_t,
    dev_addr: u8,
    reason: &str,
) -> Result<Option<OpenedDevice>, UsbHidError> {
    wait_for_attach_settle(reason);
    open_device_at_addr(client_hdl, dev_addr)
}

fn list_present_device_addrs() -> Result<Vec<u8>, UsbHidError> {
    let mut num_devs: i32 = 0;
    let mut dev_addrs = [0u8; 8];
    unsafe {
        esp_ok(usb_host_device_addr_list_fill(
            dev_addrs.len() as i32,
            dev_addrs.as_mut_ptr(),
            &mut num_devs,
        ))
        .map_err(|e| UsbHidError::Init(format!("device_addr_list_fill: {e}")))?;
    }

    let mut addrs = Vec::new();
    for addr in dev_addrs.into_iter().take(num_devs.max(0) as usize) {
        if addr > 0 {
            addrs.push(addr);
        }
    }
    Ok(addrs)
}

fn open_device_at_addr(
    client_hdl: usb_host_client_handle_t,
    dev_addr: u8,
) -> Result<Option<OpenedDevice>, UsbHidError> {
    info!("USB: opening device at address {dev_addr}");

    let dev_hdl = unsafe {
        let mut hdl: usb_device_handle_t = ptr::null_mut();
        match esp_ok(usb_host_device_open(client_hdl, dev_addr, &mut hdl)) {
            Ok(()) => hdl,
            Err(e) => {
                warn!("USB: device_open({dev_addr}) failed: {e}");
                return Ok(None);
            }
        }
    };

    let (vid, pid) = unsafe {
        let mut desc_ptr: *const usb_device_desc_t = ptr::null();
        match esp_ok(usb_host_get_device_descriptor(dev_hdl, &mut desc_ptr)) {
            Ok(()) => {
                let desc = &*desc_ptr;
                (desc.idVendor, desc.idProduct)
            }
            Err(e) => {
                usb_host_device_close(client_hdl, dev_hdl);
                return Err(UsbHidError::Init(format!(
                    "get_descriptor(addr={dev_addr}): {e}"
                )));
            }
        }
    };

    if !is_official_usb_device(vid, pid) {
        unsafe { usb_host_device_close(client_hdl, dev_hdl) };
        warn!("USB: skipping non-MiniDSP device addr={dev_addr} VID=0x{vid:04X} PID=0x{pid:04X}");
        return Ok(None);
    }
    info!("MiniDSP device: addr={dev_addr} VID=0x{vid:04X} PID=0x{pid:04X}");

    let (hid_intf, ep_in, ep_out) = unsafe {
        let mut config_ptr: *const usb_config_desc_t = ptr::null();
        match esp_ok(usb_host_get_active_config_descriptor(
            dev_hdl,
            &mut config_ptr,
        )) {
            Ok(()) => match find_hid_endpoints(config_ptr) {
                Ok(endpoints) => endpoints,
                Err(err) => {
                    usb_host_device_close(client_hdl, dev_hdl);
                    return Err(err);
                }
            },
            Err(e) => {
                usb_host_device_close(client_hdl, dev_hdl);
                return Err(UsbHidError::Init(format!(
                    "get_config_desc(addr={dev_addr}): {e}"
                )));
            }
        }
    };
    info!("HID interface {hid_intf}: EP IN=0x{ep_in:02X}, EP OUT=0x{ep_out:02X}");

    unsafe {
        match esp_ok(usb_host_interface_claim(client_hdl, dev_hdl, hid_intf, 0)) {
            Ok(()) => info!("HID interface {hid_intf} claimed"),
            Err(e) => {
                usb_host_device_close(client_hdl, dev_hdl);
                return Err(UsbHidError::Init(format!(
                    "interface_claim(addr={dev_addr}, intf={hid_intf}): {e}"
                )));
            }
        }
    }

    cb_state().device_present.store(true, Ordering::SeqCst);
    Ok(Some((dev_hdl, vid, pid, hid_intf, ep_in, ep_out)))
}

/// Allocate a USB transfer object for the given device and endpoint.
fn alloc_transfer(
    dev_hdl: usb_device_handle_t,
    ep_addr: u8,
) -> Result<*mut usb_transfer_t, UsbHidError> {
    unsafe {
        let mut xfer: *mut usb_transfer_t = ptr::null_mut();
        esp_ok(usb_host_transfer_alloc(USB_REPORT_SIZE, 0, &mut xfer))
            .map_err(|e| UsbHidError::Init(format!("alloc xfer 0x{ep_addr:02X}: {e}")))?;
        (*xfer).device_handle = dev_hdl;
        (*xfer).bEndpointAddress = ep_addr;
        (*xfer).callback = Some(transfer_complete_callback);
        (*xfer).context = ptr::null_mut(); // Set by command() before submit
        Ok(xfer)
    }
}

// -- Descriptor parsing --

/// Walk the config descriptor to find the HID interface and its IN/OUT endpoints.
/// Returns (interface_number, ep_in_addr, ep_out_addr).
unsafe fn find_hid_endpoints(
    config: *const usb_config_desc_t,
) -> Result<(u8, u8, u8), UsbHidError> {
    let inner = &*config;
    let total_len = inner.wTotalLength as usize;
    let base = config as *const u8;

    let mut offset = inner.bLength as usize;
    let mut in_hid_interface = false;
    let mut hid_intf_num: u8 = 0;
    let mut ep_in: Option<u8> = None;
    let mut ep_out: Option<u8> = None;

    while offset + 2 <= total_len {
        let desc_len = *base.add(offset) as usize;
        let desc_type = *base.add(offset + 1);

        if desc_len == 0 {
            break; // Prevent infinite loop
        }

        match desc_type {
            DESC_TYPE_INTERFACE if offset + 9 <= total_len => {
                let intf_class = *base.add(offset + 5);
                if intf_class == USB_CLASS_HID {
                    in_hid_interface = true;
                    hid_intf_num = *base.add(offset + 2);
                    ep_in = None;
                    ep_out = None;
                    debug!("found HID interface {hid_intf_num}");
                } else {
                    in_hid_interface = false;
                }
            }
            DESC_TYPE_ENDPOINT if in_hid_interface && offset + 7 <= total_len => {
                let ep_addr = *base.add(offset + 2);
                if ep_addr & EP_DIR_IN != 0 {
                    ep_in = Some(ep_addr);
                } else {
                    ep_out = Some(ep_addr);
                }
            }
            _ => {}
        }

        // If we found both endpoints in the HID interface, we're done
        if let (true, Some(ep_i), Some(ep_o)) = (in_hid_interface, ep_in, ep_out) {
            return Ok((hid_intf_num, ep_i, ep_o));
        }

        offset += desc_len;
    }

    // Some devices only have an IN endpoint; MiniDSP should have both
    if let (Some(ep_i), Some(ep_o)) = (ep_in, ep_out) {
        return Ok((hid_intf_num, ep_i, ep_o));
    }

    Err(UsbHidError::Init(format!(
        "HID endpoints not found (ep_in={ep_in:?}, ep_out={ep_out:?})"
    )))
}

// -- C callbacks --

/// USB host client event callback — called when devices connect/disconnect.
unsafe extern "C" fn client_event_callback(
    event_msg: *const usb_host_client_event_msg_t,
    _arg: *mut core::ffi::c_void,
) {
    let event = &*event_msg;
    let cb = cb_state();

    if event.event == usb_host_client_event_t_USB_HOST_CLIENT_EVENT_NEW_DEV {
        let addr = event.__bindgen_anon_1.new_dev.address;
        info!("USB device connected: addr={addr}");
        cb.device_present.store(true, Ordering::SeqCst);
        cb.last_attach_ms.store(
            (unsafe { esp_timer_get_time() } / 1000) as u32,
            Ordering::SeqCst,
        );
        cb.attach_generation.fetch_add(1, Ordering::SeqCst);
        // Set address and signal under the same lock to avoid races with the waiter
        let (lock, cvar) = &cb.dev_event;
        {
            let mut guard = lock.lock().unwrap();
            *cb.new_dev_addr.lock().unwrap() = Some(addr);
            *guard = true;
            cvar.notify_all();
        }
    } else if event.event == usb_host_client_event_t_USB_HOST_CLIENT_EVENT_DEV_GONE {
        warn!("USB device disconnected");
        cb.last_disconnect_attach_generation.store(
            cb.attach_generation.load(Ordering::SeqCst),
            Ordering::SeqCst,
        );
        cb.connection_epoch.fetch_add(1, Ordering::SeqCst);
        cb.device_present.store(false, Ordering::SeqCst);
        cb.last_attach_ms.store(0, Ordering::SeqCst);
        crate::led::set_device_ready(false);
    } else {
        debug!("USB client event: {}", event.event);
    }
}

/// Transfer completion callback — signals the waiting thread.
unsafe extern "C" fn transfer_complete_callback(xfer: *mut usb_transfer_t) {
    let ctx = (*xfer).context;
    let cb = cb_state();
    let ep = (*xfer).bEndpointAddress;
    let status = (*xfer).status as i32;
    cb.last_xfer_ep.store(ep, Ordering::SeqCst);
    cb.last_xfer_status.store(status, Ordering::SeqCst);
    cb.last_xfer_ms
        .store((esp_timer_get_time() / 1000) as u32, Ordering::SeqCst);
    if ctx.is_null() {
        return; // No waiter — transfer was not submitted by command()
    }
    let sync = &*(ctx as *const XferSync);
    let generation = sync.session_generation();
    // Retirement must not be visible until the callback is done touching sync.
    sync.signal(status);
    if generation == cb.session_generation.load(Ordering::SeqCst) {
        if ep & EP_DIR_IN != 0 {
            cb.in_completion_count.fetch_add(1, Ordering::SeqCst);
        } else {
            cb.out_completion_count.fetch_add(1, Ordering::SeqCst);
        }
    } else {
        debug!(
            "USB completion from stale session ignored (ep=0x{ep:02X}, status={status}, session={}, current={})",
            generation,
            cb.session_generation.load(Ordering::SeqCst)
        );
    }
}

// -- Background tasks --

/// USB host library event handling loop (runs in background thread).
fn usb_host_event_loop() {
    const EVENT_TIMEOUT_MS: u32 = 1000;

    info!("USB host library event loop started");
    loop {
        let mut flags: u32 = 0;
        unsafe {
            let err = usb_host_lib_handle_events(EVENT_TIMEOUT_MS, &mut flags);
            if err != ESP_OK && err != ESP_ERR_TIMEOUT {
                error!("usb_host_lib_handle_events: 0x{err:04x}");
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

/// USB host client event handling loop (runs in background thread).
fn usb_client_event_loop(client_hdl: usb_host_client_handle_t) {
    const EVENT_TIMEOUT_MS: u32 = 1000;

    info!("USB host client event loop started");
    loop {
        unsafe {
            let err = usb_host_client_handle_events(client_hdl, EVENT_TIMEOUT_MS);
            if err != ESP_OK && err != ESP_ERR_TIMEOUT {
                error!("usb_host_client_handle_events: 0x{err:04x}");
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

// -- Helpers --

/// Check an ESP-IDF error code.
fn esp_ok(err: esp_err_t) -> Result<(), String> {
    if err == ESP_OK {
        Ok(())
    } else {
        Err(format!("0x{err:04x}"))
    }
}
