//! USB Audio Class 2 host wrapper for ESP32-S3.
//!
//! This wraps the external `esp-uac2-host` component and maintains shared
//! playback state independently from the HID device state.

use esp_idf_sys::{esp_err_t, esp_timer_get_time, BaseType_t, ESP_OK};
use log::{debug, error, info, warn};
use minidsp_protocol::playback::BitDepth;
use std::ffi::c_void;
use std::fmt::Write as _;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};

pub type SharedAudioState = Arc<AudioState>;
static TX_NEED_DATA_COUNT: AtomicU32 = AtomicU32::new(0);

pub struct AudioState {
    device: Mutex<Option<Arc<Uac2Device>>>,
    connected_flag: Mutex<bool>,
    pub connected: Condvar,
    pub playing: AtomicBool,
}

impl AudioState {
    pub fn new() -> SharedAudioState {
        Arc::new(Self {
            device: Mutex::new(None),
            connected_flag: Mutex::new(false),
            connected: Condvar::new(),
            playing: AtomicBool::new(false),
        })
    }

    pub fn current_device(&self) -> Option<Arc<Uac2Device>> {
        self.device.lock().unwrap().clone()
    }

    pub fn replace_device(&self, device: Arc<Uac2Device>) -> Option<Arc<Uac2Device>> {
        let mut slot = self.device.lock().unwrap();
        let previous = slot.replace(device);
        *self.connected_flag.lock().unwrap() = true;
        self.connected.notify_all();
        previous
    }

    pub fn clear_device_by_handle(
        &self,
        handle: uac2_host_device_handle_t,
    ) -> Option<Arc<Uac2Device>> {
        let mut slot = self.device.lock().unwrap();
        let should_clear = slot
            .as_ref()
            .is_some_and(|device| device.handle_ptr() == handle);
        if should_clear {
            *self.connected_flag.lock().unwrap() = false;
            slot.take()
        } else {
            None
        }
    }

    pub fn try_begin_playback(&self) -> bool {
        self.playing
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    pub fn finish_playback(&self) {
        self.playing.store(false, Ordering::SeqCst);
    }

    pub fn is_playing(&self) -> bool {
        self.playing.load(Ordering::SeqCst)
    }
}

pub fn install(state: SharedAudioState) -> Result<(), UsbAudioError> {
    let config = uac2_host_driver_config_t {
        create_background_task: true,
        task_priority: 3,
        stack_size: 6 * 1024,
        core_id: UAC2_TASK_CORE,
        callback: Some(driver_event_cb),
        callback_arg: Arc::as_ptr(&state) as *mut c_void,
    };

    unsafe {
        esp_ok(uac2_host_install(&config))
            .map_err(|e| UsbAudioError::Install(format!("uac2_host_install: {e}")))?;
    }
    info!("UAC2 host driver installed");
    Ok(())
}

pub fn server_time_us() -> i64 {
    unsafe { esp_timer_get_time() }
}

pub fn reset_tx_need_data_count() {
    TX_NEED_DATA_COUNT.store(0, Ordering::SeqCst);
}

pub fn tx_need_data_count() -> u32 {
    TX_NEED_DATA_COUNT.load(Ordering::SeqCst)
}

#[derive(Debug)]
pub enum UsbAudioError {
    Install(String),
    Open(String),
    Unsupported(String),
    Start(String),
    Write(String),
    Stop(String),
}

impl std::fmt::Display for UsbAudioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Install(msg) => write!(f, "{msg}"),
            Self::Open(msg) => write!(f, "{msg}"),
            Self::Unsupported(msg) => write!(f, "{msg}"),
            Self::Start(msg) => write!(f, "{msg}"),
            Self::Write(msg) => write!(f, "{msg}"),
            Self::Stop(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for UsbAudioError {}

pub struct Uac2Device {
    handle: Mutex<uac2_host_device_handle_t>,
}

unsafe impl Send for Uac2Device {}
unsafe impl Sync for Uac2Device {}

impl Uac2Device {
    fn new(handle: uac2_host_device_handle_t) -> Self {
        Self {
            handle: Mutex::new(handle),
        }
    }

    pub fn handle_ptr(&self) -> uac2_host_device_handle_t {
        *self.handle.lock().unwrap()
    }

    pub fn start_stream(&self, sample_rate: u32, bit_depth: BitDepth) -> Result<(), UsbAudioError> {
        self.validate_stream(sample_rate, bit_depth)?;
        let config = uac2_host_stream_config_t {
            channels: 2,
            bit_resolution: bit_depth.bits(),
            sample_freq: sample_rate,
            flags: 0,
        };
        unsafe {
            esp_ok(uac2_host_device_start(self.handle_ptr(), &config))
                .map_err(|e| UsbAudioError::Start(format!("uac2_host_device_start: {e}")))
        }
    }

    fn validate_stream(&self, sample_rate: u32, bit_depth: BitDepth) -> Result<(), UsbAudioError> {
        const MAX_ALT_SETTINGS: u8 = 8;
        const MAX_SAMPLE_RATE_RANGES: usize = 16;

        let mut matching_alts = 0usize;
        let mut alt_descriptions = String::new();

        for alt in 1..=MAX_ALT_SETTINGS {
            let mut param = uac2_host_dev_alt_param_t::default();
            let err = unsafe { uac2_host_get_device_alt_param(self.handle_ptr(), alt, &mut param) };
            if err != ESP_OK as esp_err_t {
                if alt > 1 {
                    break;
                }
                continue;
            }

            if !alt_descriptions.is_empty() {
                alt_descriptions.push_str(", ");
            }
            let _ = write!(
                alt_descriptions,
                "alt {}: {}ch/{}-bit",
                param.alt_setting, param.channels, param.bit_resolution
            );

            if param.channels == 2 && param.bit_resolution == bit_depth.bits() {
                matching_alts += 1;
            }
        }

        if matching_alts == 0 {
            let supported = if alt_descriptions.is_empty() {
                "no matching alternate settings discovered".to_string()
            } else {
                format!("supported formats: {alt_descriptions}")
            };
            return Err(UsbAudioError::Unsupported(format!(
                "USB audio device does not support 2-channel {}-bit playback ({supported})",
                bit_depth.bits()
            )));
        }

        let mut ranges = [uac2_sample_rate_range_t::default(); MAX_SAMPLE_RATE_RANGES];
        let mut num_ranges = MAX_SAMPLE_RATE_RANGES as u8;
        let err = unsafe {
            uac2_host_device_get_sample_rate_range(
                self.handle_ptr(),
                ranges.as_mut_ptr(),
                &mut num_ranges,
            )
        };
        if err == ESP_OK as esp_err_t {
            let advertised = &ranges[..num_ranges as usize];
            if !advertised.is_empty()
                && !advertised
                    .iter()
                    .any(|range| sample_rate_in_range(sample_rate, *range))
            {
                let supported = advertised
                    .iter()
                    .map(format_sample_rate_range)
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(UsbAudioError::Unsupported(format!(
                    "USB audio device does not advertise {sample_rate} Hz (supported: {supported})"
                )));
            }
        }

        Ok(())
    }

    pub fn write(&self, data: &[u8], timeout_ms: u32) -> Result<(), UsbAudioError> {
        unsafe {
            esp_ok(uac2_host_device_write(
                self.handle_ptr(),
                data.as_ptr(),
                data.len() as u32,
                timeout_ms,
            ))
            .map_err(|e| UsbAudioError::Write(format!("uac2_host_device_write: {e}")))
        }
    }

    pub fn start_time_us(&self) -> i64 {
        unsafe { uac2_host_device_get_start_time(self.handle_ptr()) }
    }

    pub fn feedback_raw(&self) -> u32 {
        unsafe { uac2_host_device_get_feedback(self.handle_ptr()) }
    }

    pub fn stop_stream(&self) -> Result<(), UsbAudioError> {
        unsafe {
            esp_ok(uac2_host_device_stop(self.handle_ptr()))
                .map_err(|e| UsbAudioError::Stop(format!("uac2_host_device_stop: {e}")))
        }
    }

    fn close(&self) -> Result<(), UsbAudioError> {
        let mut handle = self.handle.lock().unwrap();
        if handle.is_null() {
            return Ok(());
        }

        let current = *handle;
        *handle = ptr::null_mut();
        unsafe {
            let _ = uac2_host_device_stop(current);
            esp_ok(uac2_host_device_close(current))
                .map_err(|e| UsbAudioError::Stop(format!("uac2_host_device_close: {e}")))
        }
    }
}

impl Drop for Uac2Device {
    fn drop(&mut self) {
        if let Err(err) = self.close() {
            warn!("UAC2 device cleanup failed: {err}");
        }
    }
}

extern "C" fn driver_event_cb(
    addr: u8,
    iface_num: u8,
    event: uac2_host_driver_event_t,
    arg: *mut c_void,
) {
    if event != uac2_host_driver_event_t::UAC2_HOST_DRIVER_EVENT_TX_CONNECTED {
        return;
    }

    let state = clone_audio_state(arg);
    if let Err(err) = std::thread::Builder::new()
        .name("uac2_open".into())
        .stack_size(12 * 1024)
        .spawn(move || open_tx_device(state, addr, iface_num))
    {
        error!("failed to spawn UAC2 open thread: {err}");
    }
}

extern "C" fn device_event_cb(
    dev: uac2_host_device_handle_t,
    event: uac2_host_device_event_t,
    arg: *mut c_void,
) {
    let state = clone_audio_state(arg);
    match event {
        uac2_host_device_event_t::UAC2_HOST_DEVICE_EVENT_DISCONNECTED => {
            warn!("UAC2 device disconnected");
            if let Some(device) = state.clear_device_by_handle(dev) {
                if !state.is_playing() {
                    defer_drop(device);
                }
            }
        }
        uac2_host_device_event_t::UAC2_HOST_DEVICE_EVENT_TRANSFER_ERROR => {
            warn!("UAC2 transfer error");
        }
        uac2_host_device_event_t::UAC2_HOST_DEVICE_EVENT_STREAM_ERROR => {
            warn!("UAC2 stream error");
        }
        uac2_host_device_event_t::UAC2_HOST_DEVICE_EVENT_TX_DONE => {
            TX_NEED_DATA_COUNT.fetch_add(1, Ordering::SeqCst);
            debug!("UAC2 TX needs more data");
        }
        uac2_host_device_event_t::UAC2_HOST_DEVICE_EVENT_RX_DONE => {}
    }
}

fn open_tx_device(state: SharedAudioState, addr: u8, iface_num: u8) {
    let config = uac2_host_device_config_t {
        addr,
        iface_num,
        buffer_size: TX_RING_BUFFER_BYTES as u32,
        buffer_threshold: 0,
        callback: Some(device_event_cb),
        callback_arg: Arc::as_ptr(&state) as *mut c_void,
    };
    let mut handle: uac2_host_device_handle_t = ptr::null_mut();

    let result = unsafe { esp_ok(uac2_host_device_open(&config, &mut handle)) };
    match result {
        Ok(()) => {
            let device = Arc::new(Uac2Device::new(handle));
            if let Some(previous) = state.replace_device(device) {
                drop(previous);
            }
            info!("UAC2 TX interface opened: addr={addr} iface={iface_num}");
        }
        Err(err) => {
            let open_err = UsbAudioError::Open(format!(
                "open UAC2 TX interface addr={addr} iface={iface_num}: {err}"
            ));
            error!("{open_err}");
        }
    }
}

fn defer_drop(device: Arc<Uac2Device>) {
    let clone = device.clone();
    if let Err(err) = std::thread::Builder::new()
        .name("uac2_gc".into())
        .stack_size(8 * 1024)
        .spawn(move || drop(clone))
    {
        error!("failed to spawn UAC2 cleanup thread: {err}");
        std::mem::forget(device);
    }
}

fn clone_audio_state(arg: *mut c_void) -> SharedAudioState {
    let ptr = arg as *const AudioState;
    unsafe {
        Arc::increment_strong_count(ptr);
        Arc::from_raw(ptr)
    }
}

fn esp_ok(err: esp_err_t) -> Result<(), String> {
    if err == ESP_OK as esp_err_t {
        Ok(())
    } else {
        Err(format!("0x{err:04x}"))
    }
}

fn sample_rate_in_range(sample_rate: u32, range: uac2_sample_rate_range_t) -> bool {
    if range.min == range.max {
        sample_rate == range.min
    } else {
        sample_rate >= range.min && sample_rate <= range.max
    }
}

fn format_sample_rate_range(range: &uac2_sample_rate_range_t) -> String {
    if range.min == range.max {
        format!("{} Hz", range.min)
    } else if range.res > 0 {
        format!("{}-{} Hz step {}", range.min, range.max, range.res)
    } else {
        format!("{}-{} Hz", range.min, range.max)
    }
}

#[allow(non_camel_case_types)]
type uac2_host_device_handle_t = *mut uac2_interface;

#[repr(C)]
pub struct uac2_interface {
    _private: [u8; 0],
}

#[allow(non_camel_case_types)]
#[allow(dead_code)]
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum uac2_host_driver_event_t {
    UAC2_HOST_DRIVER_EVENT_TX_CONNECTED = 0,
    UAC2_HOST_DRIVER_EVENT_RX_CONNECTED = 1,
}

#[allow(non_camel_case_types)]
#[allow(dead_code)]
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum uac2_host_device_event_t {
    UAC2_HOST_DEVICE_EVENT_TX_DONE = 0,
    UAC2_HOST_DEVICE_EVENT_RX_DONE = 1,
    UAC2_HOST_DEVICE_EVENT_TRANSFER_ERROR = 2,
    UAC2_HOST_DEVICE_EVENT_DISCONNECTED = 3,
    UAC2_HOST_DEVICE_EVENT_STREAM_ERROR = 4,
}

#[allow(non_camel_case_types)]
type uac2_host_driver_event_cb_t =
    Option<extern "C" fn(u8, u8, uac2_host_driver_event_t, *mut c_void)>;

#[allow(non_camel_case_types)]
type uac2_host_device_event_cb_t =
    Option<extern "C" fn(uac2_host_device_handle_t, uac2_host_device_event_t, *mut c_void)>;

#[allow(non_camel_case_types)]
#[repr(C)]
struct uac2_host_driver_config_t {
    create_background_task: bool,
    task_priority: usize,
    stack_size: usize,
    core_id: BaseType_t,
    callback: uac2_host_driver_event_cb_t,
    callback_arg: *mut c_void,
}

#[allow(non_camel_case_types)]
#[repr(C)]
struct uac2_host_device_config_t {
    addr: u8,
    iface_num: u8,
    buffer_size: u32,
    buffer_threshold: u32,
    callback: uac2_host_device_event_cb_t,
    callback_arg: *mut c_void,
}

#[allow(non_camel_case_types)]
#[derive(Clone, Copy, Default)]
#[repr(C)]
struct uac2_sample_rate_range_t {
    min: u32,
    max: u32,
    res: u32,
}

#[allow(non_camel_case_types)]
#[repr(C)]
struct uac2_host_stream_config_t {
    channels: u8,
    bit_resolution: u8,
    sample_freq: u32,
    flags: u16,
}

#[allow(non_camel_case_types)]
#[derive(Default)]
#[repr(C)]
struct uac2_host_dev_alt_param_t {
    alt_setting: u8,
    channels: u8,
    bit_resolution: u8,
    sub_slot_size: u8,
    ep_max_packet_size: u16,
    ep_addr: u8,
    fb_ep_addr: u8,
}

unsafe extern "C" {
    fn uac2_host_install(config: *const uac2_host_driver_config_t) -> esp_err_t;
    fn uac2_host_device_open(
        config: *const uac2_host_device_config_t,
        out_handle: *mut uac2_host_device_handle_t,
    ) -> esp_err_t;
    fn uac2_host_device_close(dev: uac2_host_device_handle_t) -> esp_err_t;
    fn uac2_host_device_start(
        dev: uac2_host_device_handle_t,
        config: *const uac2_host_stream_config_t,
    ) -> esp_err_t;
    fn uac2_host_get_device_alt_param(
        dev: uac2_host_device_handle_t,
        alt: u8,
        param: *mut uac2_host_dev_alt_param_t,
    ) -> esp_err_t;
    fn uac2_host_device_get_sample_rate_range(
        dev: uac2_host_device_handle_t,
        ranges: *mut uac2_sample_rate_range_t,
        num_ranges: *mut u8,
    ) -> esp_err_t;
    fn uac2_host_device_stop(dev: uac2_host_device_handle_t) -> esp_err_t;
    fn uac2_host_device_write(
        dev: uac2_host_device_handle_t,
        data: *const u8,
        size: u32,
        timeout_ms: u32,
    ) -> esp_err_t;
    fn uac2_host_device_get_start_time(dev: uac2_host_device_handle_t) -> i64;
    fn uac2_host_device_get_feedback(dev: uac2_host_device_handle_t) -> u32;
}
const UAC2_TASK_CORE: BaseType_t = 1;
pub const TX_RING_BUFFER_BYTES: usize = 48 * 1024;
