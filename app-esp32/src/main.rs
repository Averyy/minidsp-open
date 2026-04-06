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
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod device;
mod diagnostics;
mod http_server;
mod usb_hid;
mod wifi;

fn main() {
    sys::link_patches();

    // Step 0: Initialize ring buffer logger (before EspLogger so we capture everything)
    diagnostics::init_logger();

    info!("minidsp-open v{} starting", env!("CARGO_PKG_VERSION"));

    // Initialize NVS (required for WiFi credentials)
    let nvs = esp_idf_svc::nvs::EspDefaultNvsPartition::take()
        .expect("failed to initialize NVS");

    // Step 1: WiFi connection (or provisioning)
    info!("step 1: WiFi setup");
    let wifi_handle = match wifi::connect_or_provision(nvs.clone()) {
        Ok(handle) => {
            if handle.is_sta {
                info!("WiFi connected (STA mode)");
            } else {
                info!("WiFi in SoftAP provisioning mode");
            }
            Some(handle)
        }
        Err(e) => {
            error!("WiFi failed: {e} — continuing without network");
            None
        }
    };

    let wifi_is_sta = wifi_handle.as_ref().is_some_and(|h| h.is_sta);

    // Shared device state (None until USB device is probed)
    let device_state: http_server::SharedDeviceState = Arc::new(Mutex::new(None));

    // In SoftAP mode, start captive portal BEFORE USB init.
    // Phone captive portal detection has a ~5s timeout — USB blocks up to 30s.
    if !wifi_is_sta && wifi_handle.is_some() {
        let ap_ip = wifi::get_softap_ip();
        info!("step 2: starting captive portal at {ap_ip}");
        wifi::configure_dhcp_for_captive_portal(ap_ip);
        wifi::start_captive_dns(ap_ip);
        match http_server::start(device_state.clone(), 80, nvs.clone(), Some(ap_ip)) {
            Ok(server) => {
                info!("captive portal running on port 80");
                Box::leak(Box::new(server));
            }
            Err(e) => error!("captive portal HTTP failed: {e}"),
        }
    }

    // Step 3: USB Host HID initialization (blocks until device found)
    info!("step 3: USB Host HID setup");
    let bus = match usb_hid::UsbHidBus::init() {
        Ok(bus) => {
            info!("USB HID bus initialized");
            Some(Arc::new(bus))
        }
        Err(e) => {
            warn!("USB HID init failed: {e}");
            None
        }
    };

    // Step 4: Probe device and build state
    if let Some(ref bus) = bus {
        match device::DeviceState::probe(bus.clone()) {
            Ok(state) => {
                info!(
                    "device: {} (hw_id={}, dsp={})",
                    state.product_name(),
                    state.hw_id().hw_id,
                    state.dsp_version()
                );
                *device_state.lock().unwrap() = Some(Arc::new(state));
            }
            Err(e) => {
                error!("device probe failed: {e}");
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
        info!("step 6: starting HTTP server on port 5380");
        match http_server::start(device_state.clone(), 5380, nvs.clone(), None) {
            Ok(server) => {
                info!("HTTP server running on port 5380");
                Box::leak(Box::new(server));
            }
            Err(e) => error!("HTTP server failed: {e}"),
        }
    }

    // Step 7: Start WiFi monitor thread (auto-reconnect on disconnect)
    if let Some(handle) = wifi_handle {
        if handle.is_sta {
            let wifi_mutex = Arc::new(std::sync::Mutex::new(handle.wifi));
            wifi::start_monitor(wifi_mutex);
        } else {
            // SoftAP mode — keep the wifi handle alive but no monitor
            Box::leak(Box::new(handle.wifi));
        }
    }

    info!("minidsp-open ready");

    // Dump the boot log from ring buffer so late serial connections can see it
    let boot_log = diagnostics::log_snapshot();
    for line in boot_log.lines() {
        info!("[boot] {line}");
    }

    // Main loop: diagnostics tick + USB reconnect check
    let mut usb_reconnect_backoff = Duration::from_secs(5);
    loop {
        diagnostics::tick();

        // Check for USB reconnection if device disconnected
        if let Some(ref bus) = bus {
            if !bus.is_connected() {
                let had_device = device_state.lock().unwrap().is_some();
                if had_device {
                    warn!("USB device lost — clearing device state");
                    *device_state.lock().unwrap() = None;
                }

                info!("USB: attempting reconnect (backoff: {usb_reconnect_backoff:?})");
                match bus.reconnect(Duration::from_secs(10)) {
                    Ok(()) => {
                        // Re-probe the device
                        match device::DeviceState::probe(bus.clone()) {
                            Ok(state) => {
                                info!(
                                    "USB reconnected: {} (hw_id={}, dsp={})",
                                    state.product_name(),
                                    state.hw_id().hw_id,
                                    state.dsp_version()
                                );
                                *device_state.lock().unwrap() = Some(Arc::new(state));
                                usb_reconnect_backoff = Duration::from_secs(5);
                            }
                            Err(e) => {
                                error!("USB re-probe failed: {e}");
                            }
                        }
                    }
                    Err(e) => {
                        warn!("USB reconnect failed: {e}");
                        usb_reconnect_backoff =
                            (usb_reconnect_backoff * 2).min(Duration::from_secs(60));
                    }
                }
            } else {
                usb_reconnect_backoff = Duration::from_secs(5);
            }
        }

        std::thread::sleep(Duration::from_secs(30));
    }
}

/// Start mDNS advertisement for the MiniDSP service.
#[cfg(any(esp_idf_comp_mdns_enabled, esp_idf_comp_espressif__mdns_enabled))]
fn start_mdns(
    device_state: &http_server::SharedDeviceState,
) -> Option<esp_idf_svc::mdns::EspMdns> {
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
