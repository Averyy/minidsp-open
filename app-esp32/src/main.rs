//! minidsp-open ESP32-S3 firmware
//!
//! USB Host HID → MiniDSP protocol → WiFi HTTP API + TCP bridge + UDP discovery.
//! Runs on ESP-IDF with embassy-executor for async.

use esp_idf_svc::hal::prelude::*;
use esp_idf_svc::log::EspLogger;
use esp_idf_svc::sys;
use log::{info, warn, error};

mod usb_hid;
mod wifi;
mod http_server;
mod diagnostics;

fn main() {
    // Initialize ESP-IDF (binstart entry point)
    sys::link_patches();
    EspLogger::initialize_default();

    info!("minidsp-open v{} starting", env!("CARGO_PKG_VERSION"));

    // Initialize NVS (required for WiFi credentials)
    let nvs = esp_idf_svc::nvs::EspDefaultNvsPartition::take()
        .expect("failed to initialize NVS");

    // Initialize peripherals
    let peripherals = Peripherals::take()
        .expect("failed to take peripherals");

    // Start the main async runtime
    let executor = embassy_executor::Executor::new();
    let executor = Box::leak(Box::new(executor));

    executor.run(|spawner| {
        spawner.spawn(main_task(nvs, spawner)).unwrap();
    });
}

#[embassy_executor::task]
async fn main_task(
    nvs: esp_idf_svc::nvs::EspDefaultNvsPartition,
    spawner: embassy_executor::Spawner,
) {
    // Step 1: WiFi connection (or provisioning)
    info!("step 1: WiFi setup");
    let wifi_connected = match wifi::connect_or_provision(nvs.clone()).await {
        Ok(connected) => {
            info!("WiFi connected: {}", connected);
            connected
        }
        Err(e) => {
            error!("WiFi setup failed: {e}");
            false
        }
    };

    // Step 2: USB Host HID initialization
    info!("step 2: USB Host HID setup");
    let device = match usb_hid::init_and_find_device().await {
        Ok(dev) => {
            info!("MiniDSP device found: hw_id={}", dev.hw_id);
            Some(dev)
        }
        Err(e) => {
            warn!("No MiniDSP device found: {e}");
            None
        }
    };

    // Step 3: Start HTTP server (if WiFi connected)
    if wifi_connected {
        info!("step 3: Starting HTTP server");
        spawner.spawn(http_server::run(device)).unwrap();
    }

    // Step 4: Start diagnostics
    info!("step 4: Starting diagnostics");
    spawner.spawn(diagnostics::run()).unwrap();

    info!("minidsp-open ready");

    // Keep main task alive
    loop {
        embassy_time::Timer::after(embassy_time::Duration::from_secs(60)).await;
    }
}
