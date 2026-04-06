//! WiFi connection, SoftAP provisioning, and auto-reconnection for ESP32-S3.
//!
//! Boot sequence:
//! 1. Check NVS for saved WiFi credentials
//! 2. If credentials exist: connect with exponential backoff
//! 3. If no credentials or AUTH_FAIL: start SoftAP captive portal
//! 4. User enters WiFi credentials via web page
//! 5. Save to NVS, restart into STA mode
//!
//! After connection, a background monitor thread watches for disconnects
//! and reconnects automatically with exponential backoff.

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use esp_idf_svc::wifi::{
    AccessPointConfiguration, AuthMethod, BlockingWifi, ClientConfiguration, Configuration,
    EspWifi,
};
use log::{info, warn};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
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

/// Shared flag indicating whether WiFi is currently connected (STA mode).
static WIFI_CONNECTED: AtomicBool = AtomicBool::new(false);

/// Check if WiFi STA is connected (safe to call from any thread).
#[allow(dead_code)]
pub fn is_connected() -> bool {
    WIFI_CONNECTED.load(Ordering::Relaxed)
}

/// Result of WiFi initialization.
pub struct WifiHandle {
    pub wifi: BlockingWifi<EspWifi<'static>>,
    pub is_sta: bool,
}

/// Attempt to connect to saved WiFi, or start provisioning.
/// Returns the WiFi handle (must be kept alive for WiFi to stay connected).
pub fn connect_or_provision(
    nvs: EspDefaultNvsPartition,
) -> Result<WifiHandle, WifiError> {
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
                WIFI_CONNECTED.store(true, Ordering::Relaxed);
                return Ok(WifiHandle {
                    wifi,
                    is_sta: true,
                });
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

    Ok(WifiHandle {
        wifi,
        is_sta: false,
    })
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

    info!("SoftAP started: SSID='{SOFTAP_SSID}'");
    Ok(())
}

// ---------------------------------------------------------------------------
// WiFi monitor — auto-reconnect background thread
// ---------------------------------------------------------------------------

/// Start a background thread that monitors WiFi and reconnects on disconnect.
pub fn start_monitor(
    wifi: Arc<std::sync::Mutex<BlockingWifi<EspWifi<'static>>>>,
) {
    std::thread::Builder::new()
        .name("wifi_mon".into())
        .stack_size(8192)
        .spawn(move || monitor_loop(wifi))
        .expect("failed to spawn wifi monitor thread");
}

fn monitor_loop(
    wifi: Arc<std::sync::Mutex<BlockingWifi<EspWifi<'static>>>>,
) {
    let mut backoff = Duration::from_secs(1);
    let max_backoff = Duration::from_secs(60);
    let check_interval = Duration::from_secs(5);

    info!("WiFi monitor started");

    loop {
        std::thread::sleep(check_interval);

        let connected = {
            let wifi = wifi.lock().unwrap_or_else(|e| e.into_inner());
            wifi.is_connected().unwrap_or(false)
        };

        if connected {
            if !WIFI_CONNECTED.load(Ordering::Relaxed) {
                // Transition: disconnected → connected
                let ip = {
                    let wifi = wifi.lock().unwrap_or_else(|e| e.into_inner());
                    wifi.wifi()
                        .sta_netif()
                        .get_ip_info()
                        .map(|info| format!("{}", info.ip))
                        .unwrap_or_else(|_| "unknown".into())
                };
                info!("WiFi reconnected: {ip}");
                WIFI_CONNECTED.store(true, Ordering::Relaxed);
                backoff = Duration::from_secs(1);
            }
            continue;
        }

        // Disconnected
        if WIFI_CONNECTED.load(Ordering::Relaxed) {
            warn!("WiFi disconnected — starting reconnect loop");
            WIFI_CONNECTED.store(false, Ordering::Relaxed);
        }

        info!("WiFi: reconnect attempt (backoff: {backoff:?})");

        // Connect — brief mutex hold, don't block on wait_netif_up under lock
        let connect_ok = {
            let mut wifi = wifi.lock().unwrap_or_else(|e| e.into_inner());
            match wifi.connect() {
                Ok(()) => true,
                Err(e) => {
                    warn!("WiFi: reconnect failed: {e}");
                    false
                }
            }
        };

        if connect_ok {
            // Wait for IP in a separate mutex scope
            let wifi = wifi.lock().unwrap_or_else(|e| e.into_inner());
            match wifi.wait_netif_up() {
                Ok(()) => {
                    let ip = wifi
                        .wifi()
                        .sta_netif()
                        .get_ip_info()
                        .map(|info| format!("{}", info.ip))
                        .unwrap_or_else(|_| "unknown".into());
                    info!("WiFi reconnected: {ip}");
                    WIFI_CONNECTED.store(true, Ordering::Relaxed);
                    backoff = Duration::from_secs(1);
                    continue;
                }
                Err(e) => warn!("WiFi: netif_up failed: {e}"),
            }
        }

        // Wait before next attempt
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(max_backoff);
    }
}

// ---------------------------------------------------------------------------
// Captive portal DNS hijack
// ---------------------------------------------------------------------------

/// Start a DNS server that responds to ALL queries with the given IP.
/// This triggers captive portal detection on phones (iOS, Android, Windows).
pub fn start_captive_dns(ip: std::net::Ipv4Addr) {
    std::thread::Builder::new()
        .name("dns_cp".into())
        .stack_size(6144)
        .spawn(move || captive_dns_loop(ip))
        .expect("failed to spawn captive DNS thread");
    info!("Captive portal DNS started (all queries → {ip})");
}

fn captive_dns_loop(ip: std::net::Ipv4Addr) {
    use std::net::UdpSocket;

    let socket = match UdpSocket::bind("0.0.0.0:53") {
        Ok(s) => s,
        Err(e) => {
            warn!("DNS bind failed: {e}");
            return;
        }
    };

    let mut buf = [0u8; 512];
    loop {
        let (len, src) = match socket.recv_from(&mut buf) {
            Ok(r) => r,
            Err(_) => continue,
        };

        // Minimum DNS header is 12 bytes
        if len < 12 {
            continue;
        }

        // Build response: copy the query, flip flags, append answer
        let mut resp = Vec::with_capacity(len + 16);
        resp.extend_from_slice(&buf[..len]);

        // Response flags: QR=1, AA=1, RD=1, RA=1
        resp[2] = 0x81;
        resp[3] = 0x80;
        // ANCOUNT = 1
        resp[6] = 0x00;
        resp[7] = 0x01;

        // Answer record: name pointer → offset 12 (question name)
        resp.extend_from_slice(&[
            0xC0, 0x0C, // Name pointer
            0x00, 0x01, // Type A
            0x00, 0x01, // Class IN
        ]);
        resp.extend_from_slice(&60u32.to_be_bytes()); // TTL = 60s
        resp.extend_from_slice(&[0x00, 0x04]); // RDLENGTH = 4
        resp.extend_from_slice(&ip.octets()); // The IP address

        let _ = socket.send_to(&resp, src);
    }
}

// ---------------------------------------------------------------------------
// NVS credentials
// ---------------------------------------------------------------------------

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
    let nvs = EspNvs::<NvsDefault>::new(nvs.clone(), NVS_NAMESPACE, true)
        .map_err(|e| WifiError::Provision(format!("NVS open: {e}")))?;

    nvs.set_str(NVS_KEY_SSID, ssid)
        .map_err(|e| WifiError::Provision(format!("NVS write ssid: {e}")))?;
    nvs.set_str(NVS_KEY_PASS, password)
        .map_err(|e| WifiError::Provision(format!("NVS write pass: {e}")))?;

    info!("WiFi credentials saved to NVS");
    Ok(())
}

/// Read the actual SoftAP IP from the ESP-IDF network interface.
/// Falls back to 192.168.4.1 if the interface isn't available.
pub fn get_softap_ip() -> std::net::Ipv4Addr {
    unsafe {
        let netif = esp_idf_svc::sys::esp_netif_get_handle_from_ifkey(
            b"WIFI_AP_DEF\0".as_ptr() as _,
        );
        if !netif.is_null() {
            let mut ip_info: esp_idf_svc::sys::esp_netif_ip_info_t = core::mem::zeroed();
            if esp_idf_svc::sys::esp_netif_get_ip_info(netif, &mut ip_info)
                == esp_idf_svc::sys::ESP_OK as i32
            {
                let ip = ip_info.ip.addr;
                if ip != 0 {
                    return std::net::Ipv4Addr::new(
                        (ip & 0xFF) as u8,
                        ((ip >> 8) & 0xFF) as u8,
                        ((ip >> 16) & 0xFF) as u8,
                        ((ip >> 24) & 0xFF) as u8,
                    );
                }
            }
        }
    }
    std::net::Ipv4Addr::new(192, 168, 4, 1)
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
<label for="ssid">Network Name (SSID)</label>
<input id="ssid" name="ssid" required autofocus>
<label for="password">Password</label>
<input id="password" name="password" type="password">
<button type="submit">Connect</button>
</form>
</body></html>"#;
