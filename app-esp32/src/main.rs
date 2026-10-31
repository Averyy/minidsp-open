//! minidsp-open ESP32-S3 firmware
//!
//! USB Host HID -> MiniDSP protocol -> WiFi HTTP API.
//! Blocking architecture on ESP-IDF std.
//!
//! Boot sequence:
//! 1. Ring buffer logger initialization
//! 2. NVS + WiFi (STA or SoftAP provisioning)
//! 3. USB Host HID (blocks up to 30s for device)
//! 4. Device probe + state
//! 5. mDNS advertisement
//! 6. HTTP server (+ WebSocket if configured)
//! 7. WiFi monitor thread (auto-reconnect)
//! 8. Main loop: diagnostics tick + USB reconnect

use esp_idf_svc::sys;
use log::{error, info, warn};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[global_allocator]
static ALLOCATOR: psram::Allocator = psram::Allocator;

/// Main-loop heartbeat for the stall supervisor, in seconds since boot.
static MAIN_LOOP_ALIVE: AtomicU32 = AtomicU32::new(0);
/// When the main loop entered device-loss cleanup; 0 when it isn't in it.
static USB_CLEANUP_SINCE: AtomicU32 = AtomicU32::new(0);
/// Discovery plus interrupted-stream recovery can hold one pass about three minutes.
const MAIN_LOOP_STALL_SECS: u32 = 300;
/// Cleanup normally finishes in under ten seconds.
const USB_CLEANUP_STALL_SECS: u32 = 30;
/// A healthy endpoint halt finishes within a frame or two.
const HALT_DUMP_AFTER_MS: u32 = 2_000;

fn uptime_secs() -> u32 {
    (unsafe { sys::esp_timer_get_time() } / 1_000_000) as u32
}

fn current_stall() -> Option<(&'static str, u32)> {
    let now = uptime_secs();
    let cleanup_since = USB_CLEANUP_SINCE.load(Ordering::Relaxed);
    if cleanup_since != 0 && now - cleanup_since > USB_CLEANUP_STALL_SECS {
        return Some(("USB cleanup", now - cleanup_since));
    }
    let alive = MAIN_LOOP_ALIVE.load(Ordering::Relaxed);
    (now - alive > MAIN_LOOP_STALL_SECS).then_some(("the main loop", now - alive))
}

/// Log the controller registers once per endpoint halt that outlives a healthy one.
fn dump_pending_halt(dumped: &mut u32) {
    let Some(started) = usb_hid::halt_started_ms() else {
        return;
    };
    let now_ms = (unsafe { sys::esp_timer_get_time() } / 1000) as u32;
    if started != *dumped && now_ms.wrapping_sub(started) >= HALT_DUMP_AFTER_MS {
        *dumped = started;
        usb_dwc::log_controller_state("halt pending");
    }
}

/// A USB host call that never returns (a channel halt whose interrupt never
/// comes) leaves its task holding the HID bus lock, and the main loop then
/// waits on that lock forever. Restart instead of hanging; the boot flags
/// cover a stream it interrupts.
fn start_stall_supervisor() -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("stall_sup".into())
        .stack_size(4 * 1024)
        .spawn(|| {
            let mut halt_dumped = 0;
            loop {
                std::thread::sleep(Duration::from_secs(5));
                dump_pending_halt(&mut halt_dumped);
                if let Some((what, secs)) = current_stall() {
                    error!("{what} stalled for {secs} s, likely on a USB host call that never returned; restarting");
                    std::thread::sleep(Duration::from_millis(200));
                    unsafe { sys::esp_restart() };
                }
            }
        })?;
    Ok(())
}

mod auth;
mod device;
mod diagnostics;
mod ezbeq;
mod http_server;
mod led;
mod ota;
mod psram;
mod settings;
mod snapshot;
mod storage;
mod threads;
mod updater;
mod usb_audio;
mod usb_dwc;
mod usb_hid;
mod wifi;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UsbState {
    Absent,
    ProtocolReady,
}

fn reset_reason_label(reason: sys::esp_reset_reason_t) -> &'static str {
    match reason {
        sys::esp_reset_reason_t_ESP_RST_UNKNOWN => "unknown",
        sys::esp_reset_reason_t_ESP_RST_POWERON => "power-on",
        sys::esp_reset_reason_t_ESP_RST_EXT => "external-pin",
        sys::esp_reset_reason_t_ESP_RST_SW => "software",
        sys::esp_reset_reason_t_ESP_RST_PANIC => "panic",
        sys::esp_reset_reason_t_ESP_RST_INT_WDT => "interrupt-watchdog",
        sys::esp_reset_reason_t_ESP_RST_TASK_WDT => "task-watchdog",
        sys::esp_reset_reason_t_ESP_RST_WDT => "watchdog",
        sys::esp_reset_reason_t_ESP_RST_DEEPSLEEP => "deep-sleep",
        sys::esp_reset_reason_t_ESP_RST_BROWNOUT => "brownout",
        sys::esp_reset_reason_t_ESP_RST_SDIO => "sdio",
        sys::esp_reset_reason_t_ESP_RST_USB => "usb",
        sys::esp_reset_reason_t_ESP_RST_JTAG => "jtag",
        sys::esp_reset_reason_t_ESP_RST_EFUSE => "efuse",
        sys::esp_reset_reason_t_ESP_RST_PWR_GLITCH => "power-glitch",
        sys::esp_reset_reason_t_ESP_RST_CPU_LOCKUP => "cpu-lockup",
        _ => "other",
    }
}

fn main() {
    sys::link_patches();

    // Step 0: Initialize ring buffer logger (before EspLogger so we capture everything)
    diagnostics::init_logger();

    match led::start_global_controller() {
        Ok(controller) => {
            controller.set_mode(led::LedMode::Booting);
            controller.set_device_ready(false);
        }
        Err(e) => warn!("LED init failed: {e}"),
    }

    let reset_reason = unsafe { sys::esp_reset_reason() };
    info!(
        "minidsp-open v{} ({}) starting (reset reason: {} / {})",
        ota::FIRMWARE_VERSION,
        ota::FIRMWARE_BUILD,
        reset_reason as i32,
        reset_reason_label(reset_reason)
    );

    // Initialize NVS (required for WiFi credentials)
    let nvs = esp_idf_svc::nvs::EspDefaultNvsPartition::take().expect("failed to initialize NVS");
    settings::load(&nvs);
    usb_audio::load_stream_flag(&nvs);

    // Mount LittleFS data partition (ezBEQ catalogue, UI, config)
    match storage::mount() {
        Ok(info) => info!(
            "storage: {:.1}MB total, {:.1}KB used",
            info.total_bytes as f64 / (1024.0 * 1024.0),
            info.used_bytes as f64 / 1024.0,
        ),
        Err(e) => error!("LittleFS mount failed: {e} — data partition unavailable"),
    }

    // Step 1: WiFi connection (or provisioning)
    diagnostics::log_internal_heap("before WiFi");
    info!("step 1: WiFi setup");
    led::set_mode(led::LedMode::WifiConnecting);
    let wifi_handle = match wifi::connect_or_provision(nvs.clone()) {
        Ok(handle) => {
            if handle.is_sta {
                info!("WiFi connected (STA mode)");
                led::set_mode(led::LedMode::Operational);
            } else {
                info!("WiFi in SoftAP provisioning mode");
                led::set_mode(led::LedMode::ProvisioningAp);
            }
            Some(handle)
        }
        Err(e) => {
            error!("WiFi failed: {e} — continuing without network");
            led::set_mode(led::LedMode::Fatal);
            None
        }
    };

    auth::network_ready();

    let wifi_is_sta = wifi_handle.as_ref().is_some_and(|h| h.is_sta);

    // A freshly OTA-updated image must prove it's reachable or roll back.
    if let Err(e) = ota::start_health_check(wifi::has_saved_credentials(&nvs)) {
        error!("failed to spawn OTA health check: {e}");
    }

    // Shared device state (None until USB device is probed)
    let device_state: http_server::SharedDeviceState = Arc::new(Mutex::new(None));
    let audio_state = usb_audio::AudioState::new();

    // Shared ezBEQ slot state (initialized when device is probed, used in STA mode only)
    let beq_state: ezbeq::SharedEzBeqState = Arc::new(Mutex::new(
        ezbeq::EzBeqState::new(2, 4), // Default 2-in/4-out, updated on device probe
    ));

    // In SoftAP mode, start captive portal BEFORE USB init.
    // Phone captive portal detection has a ~5s timeout — USB blocks up to 30s.
    if !wifi_is_sta && wifi_handle.is_some() {
        let ap_ip = wifi::get_softap_ip();
        info!("step 2: starting captive portal at {ap_ip}");
        wifi::configure_dhcp_for_captive_portal(ap_ip);
        wifi::start_captive_dns(ap_ip);
        match http_server::start(
            device_state.clone(),
            audio_state.clone(),
            80,
            nvs.clone(),
            Some(ap_ip),
            None,
        ) {
            Ok(server) => {
                info!("captive portal running on port 80");
                Box::leak(Box::new(server));
                ota::mark_http_ready();
            }
            Err(e) => error!("captive portal HTTP failed: {e}"),
        }
    }

    // Step 3: USB Host HID initialization (sets up host library + background threads)
    diagnostics::log_internal_heap("after WiFi");
    info!("step 3: USB Host HID setup");
    let bus = match usb_hid::UsbHidBus::init() {
        Ok(bus) => Some(Arc::new(bus)),
        Err(e) => {
            error!("USB HID init failed: {e} — USB will not be available");
            led::set_mode(led::LedMode::Fatal);
            None
        }
    };
    // Step 4: Wait for MiniDSP device (up to 30s)
    let mut usb_state = UsbState::Absent;
    let mut audio_driver_installed = false;
    if bus.is_some() {
        // Register the UAC2 client before the first HID attach/probe pass so
        // a MiniDSP already present at boot still delivers the audio connect
        // callback without needing a second physical reconnect.
        ensure_audio_driver_installed(&audio_state, &mut audio_driver_installed);
    }
    if let Some(ref bus) = bus {
        usb_hid::register_global_bus(bus.clone());
        match discover_and_activate_device(bus, Duration::from_secs(30))
            .and_then(|state| recover_interrupted_stream(bus, state, &nvs))
        {
            Ok(state) => {
                usb_state = UsbState::ProtocolReady;
                on_device_activated(state, &device_state, &beq_state, &nvs);
                ensure_audio_driver_installed(&audio_state, &mut audio_driver_installed);
                audio_state.resume_opening("initial HID attach complete");
                led::set_device_ready(true);
            }
            Err(e) => {
                usb_state = UsbState::Absent;
                warn!("No MiniDSP device found: {e} — will retry in background");
                audio_state.pause_opening("initial HID attach failed");
                led::set_device_ready(false);
            }
        }
    }

    // Step 5: mDNS advertisement (only in STA mode with a device)
    #[cfg(any(esp_idf_comp_mdns_enabled, esp_idf_comp_espressif__mdns_enabled))]
    let _mdns = if wifi_is_sta {
        start_mdns(&device_state)
    } else {
        None
    };
    #[cfg(not(any(esp_idf_comp_mdns_enabled, esp_idf_comp_espressif__mdns_enabled)))]
    if wifi_is_sta {
        warn!("mDNS not available (component not enabled in ESP-IDF build)");
    }

    // Step 6: Start HTTP server (STA mode — SoftAP portal already started above)
    if wifi_is_sta && wifi_handle.is_some() {
        diagnostics::log_internal_heap("after USB");
        info!("step 6: starting HTTP server on port 5380");
        match http_server::start(
            device_state.clone(),
            audio_state.clone(),
            5380,
            nvs.clone(),
            None,
            Some(beq_state.clone()),
        ) {
            Ok(server) => {
                info!("HTTP server running on port 5380");
                // Handlers build large structures from small allocations.
                if !psram::prefer_for_task(c"httpd") {
                    warn!("HTTP handlers allocate in internal RAM: no httpd task to move to PSRAM");
                }
                Box::leak(Box::new(server));
                ota::mark_http_ready();
            }
            Err(e) => error!("HTTP server failed: {e}"),
        }
    }

    diagnostics::log_internal_heap("after HTTP");

    // Step 7: TCP bridge (port 5333) — Device Console compatibility
    if wifi_is_sta {
        let tcp_state = device_state.clone();
        let tcp_audio = audio_state.clone();
        if let Err(e) = std::thread::Builder::new()
            .name("tcp_brg".into())
            .stack_size(16384)
            .spawn(move || {
                tcp_bridge(tcp_state, tcp_audio);
            })
        {
            error!("failed to spawn TCP bridge: {e}");
        }
    }

    // Step 8: UDP discovery broadcast (port 3999)
    if wifi_is_sta {
        let udp_state = device_state.clone();
        // Sockets only, never flash: its stack can live in PSRAM.
        if let Err(e) = threads::spawn_psram_thread(c"udp_disc", "udp_disc", 8192, move || {
            udp_discovery(udp_state);
        }) {
            error!("failed to spawn UDP discovery: {e}");
        }
    }

    // Step 9: Start WiFi monitor thread (auto-reconnect on disconnect)
    if let Some(handle) = wifi_handle {
        if handle.is_sta {
            let wifi_mutex = Arc::new(std::sync::Mutex::new(handle.wifi));
            if let Err(e) = wifi::start_monitor(wifi_mutex, nvs.clone()) {
                error!("failed to spawn WiFi monitor thread: {e}");
            }

            // The WiFi monitor is required for reconnect stability; start the
            // catalogue updater only after the monitor has claimed its task.
            if let Err(e) = updater::start_background_updater(nvs.clone()) {
                error!("failed to spawn update thread: {e}");
            }
            if let Err(e) = ota::start_background_checker() {
                error!("failed to spawn firmware check thread: {e}");
            }
        } else {
            // SoftAP mode — keep the wifi handle alive but no monitor
            Box::leak(Box::new(handle.wifi));
        }
    }

    diagnostics::log_internal_heap("ready");
    diagnostics::capture_boot_task_names();
    diagnostics::log_task_stacks();
    info!("minidsp-open ready");

    auth::recovery_ready();
    // Main loop: diagnostics tick + USB device detection/reconnection
    let mut usb_backoff = Duration::from_secs(5);
    let mut next_diagnostics = Instant::now();
    let mut next_usb_scan = Instant::now();
    MAIN_LOOP_ALIVE.store(uptime_secs(), Ordering::Relaxed);
    if let Err(e) = start_stall_supervisor() {
        error!("failed to spawn the stall supervisor: {e}");
    }
    loop {
        MAIN_LOOP_ALIVE.store(uptime_secs(), Ordering::Relaxed);
        auth::maintenance();
        usb_audio::persist_stream_flag(&nvs);
        if Instant::now() >= next_diagnostics {
            diagnostics::tick();
            next_diagnostics = Instant::now() + Duration::from_secs(5);
        }

        // Check for USB device — handles both initial detection and reconnection
        if let Some(ref bus) = bus {
            let has_device = device_state.lock().unwrap().is_some();

            if has_device && !bus.is_connected() {
                // Device was connected but is now gone
                warn!("USB device lost — clearing device state");
                USB_CLEANUP_SINCE.store(uptime_secs().max(1), Ordering::Relaxed);
                bus.cleanup_after_expected_disconnect("main loop device loss");
                USB_CLEANUP_SINCE.store(0, Ordering::Relaxed);
                *device_state.lock().unwrap() = None;
                usb_state = UsbState::Absent;
                audio_state.pause_opening("HID session lost");
                led::set_device_ready(false);
                next_usb_scan = Instant::now();
            }

            // Scan if we don't have a working device (regardless of is_connected,
            // since non-MiniDSP devices can set the flag)
            if device_state.lock().unwrap().is_none() && Instant::now() >= next_usb_scan {
                log_usb_state_transition(&mut usb_state, UsbState::Absent, "device missing");
                info!("USB: scanning for device (timeout: {usb_backoff:?})");
                let scan_started = Instant::now();
                // The device state lives as long as the connection; keep it,
                // and the NVS shadow load's temporaries, out of internal RAM.
                match psram::external(|| {
                    discover_and_activate_device(bus, usb_backoff)
                        .and_then(|state| recover_interrupted_stream(bus, state, &nvs))
                }) {
                    Ok(state) => {
                        usb_backoff = Duration::from_secs(5);
                        next_usb_scan = Instant::now();
                        usb_state = UsbState::ProtocolReady;
                        psram::external(|| {
                            on_device_activated(state, &device_state, &beq_state, &nvs)
                        });
                        ensure_audio_driver_installed(&audio_state, &mut audio_driver_installed);
                        audio_state.resume_opening("HID protocol ready");
                        led::set_device_ready(true);
                    }
                    Err(_) => {
                        // No device found within timeout — back off
                        audio_state.pause_opening("HID scan failed");
                        led::set_device_ready(false);
                        let retry_wait = usb_backoff.saturating_sub(scan_started.elapsed());
                        next_usb_scan = Instant::now() + retry_wait;
                        usb_backoff = (usb_backoff * 2).min(Duration::from_secs(60));
                    }
                }
            }
        }

        std::thread::sleep(Duration::from_millis(250));
    }
}

fn ensure_audio_driver_installed(audio_state: &usb_audio::SharedAudioState, installed: &mut bool) {
    if *installed {
        return;
    }

    match usb_audio::install(audio_state.clone()) {
        Ok(()) => *installed = true,
        Err(e) => error!("UAC2 audio init failed: {e} — audio playback unavailable"),
    }
}

fn probe_error_is_root_port_recoverable(err: &usb_hid::UsbHidError) -> bool {
    match err {
        usb_hid::UsbHidError::Timeout | usb_hid::UsbHidError::Transfer(_) => true,
        usb_hid::UsbHidError::Init(message) => {
            message.contains("interface_claim") || message.contains("alloc xfer")
        }
        _ => false,
    }
}

/// If the previous run reset mid-stream, the MiniDSP may be accepting USB
/// audio but silent (see `usb_audio::stream_was_interrupted`). Its own reboot
/// clears that, so reboot it before publishing it or opening audio.
fn recover_interrupted_stream(
    bus: &Arc<usb_hid::UsbHidBus>,
    state: device::DeviceState,
    nvs: &esp_idf_svc::nvs::EspDefaultNvsPartition,
) -> Result<device::DeviceState, usb_hid::UsbHidError> {
    if !usb_audio::stream_was_interrupted() {
        return Ok(state);
    }
    warn!("USB audio: the previous run reset mid-stream; rebooting the MiniDSP to clear a silent output");
    if let Err(e) = state.reboot_device() {
        warn!("USB audio: MiniDSP reboot after an interrupted stream failed: {e}");
        return Ok(state);
    }
    usb_audio::clear_interrupted_stream(nvs);
    drop(state);
    // Let the reboot's disconnect land before looking for the device again.
    let deadline = Instant::now() + Duration::from_secs(10);
    while bus.is_connected() && Instant::now() < deadline {
        auth::responsive_sleep(Duration::from_millis(100));
    }
    discover_and_activate_device(bus, Duration::from_secs(60))
}

fn discover_and_activate_device(
    bus: &Arc<usb_hid::UsbHidBus>,
    timeout: Duration,
) -> Result<device::DeviceState, usb_hid::UsbHidError> {
    const ATTACH_SETTLE: Duration = Duration::from_secs(10);

    info!("USB state: waiting for HID attach");
    bus.wait_for_device(timeout)?;
    info!("USB state: HID session ready");
    if let Some(age) = bus.last_attach_age() {
        if age < ATTACH_SETTLE {
            let wait = ATTACH_SETTLE - age;
            info!("USB state: waiting {wait:?} for device firmware warmup after attach");
            auth::responsive_sleep(wait);
        }
    }

    match probe_device_with_retries(bus.clone(), Duration::from_secs(20)) {
        Ok(state) => Ok(state),
        Err(err)
            if probe_error_is_root_port_recoverable(&err)
                && bus.has_present_device().unwrap_or(false) =>
        {
            warn!(
                "USB state: probe failed while a device is still present ({err}); recycling root port"
            );
            bus.recycle_root_port("probe recovery")?;
            info!("USB state: waiting for HID attach after root-port recycle");
            bus.wait_for_device(timeout)?;
            if let Some(age) = bus.last_attach_age() {
                if age < ATTACH_SETTLE {
                    let wait = ATTACH_SETTLE - age;
                    info!(
                        "USB state: waiting {wait:?} for device firmware warmup after root-port recycle"
                    );
                    auth::responsive_sleep(wait);
                }
            }
            probe_device_with_retries(bus.clone(), Duration::from_secs(20))
        }
        Err(err) => Err(err),
    }
}

fn probe_device_with_retries(
    bus: Arc<usb_hid::UsbHidBus>,
    warmup_window: Duration,
) -> Result<device::DeviceState, usb_hid::UsbHidError> {
    const PROBE_COMMAND_TIMEOUT: Duration = Duration::from_secs(20);
    const RETRY_DELAY: Duration = Duration::from_millis(500);

    let deadline = Instant::now() + warmup_window;
    let mut attempt = 0usize;

    let final_err = loop {
        attempt += 1;
        info!("USB state: probing protocol (attempt {attempt})");
        let err = match device::DeviceState::probe_with_timeout(bus.clone(), PROBE_COMMAND_TIMEOUT)
        {
            Ok(state) => {
                info!("USB state: protocol ready after {attempt} attempt(s)");
                return Ok(state);
            }
            Err(usb_hid::UsbHidError::Protocol(msg)) => {
                warn!("USB state: protocol not ready on attempt {attempt}: {msg}");
                usb_hid::UsbHidError::Protocol(msg)
            }
            Err(usb_hid::UsbHidError::Timeout) => {
                warn!("USB state: timeout during probe attempt {attempt}");
                usb_hid::UsbHidError::Timeout
            }
            Err(usb_hid::UsbHidError::Transfer(msg)) => {
                warn!("USB state: transport failure during probe attempt {attempt}: {msg}");
                if bus.is_connected() {
                    match bus.reopen_present_device() {
                        Ok(Some((_vid, _pid))) => info!("USB state: reopened HID session"),
                        Ok(None) => warn!("USB state: no present device found while reopening HID"),
                        Err(err) => warn!("USB state: HID reopen failed: {err}"),
                    }
                } else {
                    return Err(usb_hid::UsbHidError::Disconnected);
                }
                usb_hid::UsbHidError::Transfer(msg)
            }
            Err(usb_hid::UsbHidError::Disconnected) => {
                warn!("USB state: device disconnected during probe");
                return Err(usb_hid::UsbHidError::Disconnected);
            }
            Err(err) => return Err(err),
        };

        if Instant::now() >= deadline {
            break err;
        }
        if !(bus.is_connected() || bus.has_present_device().unwrap_or(false)) {
            return Err(usb_hid::UsbHidError::Disconnected);
        }
        auth::responsive_sleep(RETRY_DELAY);
    };

    Err(final_err)
}

fn on_device_activated(
    state: device::DeviceState,
    device_state: &http_server::SharedDeviceState,
    beq_state: &ezbeq::SharedEzBeqState,
    nvs: &esp_idf_svc::nvs::EspDefaultNvsPartition,
) {
    info!(
        "device: {} (hw_id={}, dsp={})",
        state.product_name(),
        state.hw_id().hw_id,
        state.dsp_version()
    );

    if let Some(table) = state.profile().dsp_table {
        let inputs = table.inputs.len() as u8;
        let outputs = table.outputs.len() as u8;
        *beq_state.lock().unwrap() = ezbeq::EzBeqState::new(inputs, outputs);
    }

    // Restore any persisted PEQ shadow from NVS before publishing the device
    // into shared state, so the first /devices/0/config read after a reboot
    // reflects the last committed biquads (saved at /config POST and
    // measurement-mode commit boundaries).
    state.load_peq_shadow_from_nvs(nvs);

    let settings = settings::SETTINGS
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    if let Err(err) = state.enforce_volume_settings(*settings) {
        error!("cannot enforce maximum volume on connected device: {err}");
        return;
    }
    *device_state.lock().unwrap() = Some(Arc::new(state));
}

fn log_usb_state_transition(current: &mut UsbState, next: UsbState, reason: &str) {
    if *current != next {
        info!("USB state: {current:?} -> {next:?} ({reason})");
        *current = next;
    }
}

/// Start mDNS advertisement for the MiniDSP service.
#[cfg(any(esp_idf_comp_mdns_enabled, esp_idf_comp_espressif__mdns_enabled))]
fn start_mdns(device_state: &http_server::SharedDeviceState) -> Option<esp_idf_svc::mdns::EspMdns> {
    use esp_idf_svc::mdns::EspMdns;

    let mut mdns = match EspMdns::take() {
        Ok(m) => m,
        Err(e) => {
            warn!("mDNS init failed: {e}");
            return None;
        }
    };

    // Set hostname based on device serial, or fallback
    let (hostname, txt_records) = {
        let guard = device_state.lock().unwrap();
        match guard.as_ref() {
            Some(dev) => {
                let serial = dev.serial();
                let hostname = format!("minidsp-{serial}");
                let txt = vec![
                    ("hw_id".to_string(), dev.hw_id().hw_id.to_string()),
                    ("dsp_version".to_string(), dev.dsp_version().to_string()),
                    ("serial".to_string(), serial.to_string()),
                    ("product".to_string(), dev.product_name().to_string()),
                ];
                (hostname, txt)
            }
            None => ("minidsp".to_string(), vec![]),
        }
    };

    if let Err(e) = mdns.set_hostname(&hostname) {
        warn!("mDNS set_hostname failed: {e}");
        return Some(mdns);
    }

    // Build TXT records as slice of (&str, &str) pairs
    let txt_refs: Vec<(&str, &str)> = txt_records
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    if let Err(e) = mdns.add_service(
        Some(&format!("{hostname}-http")),
        "_minidsp",
        "_tcp",
        5380,
        &txt_refs,
    ) {
        warn!("mDNS add_service failed: {e}");
    } else {
        info!("mDNS: advertising {hostname}.local _minidsp._tcp:5380");
    }

    Some(mdns)
}

/// TCP bridge server on port 5333 — Device Console compatibility.
/// Accepts one connection at a time, forwards length-prefixed frames to/from the device.
/// Wire format: client sends [len][payload][checksum], server responds [len][payload] (no checksum).
fn tcp_bridge(state: http_server::SharedDeviceState, audio_state: usb_audio::SharedAudioState) {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    const TCP_BRIDGE_BUSY_FRAME: [u8; 1] = [minidsp_protocol::commands::RESP_ERROR];

    // The optional password has no TCP equivalent, so the bridge stops
    // listening while one is set and the listener polls to notice changes.
    loop {
        while auth::protected() {
            std::thread::sleep(Duration::from_millis(250));
        }
        let listener = match TcpListener::bind("0.0.0.0:5333") {
            Ok(l) => l,
            Err(e) => {
                log::error!("TCP bridge: failed to bind port 5333: {e}");
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }
        };
        log::info!("TCP bridge listening on port 5333");

        let _ = listener.set_nonblocking(true);
        loop {
            if auth::protected() {
                log::info!("TCP bridge: stopped while password protection is on");
                break;
            }
            let mut stream = match listener.accept() {
                Ok((s, _)) => s,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
                Err(e) => {
                    log::warn!("TCP bridge: accept error: {e}");
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
            };
            use std::os::fd::AsRawFd;
            auth::tcp_connected(stream.as_raw_fd());
            let _ = stream.set_nonblocking(false);
            log::info!("TCP bridge: client connected from {:?}", stream.peer_addr());

            // Set read timeout to detect disconnects
            let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(30)));

            // Persistent read buffer for TCP stream reassembly (handles zero-padding from WI-DG)
            let mut read_buf = Vec::with_capacity(256);
            let mut tmp = [0u8; 128];

            loop {
                if auth::protected() {
                    break;
                }
                // Try to decode a frame from buffered data first
                if let Some((payload, consumed)) =
                    minidsp_protocol::packet::decode_tcp_frame(&read_buf)
                {
                    read_buf.drain(..consumed);

                    if audio_state.is_playing() {
                        log::warn!("TCP bridge: rejecting command during playback");
                        if stream.write_all(&TCP_BRIDGE_BUSY_FRAME).is_err() {
                            break;
                        }
                        continue;
                    }

                    // Forward to device
                    let dev = match http_server::get_device(&state) {
                        Some(d) => d,
                        None => {
                            log::warn!("TCP bridge: no device connected");
                            // Don't break — keep connection alive, device may reconnect
                            continue;
                        }
                    };
                    let response = http_server::tcp_bridge_command_with_audio_coordination(
                        &state,
                        &audio_state,
                        &dev,
                        &payload,
                    );

                    match response {
                        Ok(resp_frame) => {
                            if stream.write_all(&resp_frame).is_err() {
                                break;
                            }
                        }
                        Err(e) => {
                            if http_server::is_device_busy_error(&e) {
                                if stream.write_all(&TCP_BRIDGE_BUSY_FRAME).is_err() {
                                    break;
                                }
                            } else {
                                log::warn!("TCP bridge: device error: {e}");
                            }
                            // Don't break — keep connection alive
                        }
                    }
                    continue;
                }

                // Need more data — read from socket
                match stream.read(&mut tmp) {
                    Ok(0) => break, // Client disconnected
                    Ok(n) => {
                        read_buf.extend_from_slice(&tmp[..n]);
                        // Cap buffer to prevent OOM on malformed/malicious input
                        if read_buf.len() > 1024 {
                            log::warn!("TCP bridge: oversized buffer, dropping connection");
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            auth::tcp_disconnected();
            log::info!("TCP bridge: client disconnected");
        }
    }
}

/// UDP discovery broadcaster on port 3999.
/// Broadcasts device info every 1 second, matching minidsp-rs format.
fn udp_discovery(state: http_server::SharedDeviceState) {
    use std::net::UdpSocket;

    let socket = match UdpSocket::bind("0.0.0.0:0") {
        Ok(s) => s,
        Err(e) => {
            log::error!("UDP discovery: failed to bind: {e}");
            return;
        }
    };
    if let Err(e) = socket.set_broadcast(true) {
        log::error!("UDP discovery: set_broadcast failed: {e}");
        return;
    }

    log::info!("UDP discovery broadcaster started");

    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));

        if auth::protected() {
            continue;
        }
        let Some(packet) = discovery_packet_for_state(&state) else {
            continue;
        };
        let pkt = packet.encode();

        if let Err(e) = socket.send_to(&pkt, "255.255.255.255:3999") {
            log::debug!("UDP discovery send failed: {e}");
        }
        let _ = socket.send_to(&pkt, "224.0.0.250:3999");
    }
}

fn discovery_packet_for_state(
    state: &http_server::SharedDeviceState,
) -> Option<minidsp_protocol::discovery::DiscoveryPacket> {
    let ip = get_sta_ip()?;
    let mac = get_sta_mac().unwrap_or([0; 6]);

    if let Some(dev) = http_server::get_device(state) {
        let hw = dev.hw_id();
        return Some(minidsp_protocol::discovery::DiscoveryPacket::from_device(
            mac,
            ip,
            hw.hw_id,
            hw.fw_major,
            hw.fw_minor,
            dev.dsp_version(),
            minidsp_protocol::discovery::serial_to_u16(dev.serial()),
            format!("minidsp-{:08x}", dev.serial()),
        ));
    }

    let serial = fallback_discovery_serial();
    Some(minidsp_protocol::discovery::DiscoveryPacket::from_device(
        mac,
        ip,
        0,
        0,
        1,
        0,
        serial,
        format!("minidsp-bridge-{serial:04x}"),
    ))
}

fn fallback_discovery_serial() -> u16 {
    let mac = get_sta_mac().unwrap_or([0, 0, 0, 0, 0, 0]);
    u16::from_be_bytes([mac[4], mac[5]])
}

/// Get the ESP32's STA IP address.
fn get_sta_ip() -> Option<[u8; 4]> {
    unsafe {
        let netif = esp_idf_svc::sys::esp_netif_get_handle_from_ifkey(c"WIFI_STA_DEF".as_ptr());
        if netif.is_null() {
            return None;
        }
        let mut ip_info: esp_idf_svc::sys::esp_netif_ip_info_t = std::mem::zeroed();
        if esp_idf_svc::sys::esp_netif_get_ip_info(netif, &mut ip_info) != esp_idf_svc::sys::ESP_OK
        {
            return None;
        }
        let ip = ip_info.ip.addr.to_le_bytes();
        Some(ip)
    }
}

fn get_sta_mac() -> Option<[u8; 6]> {
    use esp_idf_svc::sys::{esp_wifi_get_mac, wifi_interface_t_WIFI_IF_STA, ESP_OK};

    unsafe {
        let mut mac = [0u8; 6];
        if esp_wifi_get_mac(wifi_interface_t_WIFI_IF_STA, mac.as_mut_ptr()) != ESP_OK {
            return None;
        }
        Some(mac)
    }
}
