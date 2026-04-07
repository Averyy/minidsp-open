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
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicU16, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Duration;

/// MiniDSP USB VID.
const MINIDSP_VID: u16 = 0x2752;
/// Legacy MiniDSP USB VID (older devices).
const LEGACY_VID: u16 = 0x04D8;
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
        let guard = self
            .mutex
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let result = self
            .condvar
            .wait_timeout_while(guard, timeout, |_| {
                !self.done.load(Ordering::SeqCst)
            })
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
}

static CB_STATE: OnceLock<CallbackState> = OnceLock::new();

fn cb_state() -> &'static CallbackState {
    CB_STATE.get_or_init(|| CallbackState {
        new_dev_addr: Mutex::new(None),
        dev_event: (Mutex::new(false), Condvar::new()),
        device_present: AtomicBool::new(false),
        connect_generation: AtomicU8::new(0),
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
                        // Inject the address as if NEW_DEV just fired
                        let (lock, cvar) = &cb.dev_event;
                        let mut guard = lock.lock().unwrap();
                        *cb.new_dev_addr.lock().unwrap() = Some(addr);
                        cb.device_present.store(true, Ordering::SeqCst);
                        *guard = true;
                        cvar.notify_all();
                        info!("USB: pre-connected device at addr={addr}");
                        break; // Only handle one device
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

        let (dev_hdl, vid, pid, hid_intf, ep_in, ep_out) =
            wait_and_open_device(self.client_hdl, timeout)?;

        // Allocate transfers
        let new_out = alloc_transfer(dev_hdl, ep_out)?;
        let new_in = alloc_transfer(dev_hdl, ep_in)?;

        // Free old transfers if any
        {
            let mut out = self.out_xfer.lock().unwrap();
            if !(*out).is_null() {
                unsafe { usb_host_transfer_free(*out) };
            }
            *out = new_out;
        }
        {
            let mut inp = self.in_xfer.lock().unwrap();
            if !(*inp).is_null() {
                unsafe { usb_host_transfer_free(*inp) };
            }
            *inp = new_in;
        }

        *self.dev_hdl.lock().unwrap() = dev_hdl;
        self.ep_in.store(ep_in, Ordering::SeqCst);
        self.ep_out.store(ep_out, Ordering::SeqCst);
        self.hid_intf.store(hid_intf, Ordering::SeqCst);
        self.vid.store(vid, Ordering::SeqCst);
        self.pid.store(pid, Ordering::SeqCst);

        info!("USB HID bus ready");
        Ok((vid, pid))
    }

    /// True if the USB device is currently connected.
    pub fn is_connected(&self) -> bool {
        cb_state().device_present.load(Ordering::SeqCst)
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
    pub fn command(&self, payload: &[u8]) -> Result<Vec<u8>, UsbHidError> {
        let _lock = self.lock.lock().unwrap();

        // Check connection under lock to avoid TOCTOU race
        if !self.is_connected() {
            return Err(UsbHidError::Disconnected);
        }

        let out_xfer = *self.out_xfer.lock().unwrap();
        let in_xfer = *self.in_xfer.lock().unwrap();
        if out_xfer.is_null() || in_xfer.is_null() {
            return Err(UsbHidError::Disconnected);
        }

        // Encode frame: [length, payload, checksum] padded to 64 bytes with 0xFF
        let frame = minidsp_protocol::packet::encode_frame(payload);
        let mut out_buf = [0xFF_u8; USB_REPORT_SIZE];
        let copy_len = frame.len().min(USB_REPORT_SIZE);
        out_buf[..copy_len].copy_from_slice(&frame[..copy_len]);

        // Submit OUT transfer with dedicated sync
        self.out_sync.reset();
        unsafe {
            ptr::copy_nonoverlapping(
                out_buf.as_ptr(),
                (*out_xfer).data_buffer,
                USB_REPORT_SIZE,
            );
            (*out_xfer).num_bytes = USB_REPORT_SIZE as i32;
            (*out_xfer).context =
                &self.out_sync as *const XferSync as *mut core::ffi::c_void;
            esp_ok(usb_host_transfer_submit(out_xfer))
                .map_err(|e| UsbHidError::Transfer(format!("submit OUT: {e}")))?;
        }
        let status = self.out_sync.wait(XFER_TIMEOUT)?;
        if status != 0 {
            return Err(UsbHidError::Transfer(format!("OUT status: {status}")));
        }

        // Submit IN transfer with dedicated sync
        self.in_sync.reset();
        unsafe {
            (*in_xfer).num_bytes = USB_REPORT_SIZE as i32;
            (*in_xfer).context =
                &self.in_sync as *const XferSync as *mut core::ffi::c_void;
            esp_ok(usb_host_transfer_submit(in_xfer))
                .map_err(|e| UsbHidError::Transfer(format!("submit IN: {e}")))?;
        }
        let status = self.in_sync.wait(XFER_TIMEOUT)?;
        if status != 0 {
            return Err(UsbHidError::Transfer(format!("IN status: {status}")));
        }

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

        // Log raw response for debugging
        if resp_buf.len() >= 4 {
            debug!(
                "USB IN: {} bytes, first 8: {:02x?}",
                resp_buf.len(),
                &resp_buf[..resp_buf.len().min(8)]
            );
        }

        // Decode HID response.
        // ESP-IDF USB Host returns 64 bytes from the interrupt IN endpoint.
        // The first byte may be a report ID (0x00 or 0x01). Skip it to get the frame.
        // Frame format: [length][payload][checksum][0xFF padding]
        // Try decoding with and without report ID prefix.
        let data = &resp_buf;

        // Try decode starting at byte 0 (no report ID prefix)
        if let Some(payload) = minidsp_protocol::packet::decode_frame(data) {
            return Ok(payload);
        }

        // Try decode starting at byte 1 (report ID prefix from USB Host)
        if data.len() > 1 {
            if let Some(payload) = minidsp_protocol::packet::decode_frame(&data[1..]) {
                return Ok(payload);
            }
        }

        // Neither worked — log and return error
        let hex: String = data.iter().take(16).map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ");
        Err(UsbHidError::Protocol(format!("invalid response frame ({} bytes): {hex}", data.len())))
    }

    /// Best-effort cleanup of the current device (release interface, close).
    fn cleanup_device(&self) {
        let dev_hdl = *self.dev_hdl.lock().unwrap();
        if dev_hdl.is_null() {
            return;
        }
        let hid_intf = self.hid_intf.load(Ordering::SeqCst);
        unsafe {
            for attempt in 0..5 {
                let err = usb_host_interface_release(self.client_hdl, dev_hdl, hid_intf);
                if err == ESP_OK as i32 {
                    break;
                }
                if attempt < 4 {
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
            usb_host_device_close(self.client_hdl, dev_hdl);
        }
        *self.dev_hdl.lock().unwrap() = ptr::null_mut();
    }
}

impl Drop for UsbHidBus {
    fn drop(&mut self) {
        self.cleanup_device();
        unsafe {
            let out = *self.out_xfer.lock().unwrap();
            if !out.is_null() {
                usb_host_transfer_free(out);
            }
            let inp = *self.in_xfer.lock().unwrap();
            if !inp.is_null() {
                usb_host_transfer_free(inp);
            }
        }
    }
}

// -- Shared device open logic --

/// Wait for a MiniDSP USB device and open it. Returns handles and endpoint info.
fn wait_and_open_device(
    client_hdl: usb_host_client_handle_t,
    timeout: Duration,
) -> Result<(usb_device_handle_t, u16, u16, u8, u8, u8), UsbHidError> {
    info!("waiting for MiniDSP device (VID 0x{MINIDSP_VID:04X})...");

    let cb = cb_state();
    let deadline = std::time::Instant::now() + timeout;

    // Loop: wait for device events, try to open each one, skip non-MiniDSP devices
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(UsbHidError::DeviceNotFound);
        }

        // Wait for a device address under the dev_event lock (callback sets both
        // new_dev_addr and dev_event under this same lock, so no race possible).
        let dev_addr = {
            let (lock, cvar) = &cb.dev_event;
            let guard = lock.lock().unwrap();
            // Use the condvar predicate to check for a queued address.
            // This handles both "already queued" and "need to wait" cases.
            let result = cvar
                .wait_timeout_while(guard, remaining, |_| {
                    cb.new_dev_addr.lock().unwrap().is_none()
                })
                .unwrap();
            if result.1.timed_out() {
                return Err(UsbHidError::DeviceNotFound);
            }
            match cb.new_dev_addr.lock().unwrap().take() {
                Some(addr) => addr,
                None => continue,
            }
        };
        info!("USB device connected at address {dev_addr}");

    // Open device (may fail if device disappeared during enumeration)
    let dev_hdl = unsafe {
        let mut hdl: usb_device_handle_t = ptr::null_mut();
        match esp_ok(usb_host_device_open(client_hdl, dev_addr, &mut hdl)) {
            Ok(()) => hdl,
            Err(e) => {
                warn!("device_open({dev_addr}) failed: {e} — retrying");
                continue;
            }
        }
    };

    // Check VID
    let (vid, pid) = unsafe {
        let mut desc_ptr: *const usb_device_desc_t = ptr::null();
        esp_ok(usb_host_get_device_descriptor(dev_hdl, &mut desc_ptr))
            .map_err(|e| UsbHidError::Init(format!("get_descriptor: {e}")))?;
        let desc = &(*desc_ptr).__bindgen_anon_1;
        (desc.idVendor, desc.idProduct)
    };

    if vid != MINIDSP_VID && vid != LEGACY_VID {
        // Not a MiniDSP — close and try the next device
        unsafe { usb_host_device_close(client_hdl, dev_hdl) };
        warn!("skipping non-MiniDSP device VID=0x{vid:04X}");
        continue;
    }
    info!("MiniDSP device: VID=0x{vid:04X} PID=0x{pid:04X}");

    // Get config descriptor and find HID interface + endpoints
    let (hid_intf, ep_in, ep_out) = unsafe {
        let mut config_ptr: *const usb_config_desc_t = ptr::null();
        esp_ok(usb_host_get_active_config_descriptor(dev_hdl, &mut config_ptr))
            .map_err(|e| UsbHidError::Init(format!("get_config_desc: {e}")))?;

        find_hid_endpoints(config_ptr)?
    };
    info!("HID interface {hid_intf}: EP IN=0x{ep_in:02X}, EP OUT=0x{ep_out:02X}");

    // Claim HID interface
    unsafe {
        esp_ok(usb_host_interface_claim(client_hdl, dev_hdl, hid_intf, 0))
            .map_err(|e| UsbHidError::Init(format!("interface_claim({hid_intf}): {e}")))?;
    }
    info!("HID interface {hid_intf} claimed");

    cb.device_present.store(true, Ordering::SeqCst);

    return Ok((dev_hdl, vid, pid, hid_intf, ep_in, ep_out));
    } // end loop
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
