//! WiFi connection and SoftAP provisioning for ESP32-S3.
//!
//! Boot sequence:
//! 1. Check NVS for saved WiFi credentials
//! 2. If credentials exist: connect with exponential backoff
//! 3. If no credentials or AUTH_FAIL: start SoftAP captive portal
//! 4. User enters WiFi credentials via web page
//! 5. Save to NVS, restart into STA mode

use esp_idf_svc::nvs::EspDefaultNvsPartition;
use log::{info, warn, error};

/// WiFi connection error.
#[derive(Debug)]
pub enum WifiError {
    Init(String),
    Connect(String),
    Provision(String),
}

impl core::fmt::Display for WifiError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            WifiError::Init(s) => write!(f, "WiFi init: {s}"),
            WifiError::Connect(s) => write!(f, "WiFi connect: {s}"),
            WifiError::Provision(s) => write!(f, "WiFi provision: {s}"),
        }
    }
}

/// Attempt to connect to saved WiFi, or start provisioning.
///
/// Returns true if WiFi is connected, false if in provisioning mode.
pub async fn connect_or_provision(nvs: EspDefaultNvsPartition) -> Result<bool, WifiError> {
    // TODO: Implement WiFi connection flow
    // 1. Read SSID/password from NVS
    // 2. If present: esp_wifi_connect() with backoff
    // 3. If not present or AUTH_FAIL: start SoftAP
    //
    // Key disconnect reason codes:
    //   202 (AUTH_FAIL) → wrong password, don't retry endlessly
    //   200 (BEACON_TIMEOUT) → AP went away, retry aggressively
    //   201 (NO_AP_FOUND) → AP not broadcasting, retry with backoff
    //
    // Backoff: 1s base, 2x multiplier, 60s max, 20 retries before long pause

    info!("WiFi: checking NVS for saved credentials...");

    // Placeholder: return false (no WiFi)
    // In real implementation, this uses esp-idf-svc::wifi::EspWifi
    Ok(false)
}

/// Start SoftAP captive portal for WiFi provisioning.
///
/// SSID: minidsp-setup (open network)
/// DNS redirects all queries to 192.168.4.1
/// Serves a simple HTML page with network scanner + password field.
pub async fn start_provisioning(nvs: EspDefaultNvsPartition) -> Result<(), WifiError> {
    info!("WiFi: starting SoftAP provisioning (SSID: minidsp-setup)");

    // TODO: Implement SoftAP + captive portal
    // 1. Configure WiFi in APSTA mode (AP for provisioning, STA for connection test)
    // 2. Start DNS server redirecting all queries to 192.168.4.1
    // 3. Serve provisioning HTML page
    // 4. On form submit: save credentials to NVS, connect, restart

    Ok(())
}
