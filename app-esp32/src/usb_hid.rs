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
//! - interface_release() can fail if URBs in-flight — Drop retries 5 times.

use esp_idf_sys::*;
use log::{debug, error, info, warn};
use minidsp_protocol::device::probe::is_official_usb_device;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU16, AtomicU32, AtomicU8, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
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
    mutex: Mutex<()>,
    condvar: Condvar,
}

impl XferSync {
    fn new() -> Self {
        Self {
            done: AtomicBool::new(false),
            status: AtomicI32::new(-1),
            mutex: Mutex::new(()),
            condvar: Condvar::new(),
        }
    }

    fn reset(&self) {
        self.done.store(false, Ordering::SeqCst);
        self.status.store(-1, Ordering::SeqCst);
    }

    fn signal(&self, status: i32) {
        self.status.store(status, Ordering::SeqCst);
        self.done.store(true, Ordering::SeqCst);
        let _lock = self.mutex.lock().unwrap_or_else(|e| e.into_inner());
        self.condvar.notify_one();
    }

    fn wait(&self, timeout: Duration) -> Result<i32, UsbHidError> {
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
    /// Incremented each time a new device connects (used to detect reconnection).
    connect_generation: AtomicU8,
    /// esp_timer_get_time()/1000 when the latest attach was observed.
    last_attach_ms: AtomicU32,
}

static CB_STATE: OnceLock<CallbackState> = OnceLock::new();

fn cb_state() -> &'static CallbackState {
    CB_STATE.get_or_init(|| CallbackState {
        new_dev_addr: Mutex::new(None),
        dev_event: (Mutex::new(false), Condvar::new()),
        device_present: AtomicBool::new(false),
        connect_generation: AtomicU8::new(0),
        last_attach_ms: AtomicU32::new(0),
    })
}

/// USB HID bus — thread-safe handle for communicating with a MiniDSP device.
pub struct UsbHidBus {
    client_hdl: usb_host_client_handle_t,
    dev_hdl: Mutex<usb_device_handle_t>,
    out_xfer: Mutex<*mut usb_transfer_t>,
    in_xfer: Mutex<*mut usb_transfer_t>,
    ep_in: AtomicU8,
    ep_out: AtomicU8,
    hid_intf: AtomicU8,
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

    fn install_open_device(
        &self,
        device: (usb_device_handle_t, u16, u16, u8, u8, u8),
    ) -> Result<(u16, u16), UsbHidError> {
        let (dev_hdl, vid, pid, hid_intf, ep_in, ep_out) = device;

        let new_out = alloc_transfer(dev_hdl, ep_out)?;
        let new_in = alloc_transfer(dev_hdl, ep_in)?;

        self.clear_transfers();
        *self.out_xfer.lock().unwrap() = new_out;
        *self.in_xfer.lock().unwrap() = new_in;

        *self.dev_hdl.lock().unwrap() = dev_hdl;
        self.ep_in.store(ep_in, Ordering::SeqCst);
        self.ep_out.store(ep_out, Ordering::SeqCst);
        self.hid_intf.store(hid_intf, Ordering::SeqCst);
        self.vid.store(vid, Ordering::SeqCst);
        self.pid.store(pid, Ordering::SeqCst);

        info!("USB HID bus ready");
        Ok((vid, pid))
    }

    fn reset_transfers_for_device(
        &self,
        dev_hdl: usb_device_handle_t,
        ep_in: u8,
        ep_out: u8,
    ) -> Result<(), UsbHidError> {
        let new_out = alloc_transfer(dev_hdl, ep_out)?;
        let new_in = alloc_transfer(dev_hdl, ep_in)?;
        self.clear_transfers();
        *self.out_xfer.lock().unwrap() = new_out;
        *self.in_xfer.lock().unwrap() = new_in;
        Ok(())
    }

    fn recover_endpoints_after_timeout(&self) {
        let dev_hdl = *self.dev_hdl.lock().unwrap();
        if dev_hdl.is_null() {
            return;
        }

        let ep_in = self.ep_in.load(Ordering::SeqCst);
        let ep_out = self.ep_out.load(Ordering::SeqCst);
        for ep in [ep_out, ep_in] {
            if ep == 0 {
                continue;
            }
            unsafe {
                let halt = usb_host_endpoint_halt(dev_hdl, ep);
                if halt != ESP_OK as i32 && halt != ESP_ERR_INVALID_STATE as i32 {
                    warn!("USB endpoint 0x{ep:02X} halt failed: 0x{halt:04x}");
                }
                let flush = usb_host_endpoint_flush(dev_hdl, ep);
                if flush != ESP_OK as i32 && flush != ESP_ERR_INVALID_STATE as i32 {
                    warn!("USB endpoint 0x{ep:02X} flush failed: 0x{flush:04x}");
                }
                let clear = usb_host_endpoint_clear(dev_hdl, ep);
                if clear != ESP_OK as i32 && clear != ESP_ERR_INVALID_STATE as i32 {
                    warn!("USB endpoint 0x{ep:02X} clear failed: 0x{clear:04x}");
                }
            }
        }

        if let Err(err) = self.reset_transfers_for_device(dev_hdl, ep_in, ep_out) {
            warn!("USB transfer reset after timeout failed: {err}");
        }
    }

    fn submit_out_transfer(
        &self,
        out_xfer: *mut usb_transfer_t,
        out_buf: &[u8; USB_REPORT_SIZE],
        payload: &[u8],
        timeout: Duration,
    ) -> Result<(), UsbHidError> {
        self.out_sync.reset();
        unsafe {
            ptr::copy_nonoverlapping(out_buf.as_ptr(), (*out_xfer).data_buffer, USB_REPORT_SIZE);
            (*out_xfer).num_bytes = USB_REPORT_SIZE as i32;
            (*out_xfer).context = &self.out_sync as *const XferSync as *mut core::ffi::c_void;
            esp_ok(usb_host_transfer_submit(out_xfer))
                .map_err(|e| UsbHidError::Transfer(format!("submit OUT: {e}")))?;
        }
        debug!("USB OUT submitted for payload: {:02x?}", payload);
        let status = self.out_sync.wait(timeout).map_err(|e| {
            warn!("USB OUT wait failed for {:02x?}: {}", payload, e);
            self.recover_endpoints_after_timeout();
            e
        })?;
        if status != 0 {
            warn!("USB OUT completion status {} for {:02x?}", status, payload);
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

        // Start background event task
        std::thread::Builder::new()
            .name("usb_events".into())
            .stack_size(16384)
            .spawn(usb_host_event_loop)
            .map_err(|e| UsbHidError::Init(format!("spawn event task: {e}")))?;

        // Register USB host client
        let client_hdl = unsafe {
            let mut hdl: usb_host_client_handle_t = ptr::null_mut();
            let config = usb_host_client_config_t {
                is_synchronous: false,
                max_num_event_msg: 5,
                __bindgen_anon_1: usb_host_client_config_t__bindgen_ty_1 {
                    async_: usb_host_client_config_t__bindgen_ty_1__bindgen_ty_1 {
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
            .stack_size(16384)
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
            if err == ESP_OK as i32 && num_devs > 0 {
                info!("USB: {num_devs} device(s) already connected at boot");
                let cb = cb_state();
                for i in 0..num_devs as usize {
                    let addr = dev_addrs[i];
                    if addr > 0 {
                        let (lock, cvar) = &cb.dev_event;
                        let mut guard = lock.lock().unwrap();
                        *cb.new_dev_addr.lock().unwrap() = Some(addr);
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
            hid_intf: AtomicU8::new(0),
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

        // Close old device if any (required for USB host to detect reconnection)
        self.cleanup_device();

        if let Some(device) = try_open_present_device(self.client_hdl, "scan")? {
            return self.install_open_device(device);
        }

        let device = wait_and_open_device(self.client_hdl, timeout)?;
        self.install_open_device(device)
    }

    pub fn has_present_device(&self) -> Result<bool, UsbHidError> {
        Ok(!list_present_device_addrs()?.is_empty())
    }

    pub fn reopen_present_device(&self) -> Result<Option<(u16, u16)>, UsbHidError> {
        let _lock = self.lock.lock().unwrap();
        self.cleanup_device();
        match try_open_present_device(self.client_hdl, "reopen")? {
            Some(device) => self.install_open_device(device).map(Some),
            None => Ok(None),
        }
    }

    /// True if the USB device is currently connected.
    pub fn is_connected(&self) -> bool {
        cb_state().device_present.load(Ordering::SeqCst)
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
    pub fn command_with_timeout(
        &self,
        payload: &[u8],
        timeout: Duration,
    ) -> Result<Vec<u8>, UsbHidError> {
        let _lock = self.lock.lock().unwrap();
        debug!("USB command start: {:02x?}", payload);

        // Check connection under lock to avoid TOCTOU race
        if !self.is_connected() {
            warn!("USB command rejected while disconnected: {:02x?}", payload);
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
                return Err(UsbHidError::Timeout);
            }

            // Submit IN transfer
            self.in_sync.reset();
            unsafe {
                (*in_xfer).num_bytes = USB_REPORT_SIZE as i32;
                (*in_xfer).context = &self.in_sync as *const XferSync as *mut core::ffi::c_void;
                esp_ok(usb_host_transfer_submit(in_xfer))
                    .map_err(|e| UsbHidError::Transfer(format!("submit IN: {e}")))?;
            }
            debug!("USB IN submitted for payload: {:02x?}", payload);
            let status = self.in_sync.wait(remaining).map_err(|e| {
                warn!("USB IN wait failed for {:02x?}: {}", payload, e);
                self.recover_endpoints_after_timeout();
                e
            })?;
            if status != 0 {
                warn!("USB IN completion status {} for {:02x?}", status, payload);
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

    /// Send a command payload and receive the response with the default timeout.
    pub fn command(&self, payload: &[u8]) -> Result<Vec<u8>, UsbHidError> {
        self.command_with_timeout(payload, XFER_TIMEOUT)
    }

    /// Best-effort cleanup of the current device (release interface, close).
    fn cleanup_device(&self) {
        let dev_hdl = *self.dev_hdl.lock().unwrap();
        if dev_hdl.is_null() {
            self.clear_transfers();
            return;
        }
        let hid_intf = self.hid_intf.load(Ordering::SeqCst);
        self.recover_endpoints_after_timeout();
        unsafe {
            for attempt in 0..5 {
                let err = usb_host_interface_release(self.client_hdl, dev_hdl, hid_intf);
                if err == ESP_OK as i32 {
                    break;
                }
                if attempt == 4 {
                    warn!("USB interface_release({hid_intf}) failed: 0x{err:04x}");
                }
                if attempt < 4 {
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
            let err = usb_host_device_close(self.client_hdl, dev_hdl);
            if err != ESP_OK as i32 {
                warn!("USB device_close failed: 0x{err:04x}");
            }
        }
        *self.dev_hdl.lock().unwrap() = ptr::null_mut();
        self.clear_transfers();
        self.ep_in.store(0, Ordering::SeqCst);
        self.ep_out.store(0, Ordering::SeqCst);
        self.hid_intf.store(0, Ordering::SeqCst);
        self.vid.store(0, Ordering::SeqCst);
        self.pid.store(0, Ordering::SeqCst);
    }
}

impl Drop for UsbHidBus {
    fn drop(&mut self) {
        self.cleanup_device();
    }
}

// -- Shared device open logic --

/// Wait for a MiniDSP USB device and open it. Returns handles and endpoint info.
fn wait_and_open_device(
    client_hdl: usb_host_client_handle_t,
    timeout: Duration,
) -> Result<(usb_device_handle_t, u16, u16, u8, u8, u8), UsbHidError> {
    const ADDR_SCAN_INTERVAL: Duration = Duration::from_millis(500);

    info!("waiting for official MiniDSP USB HID device...");

    let cb = cb_state();
    let deadline = std::time::Instant::now() + timeout;

    // Loop: wait for device events, try to open each one, skip non-MiniDSP devices
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(UsbHidError::DeviceNotFound);
        }

        if let Some(device) = try_open_present_device(client_hdl, "wait")? {
            return Ok(device);
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
        if let Some(device) = open_device_at_addr(client_hdl, dev_addr)? {
            return Ok(device);
        }
    } // end loop
}

fn try_open_present_device(
    client_hdl: usb_host_client_handle_t,
    reason: &str,
) -> Result<Option<(usb_device_handle_t, u16, u16, u8, u8, u8)>, UsbHidError> {
    let addrs = list_present_device_addrs()?;
    if addrs.is_empty() {
        debug!("USB: no devices present in host addr list ({reason})");
        return Ok(None);
    }

    info!("USB: host addr list ({reason}) = {addrs:?}");
    for addr in addrs {
        if let Some(device) = open_device_at_addr(client_hdl, addr)? {
            return Ok(Some(device));
        }
    }
    Ok(None)
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
) -> Result<Option<(usb_device_handle_t, u16, u16, u8, u8, u8)>, UsbHidError> {
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
                let desc = &(*desc_ptr).__bindgen_anon_1;
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
            Ok(()) => find_hid_endpoints(config_ptr)?,
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
    let inner = &(*config).__bindgen_anon_1;
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
        if in_hid_interface && ep_in.is_some() && ep_out.is_some() {
            return Ok((hid_intf_num, ep_in.unwrap(), ep_out.unwrap()));
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
        cb.connect_generation.fetch_add(1, Ordering::SeqCst);
        cb.last_attach_ms.store(
            (unsafe { esp_timer_get_time() } / 1000) as u32,
            Ordering::SeqCst,
        );
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
    if ctx.is_null() {
        return; // No waiter — transfer was not submitted by command()
    }
    let sync = &*(ctx as *const XferSync);
    sync.signal((*xfer).status as i32);
}

// -- Background tasks --

/// USB host library event handling loop (runs in background thread).
fn usb_host_event_loop() {
    loop {
        let mut flags: u32 = 0;
        unsafe {
            let err = usb_host_lib_handle_events(u32::MAX, &mut flags);
            if err != ESP_OK as i32 && err != ESP_ERR_TIMEOUT as i32 {
                error!("usb_host_lib_handle_events: 0x{err:04x}");
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

/// USB host client event handling loop (runs in background thread).
fn usb_client_event_loop(client_hdl: usb_host_client_handle_t) {
    loop {
        unsafe {
            let err = usb_host_client_handle_events(client_hdl, u32::MAX);
            if err != ESP_OK as i32 && err != ESP_ERR_TIMEOUT as i32 {
                error!("usb_host_client_handle_events: 0x{err:04x}");
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

// -- Helpers --

/// Check an ESP-IDF error code.
fn esp_ok(err: esp_err_t) -> Result<(), String> {
    if err == ESP_OK as i32 {
        Ok(())
    } else {
        Err(format!("0x{err:04x}"))
    }
}
