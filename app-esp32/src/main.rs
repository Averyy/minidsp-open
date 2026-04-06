//! minidsp-open ESP32-S3 firmware
//!
//! USB Host HID → MiniDSP protocol → WiFi HTTP API.
//! Blocking architecture on ESP-IDF std.

use esp_idf_svc::log::EspLogger;
use esp_idf_svc::sys;
#[allow(unused_imports)]
use log::{error, info, warn};
use std::sync::Arc;
use std::time::Duration;

mod device;
mod diagnostics;
mod http_server;
mod usb_hid;
mod wifi;

fn main() {
    sys::link_patches();
    EspLogger::initialize_default();

    info!("minidsp-open v{} starting", env!("CARGO_PKG_VERSION"));

    // Initialize NVS (required for WiFi credentials)
    let nvs = esp_idf_svc::nvs::EspDefaultNvsPartition::take()
        .expect("failed to initialize NVS");

    // Step 1: WiFi connection (or provisioning)
    info!("step 1: WiFi setup");
    let _wifi = match wifi::connect_or_provision(nvs.clone()) {
        Ok(wifi) => {
            info!("WiFi connected");
            Some(wifi)
        }
        Err(e) => {
            error!("WiFi failed: {e} — continuing without network");
            None
        }
    };

    // Step 2: USB Host HID initialization (blocks until device found)
    info!("step 2: USB Host HID setup");
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

    // Step 3: Probe device and build state
    let state = if let Some(ref bus) = bus {
        match device::DeviceState::probe(bus.clone()) {
            Ok(state) => {
                info!(
                    "device: {} (hw_id={}, dsp={})",
                    state.product_name(),
                    state.hw_id().hw_id,
                    state.dsp_version()
                );
                Some(Arc::new(state))
            }
            Err(e) => {
                error!("device probe failed: {e}");
                None
            }
        }
    } else {
        None
    };

    // Step 4: Start HTTP server
    if _wifi.is_some() {
        info!("step 3: starting HTTP server");
        match http_server::start(state.clone()) {
            Ok(server) => {
                info!("HTTP server running on port 5380");
                // Leak server handle to keep it alive
                Box::leak(Box::new(server));
            }
            Err(e) => error!("HTTP server failed: {e}"),
        }
    }

    info!("minidsp-open ready");

    // Main loop: diagnostics
    loop {
        diagnostics::tick();
        std::thread::sleep(Duration::from_secs(30));
    }
}
