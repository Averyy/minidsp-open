//! USB Audio Class 2 host wrapper for ESP32-S3.
//!
//! This wraps the external `esp-uac2-host` component and maintains shared
//! playback state independently from the HID device state.

use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use esp_idf_sys::{
    esp_err_t, esp_timer_get_time, BaseType_t, EspError, ESP_ERR_INVALID_ARG, ESP_OK,
};
use log::{debug, error, info, warn};
use minidsp_protocol::playback::{BitDepth, PlayResponse, PlaybackRunStatus, PlaybackStatus};
use minidsp_protocol::rattle::{
    elapsed_samples, RattleResponse, RattleRunState, RattleRunStatus, RattleStatus,
};
use std::ffi::c_void;
use std::fmt::Write as _;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

pub type SharedAudioState = Arc<AudioState>;
static TX_NEED_DATA_COUNT: AtomicU32 = AtomicU32::new(0);

/// Set while a TX stream runs, in RTC memory that panics, watchdogs and
/// software resets don't clear. If the board resets mid-stream with VBUS
/// still up, the 2x4 HD keeps accepting USB audio but stays silent until it
/// reboots; a cable unplug doesn't do this. The next boot reboots the MiniDSP
/// first when it finds the flag set (reproduced 2026-10-30).
#[link_section = ".rtc_noinit"]
static mut STREAM_ACTIVE: u32 = 0;
const STREAM_ACTIVE_MAGIC: u32 = 0x5354_524D;

/// An EN-pin reset (RST button, esptool) clears RTC memory too, so the main
/// task also persists the flag to NVS. Streams start on PSRAM-stack workers,
/// which can't write flash, so they only flip this atomic.
static STREAM_RUNNING: AtomicBool = AtomicBool::new(false);
static STREAM_PERSISTED: AtomicBool = AtomicBool::new(false);
static INTERRUPTED_AT_BOOT: AtomicBool = AtomicBool::new(false);
const STREAM_LIVE_KEY: &str = "stream_live";

fn set_stream_active(active: bool) {
    let value = if active { STREAM_ACTIVE_MAGIC } else { 0 };
    unsafe { ptr::write_volatile(&raw mut STREAM_ACTIVE, value) };
    STREAM_RUNNING.store(active, Ordering::SeqCst);
}

fn settings_nvs(
    partition: &EspDefaultNvsPartition,
) -> Result<EspNvs<NvsDefault>, esp_idf_sys::EspError> {
    EspNvs::new(partition.clone(), "settings", true)
}

/// Reads both copies once at boot, before the first device activation.
pub fn load_stream_flag(partition: &EspDefaultNvsPartition) {
    let stored = settings_nvs(partition)
        .ok()
        .and_then(|nvs| nvs.get_u8(STREAM_LIVE_KEY).ok().flatten())
        .is_some_and(|v| v != 0);
    let rtc = unsafe { ptr::read_volatile(&raw const STREAM_ACTIVE) == STREAM_ACTIVE_MAGIC };
    STREAM_PERSISTED.store(stored, Ordering::SeqCst);
    INTERRUPTED_AT_BOOT.store(stored || rtc, Ordering::SeqCst);
}

/// Whether the previous run reset while a stream was active.
pub fn stream_was_interrupted() -> bool {
    INTERRUPTED_AT_BOOT.load(Ordering::SeqCst)
}

/// Called on the main task once the MiniDSP has been rebooted.
pub fn clear_interrupted_stream(partition: &EspDefaultNvsPartition) {
    INTERRUPTED_AT_BOOT.store(false, Ordering::SeqCst);
    if !STREAM_RUNNING.load(Ordering::SeqCst) {
        unsafe { ptr::write_volatile(&raw mut STREAM_ACTIVE, 0) };
    }
    persist_stream_flag(partition);
}

/// Main task only: NVS writes need its internal-RAM stack. A pending
/// recovery stays persisted until it has run.
pub fn persist_stream_flag(partition: &EspDefaultNvsPartition) {
    let want = STREAM_RUNNING.load(Ordering::SeqCst) || INTERRUPTED_AT_BOOT.load(Ordering::SeqCst);
    if want == STREAM_PERSISTED.load(Ordering::SeqCst) {
        return;
    }
    match settings_nvs(partition).and_then(|nvs| nvs.set_u8(STREAM_LIVE_KEY, u8::from(want))) {
        Ok(()) => STREAM_PERSISTED.store(want, Ordering::SeqCst),
        Err(e) => warn!("USB audio: could not persist the stream flag: {e}"),
    }
}

pub struct AudioState {
    device: Mutex<Option<Arc<Uac2Device>>>,
    last_tx_interface: Mutex<Option<(u8, u8)>>,
    connected_flag: Mutex<bool>,
    pub connected: Condvar,
    pub playing: AtomicBool,
    last_run: Mutex<Option<PlaybackRunStatus>>,
    rattle: Mutex<RattleSessionState>,
    open_gate: Mutex<AudioOpenGate>,
    open_gate_changed: Condvar,
    open_tx: mpsc::Sender<(u8, u8)>,
    cleanup_tx: mpsc::Sender<Arc<Uac2Device>>,
}

#[derive(Default)]
struct RattleSessionState {
    active: bool,
    stop_requested: bool,
    audio_ended: bool,
    output_latency_us: i64,
    last: Option<RattleRunStatus>,
}

#[derive(Debug)]
struct AudioOpenGate {
    enabled: bool,
    cleanup_inflight: usize,
    // The open worker passed the gate and is inside uac2_host_device_open().
    open_inflight: bool,
    // Recovery sequences holding the gate shut. Unlike `enabled`, a plain
    // resume_opening() (e.g. HID reactivation) cannot release these.
    holds: usize,
}

impl AudioOpenGate {
    fn open_allowed(&self) -> bool {
        self.enabled && self.holds == 0 && self.cleanup_inflight == 0
    }
}

/// Keeps the UAC2 open gate shut until dropped; see `AudioState::hold_opening`.
pub struct AudioOpenHold {
    state: SharedAudioState,
}

impl Drop for AudioOpenHold {
    fn drop(&mut self) {
        let mut gate = self.state.open_gate.lock().unwrap();
        gate.holds = gate.holds.saturating_sub(1);
        self.state.open_gate_changed.notify_all();
    }
}

impl AudioState {
    pub fn new() -> SharedAudioState {
        let (open_tx, open_rx) = mpsc::channel::<(u8, u8)>();
        let (cleanup_tx, cleanup_rx) = mpsc::channel::<Arc<Uac2Device>>();
        let state = Arc::new_cyclic(|weak: &std::sync::Weak<AudioState>| {
            if let Err(err) = std::thread::Builder::new()
                .name("uac2_open".into())
                .stack_size(12 * 1024)
                .spawn({
                    let weak = weak.clone();
                    move || {
                        while let Ok((addr, iface_num)) = open_rx.recv() {
                            let Some(state) = weak.upgrade() else {
                                break;
                            };
                            state.begin_open_when_allowed();
                            // The device may have re-enumerated while this
                            // waited at the gate (a MiniDSP reboot), so open
                            // the newest reported interface.
                            let (addr, iface_num) = state
                                .last_tx_interface
                                .lock()
                                .unwrap()
                                .unwrap_or((addr, iface_num));
                            open_tx_device(&state, addr, iface_num);
                            state.finish_open();
                        }
                    }
                })
            {
                error!("failed to spawn UAC2 open worker: {err}");
            }

            Self {
                device: Mutex::new(None),
                last_tx_interface: Mutex::new(None),
                connected_flag: Mutex::new(false),
                connected: Condvar::new(),
                playing: AtomicBool::new(false),
                last_run: Mutex::new(None),
                rattle: Mutex::new(RattleSessionState::default()),
                open_gate: Mutex::new(AudioOpenGate {
                    enabled: false,
                    cleanup_inflight: 0,
                    open_inflight: false,
                    holds: 0,
                }),
                open_gate_changed: Condvar::new(),
                open_tx,
                cleanup_tx,
            }
        });

        if let Err(err) = std::thread::Builder::new()
            .name("uac2_cleanup".into())
            .stack_size(8 * 1024)
            .spawn({
                let weak: std::sync::Weak<AudioState> = Arc::downgrade(&state);
                move || {
                    while let Ok(device) = cleanup_rx.recv() {
                        // Close explicitly instead of relying on the last Arc
                        // drop: other holders (a playback thread, an HTTP
                        // handler) may still have a reference, and cleanup
                        // completion must mean the interface is really closed.
                        device.close_with_retry();
                        drop(device);
                        if let Some(state) = weak.upgrade() {
                            state.finish_cleanup();
                        } else {
                            break;
                        }
                    }
                }
            })
        {
            error!("failed to spawn UAC2 cleanup worker: {err}");
        }

        state
    }

    pub fn current_device(&self) -> Option<Arc<Uac2Device>> {
        self.device.lock().unwrap().clone()
    }

    fn device_by_handle(&self, handle: uac2_host_device_handle_t) -> Option<Arc<Uac2Device>> {
        self.current_device()
            .filter(|device| device.handle_ptr() == handle)
    }

    /// Publish a freshly opened device unless opening was paused while the
    /// open was in flight; then hand the device back so it can be closed.
    fn publish_opened_device(
        &self,
        device: Arc<Uac2Device>,
    ) -> Result<Option<Arc<Uac2Device>>, Arc<Uac2Device>> {
        // Checked under the gate lock so a pause_opening() that returned
        // before this point can never see the device appear afterwards.
        let gate = self.open_gate.lock().unwrap();
        if !gate.enabled || gate.holds != 0 {
            return Err(device);
        }
        Ok(self.replace_device(device))
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
            if let Some(device) = slot.as_ref() {
                device.mark_disconnected();
            }
            *self.connected_flag.lock().unwrap() = false;
            slot.take()
        } else {
            None
        }
    }

    pub fn defer_device_cleanup(&self, device: Arc<Uac2Device>) {
        self.begin_cleanup();
        if let Err(err) = self.cleanup_tx.send(device) {
            error!("UAC2 cleanup queue send failed: {err}");
            self.finish_cleanup();
            drop(err.0);
        }
    }

    pub fn queue_open(&self, addr: u8, iface_num: u8) {
        *self.last_tx_interface.lock().unwrap() = Some((addr, iface_num));
        if let Err(err) = self.open_tx.send((addr, iface_num)) {
            error!("UAC2 open queue send failed: {err}");
        }
    }

    // The busy flag only changes under the last_run lock, so a status
    // snapshot never pairs one run's flag with another run's record.
    pub fn try_begin_playback(&self) -> bool {
        let _last_run = self.last_run.lock().unwrap();
        self.playing
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    pub fn finish_playback(&self) {
        let _last_run = self.last_run.lock().unwrap();
        self.rattle.lock().unwrap().active = false;
        self.playing.store(false, Ordering::SeqCst);
    }

    pub fn is_playing(&self) -> bool {
        self.playing.load(Ordering::SeqCst)
    }

    pub fn playback_status(&self) -> PlaybackStatus {
        let last_run = self.last_run.lock().unwrap();
        PlaybackStatus {
            playing: self.is_playing(),
            last: last_run.clone(),
        }
    }

    pub fn record_run_started(&self, response: &PlayResponse) {
        *self.last_run.lock().unwrap() = Some(PlaybackRunStatus::started(response));
    }

    /// Settle the run recorded by `record_run_started()`: completed, or
    /// failed with `error`.
    pub fn finish_last_run(&self, error: Option<String>) {
        if let Some(run) = self.last_run.lock().unwrap().as_mut() {
            run.finish(error);
        }
    }

    pub fn set_last_run_source_restore_error(&self, error: String) {
        if let Some(run) = self.last_run.lock().unwrap().as_mut() {
            run.source_restore_error = Some(error);
        }
    }

    pub fn record_rattle_started(&self, response: &RattleResponse, first_write_time_us: i64) {
        // Initial URBs carry about 9 ms of silence. Include prefill/write time
        // conservatively; MiniDSP's own output latency remains hardware-specific.
        const SILENCE_URBS_US: i64 = 9_000;
        *self.rattle.lock().unwrap() = RattleSessionState {
            active: true,
            stop_requested: false,
            audio_ended: false,
            output_latency_us: first_write_time_us + SILENCE_URBS_US,
            last: Some(RattleRunStatus::started(response)),
        };
    }

    pub fn rattle_status(&self) -> RattleStatus {
        let session = self.rattle.lock().unwrap();
        let current = session
            .last
            .as_ref()
            .filter(|run| {
                session.active && !session.audio_ended && run.state == RattleRunState::Playing
            })
            .and_then(|run| {
                run.schedule.current_at(elapsed_samples(
                    run.started_at_us,
                    server_time_us(),
                    run.schedule.sample_rate,
                    session.output_latency_us,
                ))
            });
        RattleStatus {
            playing: session.active,
            current,
            last: session.last.clone(),
        }
    }

    pub fn request_rattle_stop(&self) -> bool {
        let mut session = self.rattle.lock().unwrap();
        if !session.active
            || session.audio_ended
            || !session
                .last
                .as_ref()
                .is_some_and(|run| run.state == RattleRunState::Playing)
        {
            return false;
        }
        session.stop_requested = true;
        true
    }

    pub fn rattle_stop_requested(&self) -> bool {
        self.rattle.lock().unwrap().stop_requested
    }

    pub fn mark_rattle_audio_ended(&self) {
        self.rattle.lock().unwrap().audio_ended = true;
    }

    pub fn finish_rattle_run(&self, error: Option<String>, stopped: bool) {
        if let Some(run) = self.rattle.lock().unwrap().last.as_mut() {
            if stopped && error.is_none() {
                run.stopped();
            } else {
                run.finish(error);
            }
        }
    }

    pub fn set_rattle_restore_errors(&self, volume: Option<String>, source: Option<String>) {
        if let Some(run) = self.rattle.lock().unwrap().last.as_mut() {
            run.set_restore_errors(volume, source);
        }
    }

    /// Keep the open gate shut, whatever `resume_opening()` calls happen,
    /// until the returned hold is dropped. Recovery sequences that tear the
    /// audio path down (driver restart, root-port recycle) take one so HID
    /// reactivation mid-sequence cannot reopen the interface under them.
    pub fn hold_opening(self: &Arc<Self>, reason: &str) -> AudioOpenHold {
        let mut gate = self.open_gate.lock().unwrap();
        gate.holds += 1;
        debug!("UAC2 open gate held ({reason})");
        AudioOpenHold {
            state: self.clone(),
        }
    }

    pub fn pause_opening(&self, reason: &str) {
        let mut gate = self.open_gate.lock().unwrap();
        if gate.enabled {
            info!("UAC2 open gate: enabled -> paused ({reason})");
        }
        gate.enabled = false;
    }

    pub fn resume_opening(&self, reason: &str) {
        let mut gate = self.open_gate.lock().unwrap();
        if !gate.enabled {
            info!("UAC2 open gate: paused -> enabled ({reason})");
        }
        gate.enabled = true;
        self.open_gate_changed.notify_all();
    }

    pub fn pause_and_close_current(&self, reason: &str) {
        self.pause_opening(reason);

        let Some(device) = self.current_device() else {
            return;
        };
        let handle = device.handle_ptr();
        if handle.is_null() {
            return;
        }

        info!("UAC2 closing TX interface ({reason})");
        if let Some(device) = self.clear_device_by_handle(handle) {
            self.defer_device_cleanup(device);
        }
    }

    pub fn reopen_last_known(&self, reason: &str) -> bool {
        if self.current_device().is_some() {
            return true;
        }

        let Some((addr, iface_num)) = *self.last_tx_interface.lock().unwrap() else {
            warn!("UAC2 reopen requested without a remembered TX interface ({reason})");
            return false;
        };

        info!("UAC2 requesting TX reopen ({reason})");
        if let Err(err) = self.open_tx.send((addr, iface_num)) {
            error!("UAC2 reopen queue send failed: {err}");
            return false;
        }
        true
    }

    pub fn wait_for_current_device(&self, timeout: Duration) -> Option<Arc<Uac2Device>> {
        if let Some(device) = self.current_device() {
            return Some(device);
        }

        let deadline = Instant::now() + timeout;
        let mut connected = self.connected_flag.lock().unwrap();
        while !*connected {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            let result = self
                .connected
                .wait_timeout(connected, remaining)
                .unwrap_or_else(|e| e.into_inner());
            connected = result.0;
            if result.1.timed_out() {
                break;
            }
        }
        drop(connected);
        self.current_device()
    }

    /// Wait until no close is pending and no open is in flight, so the
    /// interface is really closed (callers pause opening first).
    pub fn wait_for_cleanup_complete(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut gate = self.open_gate.lock().unwrap();
        while gate.cleanup_inflight != 0 || gate.open_inflight {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }

            let result = self
                .open_gate_changed
                .wait_timeout(gate, remaining)
                .unwrap_or_else(|e| e.into_inner());
            gate = result.0;
            if result.1.timed_out() && (gate.cleanup_inflight != 0 || gate.open_inflight) {
                return false;
            }
        }
        true
    }

    fn begin_open_when_allowed(&self) {
        let mut gate = self.open_gate.lock().unwrap();
        while !gate.open_allowed() {
            gate = self
                .open_gate_changed
                .wait(gate)
                .unwrap_or_else(|e| e.into_inner());
        }
        gate.open_inflight = true;
    }

    fn finish_open(&self) {
        let mut gate = self.open_gate.lock().unwrap();
        gate.open_inflight = false;
        self.open_gate_changed.notify_all();
    }

    fn begin_cleanup(&self) {
        let mut gate = self.open_gate.lock().unwrap();
        gate.cleanup_inflight += 1;
    }

    fn finish_cleanup(&self) {
        let mut gate = self.open_gate.lock().unwrap();
        if gate.cleanup_inflight != 0 {
            gate.cleanup_inflight -= 1;
        }
        self.open_gate_changed.notify_all();
    }
}

pub fn install(state: SharedAudioState) -> Result<(), UsbAudioError> {
    let config = uac2_host_driver_config_t {
        create_background_task: true,
        // Keep the UAC2 event/submit task above default pthread priority so
        // USB transfer callbacks are not delayed by normal app threads.
        task_priority: UAC2_TASK_PRIORITY,
        stack_size: 6 * 1024,
        core_id: UAC2_TASK_CORE,
        callback: Some(driver_event_cb),
        callback_arg: Arc::as_ptr(&state) as *mut c_void,
    };

    driver_call("uac2_host_install", unsafe { uac2_host_install(&config) })?;
    info!("UAC2 host driver installed");
    Ok(())
}

pub fn restart(state: SharedAudioState, reason: &str) -> Result<(), UsbAudioError> {
    info!("restarting UAC2 host driver ({reason})");
    driver_call("uac2_host_uninstall", unsafe { uac2_host_uninstall() })?;
    install(state)
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
    Open(String),
    Unsupported(String),
    /// A driver call returned an ESP-IDF error code.
    Driver {
        call: &'static str,
        code: esp_err_t,
    },
    /// The stream started but the driver never reported when its first URB
    /// went out, so there is no valid `startedAtUs` anchor.
    NoStartTime,
}

impl UsbAudioError {
    /// The ESP-IDF error code of a failed driver call, if this is one.
    pub fn esp_code(&self) -> Option<esp_err_t> {
        match self {
            Self::Driver { code, .. } => Some(*code),
            _ => None,
        }
    }

    /// The driver call that failed, if this is a driver error.
    pub fn driver_call(&self) -> Option<&'static str> {
        match self {
            Self::Driver { call, .. } => Some(call),
            _ => None,
        }
    }
}

impl std::fmt::Display for UsbAudioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Open(msg) => write!(f, "{msg}"),
            Self::Unsupported(msg) => write!(f, "{msg}"),
            Self::Driver { call, code } => write!(f, "{call}: {}", esp_err_name(*code)),
            Self::NoStartTime => write!(
                f,
                "USB audio stream did not report a start time (stream stopped or device gone)"
            ),
        }
    }
}

impl std::error::Error for UsbAudioError {}

pub struct Uac2Device {
    // Atomic rather than a Mutex: the driver's device callback reads the
    // handle and must never block behind a close() that is itself waiting
    // on the driver task.
    handle: AtomicPtr<uac2_interface>,
    connected: AtomicBool,
    physically_disconnected: AtomicBool,
    // Serializes close attempts (cleanup worker vs. the Drop fallback).
    close_lock: Mutex<()>,
    stream_errors: AtomicU32,
    transfer_errors: AtomicU32,
}

unsafe impl Send for Uac2Device {}
unsafe impl Sync for Uac2Device {}

impl Uac2Device {
    fn new(handle: uac2_host_device_handle_t) -> Self {
        Self {
            handle: AtomicPtr::new(handle),
            connected: AtomicBool::new(true),
            physically_disconnected: AtomicBool::new(false),
            close_lock: Mutex::new(()),
            stream_errors: AtomicU32::new(0),
            transfer_errors: AtomicU32::new(0),
        }
    }

    pub fn handle_ptr(&self) -> uac2_host_device_handle_t {
        self.handle.load(Ordering::SeqCst)
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst) && !self.handle_ptr().is_null()
    }

    /// Removal from AudioState only means close was queued. It does not
    /// prove that the driver stopped transmitting.
    pub fn is_closed_or_gone(&self) -> bool {
        self.handle_ptr().is_null() || self.physically_disconnected.load(Ordering::SeqCst)
    }

    fn mark_disconnected(&self) {
        self.connected.store(false, Ordering::SeqCst);
    }

    /// Error counters reported by the driver's device callback since the
    /// last `start_stream()`: (stream errors, transfer errors). A stream
    /// error means the driver gave up on the stream.
    pub fn error_counts(&self) -> (u32, u32) {
        (
            self.stream_errors.load(Ordering::SeqCst),
            self.transfer_errors.load(Ordering::SeqCst),
        )
    }

    pub fn start_stream(&self, sample_rate: u32, bit_depth: BitDepth) -> Result<(), UsbAudioError> {
        self.validate_stream(sample_rate, bit_depth)?;
        let config = uac2_host_stream_config_t {
            channels: 2,
            bit_resolution: bit_depth.bits(),
            sample_freq: sample_rate,
            flags: 0,
        };
        self.stream_errors.store(0, Ordering::SeqCst);
        self.transfer_errors.store(0, Ordering::SeqCst);
        // Set before starting, so a reset during startup counts as well.
        set_stream_active(true);
        let result = driver_call("uac2_host_device_start", unsafe {
            uac2_host_device_start(self.handle_ptr(), &config)
        });
        if result.is_err() {
            set_stream_active(false);
        }
        result
    }

    fn validate_stream(&self, sample_rate: u32, bit_depth: BitDepth) -> Result<(), UsbAudioError> {
        const MAX_ALT_SETTINGS: u8 = 8;
        const MAX_SAMPLE_RATE_RANGES: usize = 16;

        // The driver streams on the first alt whose channel count and bit
        // resolution match, so that is the alt whose subslot size decides
        // whether our packed PCM frames line up.
        let mut selected_alt: Option<uac2_host_dev_alt_param_t> = None;
        let mut alt_descriptions = String::new();

        // Alt numbers need not be contiguous (and the driver may reject an
        // alt's descriptors), so probe every slot rather than stopping at
        // the first gap.
        for alt in 1..=MAX_ALT_SETTINGS {
            let mut param = uac2_host_dev_alt_param_t::default();
            let err = unsafe { uac2_host_get_device_alt_param(self.handle_ptr(), alt, &mut param) };
            if err != ESP_OK as esp_err_t {
                continue;
            }

            if !alt_descriptions.is_empty() {
                alt_descriptions.push_str(", ");
            }
            let _ = write!(
                alt_descriptions,
                "alt {}: {}ch/{}-bit in {}-byte slots",
                param.alt_setting, param.channels, param.bit_resolution, param.sub_slot_size
            );

            if selected_alt.is_none()
                && param.channels == 2
                && param.bit_resolution == bit_depth.bits()
            {
                selected_alt = Some(param);
            }
        }

        let Some(selected_alt) = selected_alt else {
            let supported = if alt_descriptions.is_empty() {
                "no matching alternate settings discovered".to_string()
            } else {
                format!("supported formats: {alt_descriptions}")
            };
            return Err(UsbAudioError::Unsupported(format!(
                "USB audio device does not support 2-channel {}-bit playback ({supported})",
                bit_depth.bits()
            )));
        };

        if usize::from(selected_alt.sub_slot_size) != bit_depth.bytes_per_sample() {
            return Err(UsbAudioError::Unsupported(format!(
                "USB audio alt {} carries {}-bit audio in {}-byte slots, but playback PCM is packed in {}-byte slots",
                selected_alt.alt_setting,
                bit_depth.bits(),
                selected_alt.sub_slot_size,
                bit_depth.bytes_per_sample()
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
        driver_call("uac2_host_device_write", unsafe {
            uac2_host_device_write(
                self.handle_ptr(),
                data.as_ptr(),
                data.len() as u32,
                timeout_ms,
            )
        })
    }

    pub fn start_time_us(&self) -> i64 {
        unsafe { uac2_host_device_get_start_time(self.handle_ptr()) }
    }

    pub fn feedback_raw(&self) -> u32 {
        unsafe { uac2_host_device_get_feedback(self.handle_ptr()) }
    }

    pub fn stop_stream(&self) -> Result<(), UsbAudioError> {
        let result = driver_call("uac2_host_device_stop", unsafe {
            uac2_host_device_stop(self.handle_ptr())
        });
        set_stream_active(false);
        result
    }

    /// One close attempt. `uac2_host_device_close()` stops any active
    /// stream itself. On failure the driver keeps the interface open and the
    /// handle valid, so the handle is kept for a retry; it is only dropped on
    /// success or `ESP_ERR_INVALID_ARG` (the driver no longer knows it).
    fn close(&self) -> Result<(), UsbAudioError> {
        let _close = self.close_lock.lock().unwrap_or_else(|e| e.into_inner());
        let handle = self.handle_ptr();
        // Closing stops any stream; a disconnect also lands here.
        set_stream_active(false);
        if handle.is_null() {
            return Ok(());
        }

        let err = unsafe { uac2_host_device_close(handle) };
        if err == ESP_OK as esp_err_t || err == ESP_ERR_INVALID_ARG as esp_err_t {
            self.connected.store(false, Ordering::SeqCst);
            self.handle.store(ptr::null_mut(), Ordering::SeqCst);
            return Ok(());
        }
        Err(UsbAudioError::Driver {
            call: "uac2_host_device_close",
            code: err,
        })
    }

    /// Close, retrying until the driver lets go of the interface. A leaked
    /// interface blocks every later open and `uac2_host_uninstall()` until
    /// reboot, so this never gives up; an unplug or root-port recycle makes
    /// a stuck close succeed.
    fn close_with_retry(&self) {
        let mut attempt: u32 = 0;
        loop {
            attempt += 1;
            match self.close() {
                Ok(()) => {
                    if attempt > 1 {
                        info!("UAC2 interface closed after {attempt} attempts");
                    }
                    return;
                }
                Err(err) => {
                    if attempt <= 3 || attempt.is_multiple_of(CLOSE_RETRY_FAST_ATTEMPTS) {
                        warn!("UAC2 close failed (attempt {attempt}): {err}; retrying");
                    }
                    let delay = if attempt < CLOSE_RETRY_FAST_ATTEMPTS {
                        CLOSE_RETRY_FAST_DELAY
                    } else {
                        CLOSE_RETRY_SLOW_DELAY
                    };
                    std::thread::sleep(delay);
                }
            }
        }
    }
}

impl Drop for Uac2Device {
    fn drop(&mut self) {
        // Every device leaves AudioState through the cleanup worker, which
        // closes it with retries; this is only a last-resort fallback.
        if self.handle_ptr().is_null() {
            return;
        }
        if let Err(err) = self.close() {
            error!("UAC2 interface leaked on drop: {err}");
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
    state.queue_open(addr, iface_num);
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
                device.physically_disconnected.store(true, Ordering::SeqCst);
                state.defer_device_cleanup(device);
            }
        }
        uac2_host_device_event_t::UAC2_HOST_DEVICE_EVENT_TRANSFER_ERROR => {
            if let Some(device) = state.device_by_handle(dev) {
                device.transfer_errors.fetch_add(1, Ordering::SeqCst);
            }
            warn!("UAC2 transfer error");
        }
        uac2_host_device_event_t::UAC2_HOST_DEVICE_EVENT_STREAM_ERROR => {
            if let Some(device) = state.device_by_handle(dev) {
                device.stream_errors.fetch_add(1, Ordering::SeqCst);
            }
            warn!("UAC2 stream error");
        }
        uac2_host_device_event_t::UAC2_HOST_DEVICE_EVENT_TX_DONE => {
            TX_NEED_DATA_COUNT.fetch_add(1, Ordering::SeqCst);
            debug!("UAC2 TX needs more data");
        }
        uac2_host_device_event_t::UAC2_HOST_DEVICE_EVENT_RX_DONE => {}
    }
}

fn open_tx_device(state: &SharedAudioState, addr: u8, iface_num: u8) {
    if state.current_device().is_some() {
        debug!("UAC2 open skipped: TX device already present");
        return;
    }

    let config = uac2_host_device_config_t {
        addr,
        iface_num,
        buffer_size: TX_RING_BUFFER_BYTES as u32,
        buffer_threshold: 0,
        callback: Some(device_event_cb),
        callback_arg: Arc::as_ptr(state) as *mut c_void,
    };
    let mut handle: uac2_host_device_handle_t = ptr::null_mut();

    let result = driver_call("uac2_host_device_open", unsafe {
        uac2_host_device_open(&config, &mut handle)
    });
    match result {
        Ok(()) => {
            let device = Arc::new(Uac2Device::new(handle));
            match state.publish_opened_device(device) {
                Ok(previous) => {
                    if let Some(previous) = previous {
                        state.defer_device_cleanup(previous);
                    }
                    info!("UAC2 TX interface opened: addr={addr} iface={iface_num}");
                }
                Err(device) => {
                    // Queued for close before finish_open() clears
                    // open_inflight, so waiters never see a gap.
                    info!("UAC2 TX interface opened after opening was paused; closing it");
                    state.defer_device_cleanup(device);
                    // Re-queue the request so it waits at the gate like any
                    // open that arrives while paused; resume_opening() alone
                    // (e.g. after HID recovery) relies on that.
                    state.queue_open(addr, iface_num);
                }
            }
        }
        Err(err) => {
            let open_err = UsbAudioError::Open(format!(
                "open UAC2 TX interface addr={addr} iface={iface_num}: {err}"
            ));
            error!("{open_err}");
        }
    }
}

fn clone_audio_state(arg: *mut c_void) -> SharedAudioState {
    let ptr = arg as *const AudioState;
    unsafe {
        Arc::increment_strong_count(ptr);
        Arc::from_raw(ptr)
    }
}

fn driver_call(call: &'static str, err: esp_err_t) -> Result<(), UsbAudioError> {
    if err == ESP_OK as esp_err_t {
        Ok(())
    } else {
        Err(UsbAudioError::Driver { call, code: err })
    }
}

fn esp_err_name(err: esp_err_t) -> String {
    let name = EspError::from(err)
        .map(|e| e.to_string())
        .unwrap_or_else(|| "UNKNOWN".into());
    format!("{name} (0x{err:04x})")
}

fn sample_rate_in_range(sample_rate: u32, range: uac2_sample_rate_range_t) -> bool {
    if range.min == range.max {
        return sample_rate == range.min;
    }
    if sample_rate < range.min || sample_rate > range.max {
        return false;
    }
    // A non-zero resolution makes the range a set of discrete steps.
    range.res == 0 || (sample_rate - range.min).is_multiple_of(range.res)
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
#[derive(Clone, Copy, Default)]
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
    fn uac2_host_uninstall() -> esp_err_t;
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
const UAC2_TASK_PRIORITY: usize = 20;
// Keep the playback ring buffer aligned to the chunk sizes we use in practice.
// 46,080 bytes is an exact multiple of both the older 10 ms writes and the
// current shorter 2 ms cadence, so steady-state writes never straddle the
// internal BYTEBUF wrap point.
pub const TX_RING_BUFFER_BYTES: usize = 46_080;
/// Audio the driver keeps in submitted URBs beyond the ring buffer:
/// CONFIG_UAC2_NUM_ISOC_URBS (3) x CONFIG_UAC2_NUM_PACKETS_PER_URB (3) x 1 ms.
pub const TX_URB_PIPELINE: Duration = Duration::from_millis(9);
const CLOSE_RETRY_FAST_ATTEMPTS: u32 = 20;
const CLOSE_RETRY_FAST_DELAY: Duration = Duration::from_millis(500);
const CLOSE_RETRY_SLOW_DELAY: Duration = Duration::from_secs(2);

/// Upper bound on how long audio already accepted by `write()` takes to play
/// out: a full ring buffer at `byte_rate` plus the URB pipeline. Stopping
/// the stream sooner discards the tail.
pub fn tx_drain_time(byte_rate: usize) -> Duration {
    if byte_rate == 0 {
        return TX_URB_PIPELINE;
    }
    let ring_us = (TX_RING_BUFFER_BYTES as u64 * 1_000_000).div_ceil(byte_rate as u64);
    Duration::from_micros(ring_us) + TX_URB_PIPELINE
}

#[cfg(test)]
mod tests {
    use super::{sample_rate_in_range, tx_drain_time, uac2_sample_rate_range_t, AudioState};
    use std::time::Duration;

    #[test]
    fn stepped_sample_rate_range_rejects_off_step_rates() {
        let range = uac2_sample_rate_range_t {
            min: 44_100,
            max: 96_000,
            res: 3_900,
        };
        assert!(sample_rate_in_range(44_100, range));
        assert!(sample_rate_in_range(48_000, range));
        assert!(!sample_rate_in_range(50_000, range));
        assert!(!sample_rate_in_range(96_001, range));

        let continuous = uac2_sample_rate_range_t { res: 0, ..range };
        assert!(sample_rate_in_range(50_000, continuous));
    }

    #[test]
    fn tx_drain_time_covers_full_ring_buffer() {
        // 48 kHz, 16-bit stereo: 46,080 bytes is 240 ms, plus 9 ms in flight.
        assert_eq!(tx_drain_time(48_000 * 4), Duration::from_millis(249));
    }

    #[test]
    fn wait_for_cleanup_complete_returns_after_cleanup_finishes() {
        let state = AudioState::new();
        state.begin_cleanup();

        let state_for_thread = state.clone();
        let join = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            state_for_thread.finish_cleanup();
        });

        assert!(state.wait_for_cleanup_complete(Duration::from_millis(500)));
        join.join().expect("cleanup thread should finish");
    }

    #[test]
    fn wait_for_cleanup_complete_times_out_when_cleanup_never_finishes() {
        let state = AudioState::new();
        state.begin_cleanup();

        assert!(!state.wait_for_cleanup_complete(Duration::from_millis(50)));
        state.finish_cleanup();
    }
}
