//! WiFi connection and SoftAP provisioning for ESP32-S3.
//!
//! Boot sequence:
//! 1. Check NVS for saved WiFi credentials
//! 2. If credentials exist: connect with exponential backoff
//! 3. If no credentials or AUTH_FAIL: start SoftAP captive portal
//! 4. User enters WiFi credentials via web page
//! 5. Save to NVS, restart into STA mode

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use esp_idf_svc::wifi::{
    AccessPointConfiguration, AuthMethod, BlockingWifi, ClientConfiguration, Configuration,
    EspWifi,
};
use log::{info, warn};
use std::time::Duration;

const NVS_NAMESPACE: &str = "wifi";
const NVS_KEY_SSID: &str = "ssid";
const NVS_KEY_PASS: &str = "pass";
const SOFTAP_SSID: &str = "minidsp-setup";

/// WiFi connection error.
#[derive(Debug)]
pub enum WifiError {
    Init(String),
    Connect(String),
    Provision(String),
}

impl std::fmt::Display for WifiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Init(s) => write!(f, "WiFi init: {s}"),
            Self::Connect(s) => write!(f, "WiFi connect: {s}"),
            Self::Provision(s) => write!(f, "WiFi provision: {s}"),
        }
    }
}

/// Attempt to connect to saved WiFi, or start provisioning.
/// Returns the WiFi handle (must be kept alive for WiFi to stay connected).
pub fn connect_or_provision(
    nvs: EspDefaultNvsPartition,
) -> Result<BlockingWifi<EspWifi<'static>>, WifiError> {
    let sysloop =
        EspSystemEventLoop::take().map_err(|e| WifiError::Init(format!("eventloop: {e}")))?;
    let modem = unsafe { Modem::steal() };

    let mut wifi = BlockingWifi::wrap(
        EspWifi::new(modem, sysloop.clone(), Some(nvs.clone()))
            .map_err(|e| WifiError::Init(format!("EspWifi: {e}")))?,
        sysloop,
    )
    .map_err(|e| WifiError::Init(format!("BlockingWifi: {e}")))?;

    // Try to read saved credentials from NVS
    if let Some((ssid, password)) = read_credentials(&nvs) {
        info!("WiFi: connecting to '{ssid}'");
        match connect_sta(&mut wifi, &ssid, &password) {
            Ok(()) => {
                let ip = wifi
                    .wifi()
                    .sta_netif()
                    .get_ip_info()
                    .map(|info| format!("{}", info.ip))
                    .unwrap_or_else(|_| "unknown".into());
                info!("WiFi connected: {ip}");
                return Ok(wifi);
            }
            Err(e) => {
                warn!("WiFi connection failed: {e}");
                // Fall through to provisioning
            }
        }
    } else {
        info!("WiFi: no saved credentials");
    }

    // Start SoftAP for provisioning
    info!("WiFi: starting SoftAP '{SOFTAP_SSID}' for provisioning");
    start_softap(&mut wifi)?;

    // In provisioning mode, the HTTP server will handle credential submission
    // For now, return the wifi handle in AP mode
    Ok(wifi)
}

/// Connect in STA mode with exponential backoff.
fn connect_sta(
    wifi: &mut BlockingWifi<EspWifi<'static>>,
    ssid: &str,
    password: &str,
) -> Result<(), WifiError> {
    wifi.set_configuration(&Configuration::Client(ClientConfiguration {
        ssid: ssid
            .try_into()
            .map_err(|_| WifiError::Connect("SSID too long".into()))?,
        password: password
            .try_into()
            .map_err(|_| WifiError::Connect("password too long".into()))?,
        auth_method: if password.is_empty() {
            AuthMethod::None
        } else {
            AuthMethod::WPA2Personal
        },
        ..Default::default()
    }))
    .map_err(|e| WifiError::Connect(format!("set_config: {e}")))?;

    wifi.start()
        .map_err(|e| WifiError::Connect(format!("start: {e}")))?;

    // Retry with exponential backoff
    let mut delay = Duration::from_secs(1);
    let max_delay = Duration::from_secs(60);
    let max_retries = 5;

    for attempt in 1..=max_retries {
        info!("WiFi: connect attempt {attempt}/{max_retries}");

        match wifi.connect() {
            Ok(()) => {
                // Wait for IP
                match wifi.wait_netif_up() {
                    Ok(()) => return Ok(()),
                    Err(e) => warn!("WiFi: netif_up failed: {e}"),
                }
            }
            Err(e) => {
                warn!("WiFi: connect failed: {e}");
            }
        }

        if attempt < max_retries {
            info!("WiFi: retrying in {delay:?}");
            std::thread::sleep(delay);
            delay = (delay * 2).min(max_delay);
        }
    }

    Err(WifiError::Connect(format!(
        "failed after {max_retries} attempts"
    )))
}

/// Start SoftAP mode for WiFi provisioning.
fn start_softap(wifi: &mut BlockingWifi<EspWifi<'static>>) -> Result<(), WifiError> {
    wifi.set_configuration(&Configuration::AccessPoint(AccessPointConfiguration {
        ssid: SOFTAP_SSID
            .try_into()
            .map_err(|_| WifiError::Provision("SSID too long".into()))?,
        auth_method: AuthMethod::None,
        max_connections: 4,
        ..Default::default()
    }))
    .map_err(|e| WifiError::Provision(format!("set_config: {e}")))?;

    wifi.start()
        .map_err(|e| WifiError::Provision(format!("start: {e}")))?;

    info!("SoftAP started: SSID='{SOFTAP_SSID}', IP=192.168.4.1");
    Ok(())
}

/// Read WiFi credentials from NVS.
fn read_credentials(nvs: &EspDefaultNvsPartition) -> Option<(String, String)> {
    let nvs = EspNvs::<NvsDefault>::new(nvs.clone(), NVS_NAMESPACE, false).ok()?;

    let mut ssid_buf = [0u8; 33];
    let mut pass_buf = [0u8; 65];

    let ssid = nvs.get_str(NVS_KEY_SSID, &mut ssid_buf).ok()??;
    let pass = nvs
        .get_str(NVS_KEY_PASS, &mut pass_buf)
        .ok()?
        .unwrap_or("");

    if ssid.is_empty() {
        return None;
    }

    Some((ssid.to_string(), pass.to_string()))
}

/// Save WiFi credentials to NVS.
pub fn save_credentials(
    nvs: &EspDefaultNvsPartition,
    ssid: &str,
    password: &str,
) -> Result<(), WifiError> {
    let mut nvs = EspNvs::<NvsDefault>::new(nvs.clone(), NVS_NAMESPACE, true)
        .map_err(|e| WifiError::Provision(format!("NVS open: {e}")))?;

    nvs.set_str(NVS_KEY_SSID, ssid)
        .map_err(|e| WifiError::Provision(format!("NVS write ssid: {e}")))?;
    nvs.set_str(NVS_KEY_PASS, password)
        .map_err(|e| WifiError::Provision(format!("NVS write pass: {e}")))?;

    info!("WiFi credentials saved to NVS");
    Ok(())
}

/// Provisioning HTML page (minimal captive portal).
pub const PROVISION_HTML: &str = r#"<!DOCTYPE html>
<html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>MiniDSP WiFi Setup</title>
<style>
body{font-family:system-ui;max-width:400px;margin:40px auto;padding:0 20px;background:#1a1a2e;color:#e0e0e0}
h1{color:#fff;font-size:1.4em}
input{width:100%;padding:12px;margin:8px 0;border:1px solid #444;border-radius:6px;background:#16213e;color:#fff;box-sizing:border-box}
button{width:100%;padding:14px;background:#0f3460;color:#fff;border:none;border-radius:6px;cursor:pointer;font-size:1em;margin-top:12px}
button:hover{background:#1a5276}
.status{color:#53c28b;margin-top:16px}
</style></head><body>
<h1>MiniDSP WiFi Setup</h1>
<form method="POST" action="/api/wifi/connect">
<label>Network Name (SSID)</label>
<input name="ssid" required autofocus>
<label>Password</label>
<input name="password" type="password">
<button type="submit">Connect</button>
</form>
</body></html>"#;
