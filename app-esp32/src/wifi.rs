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

use esp_idf_svc::eventloop::{EspSubscription, EspSystemEventLoop, System};
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use esp_idf_svc::wifi::{
    AccessPointConfiguration, AuthMethod, BlockingWifi, ClientConfiguration, Configuration,
    EspWifi, WifiEvent,
};
use log::{info, warn};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
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
static LAST_DISCONNECT_REASON: AtomicU32 = AtomicU32::new(0);

const WIFI_REASON_UNSPECIFIED: u32 = 1;
const WIFI_REASON_ASSOC_EXPIRE: u32 = 4;
const WIFI_REASON_MIC_FAILURE: u32 = 14;
const WIFI_REASON_4WAY_HANDSHAKE_TIMEOUT: u32 = 15;
const WIFI_REASON_BEACON_TIMEOUT: u32 = 200;
const WIFI_REASON_NO_AP_FOUND: u32 = 201;
const WIFI_REASON_AUTH_FAIL: u32 = 202;
const WIFI_REASON_NO_AP_FOUND_W_COMPATIBLE_SECURITY: u32 = 210;
const WIFI_REASON_NO_AP_FOUND_IN_AUTHMODE_THRESHOLD: u32 = 211;
const WIFI_REASON_NO_AP_FOUND_IN_RSSI_THRESHOLD: u32 = 212;

/// Check if WiFi STA is connected (safe to call from any thread).
#[allow(dead_code)]
pub fn is_connected() -> bool {
    WIFI_CONNECTED.load(Ordering::Relaxed)
}

pub fn last_disconnect_reason() -> u32 {
    LAST_DISCONNECT_REASON.load(Ordering::Relaxed)
}

/// Result of WiFi initialization.
pub struct WifiHandle {
    pub wifi: BlockingWifi<EspWifi<'static>>,
    pub is_sta: bool,
    _disconnect_subscription: EspSubscription<'static, System>,
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
        sysloop.clone(),
    )
    .map_err(|e| WifiError::Init(format!("BlockingWifi: {e}")))?;
    let disconnect_subscription = register_disconnect_reason_handler(&sysloop)?;

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
                    _disconnect_subscription: disconnect_subscription,
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
        _disconnect_subscription: disconnect_subscription,
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

fn register_disconnect_reason_handler(
    sysloop: &EspSystemEventLoop,
) -> Result<EspSubscription<'static, System>, WifiError> {
    sysloop
        .subscribe::<WifiEvent, _>(move |event| {
            if let WifiEvent::StaDisconnected(disconnected) = event {
                let reason = disconnected.reason() as u32;
                LAST_DISCONNECT_REASON.store(reason, Ordering::Relaxed);
                WIFI_CONNECTED.store(false, Ordering::Relaxed);
                warn!(
                    "WiFi disconnect reason {} ({})",
                    reason,
                    disconnect_reason_name(reason)
                );
            }
        })
        .map_err(|e| WifiError::Init(format!("wifi disconnect subscription: {e}")))
}

// ---------------------------------------------------------------------------
// WiFi monitor — auto-reconnect background thread
// ---------------------------------------------------------------------------

/// Start a background thread that monitors WiFi and reconnects on disconnect.
pub fn start_monitor(
    wifi: Arc<std::sync::Mutex<BlockingWifi<EspWifi<'static>>>>,
    nvs: EspDefaultNvsPartition,
) {
    std::thread::Builder::new()
        .name("wifi_mon".into())
        .stack_size(8192)
        .spawn(move || monitor_loop(wifi, nvs))
        .expect("failed to spawn wifi monitor thread");
}

fn monitor_loop(
    wifi: Arc<std::sync::Mutex<BlockingWifi<EspWifi<'static>>>>,
    nvs: EspDefaultNvsPartition,
) {
    let mut backoff = Duration::from_secs(1);
    let max_backoff = Duration::from_secs(60);
    let check_interval = Duration::from_secs(5);
    let mut credential_retries = 0u8;

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
                credential_retries = 0;
            }
            continue;
        }

        // Disconnected
        if WIFI_CONNECTED.load(Ordering::Relaxed) {
            warn!("WiFi disconnected — starting reconnect loop");
            WIFI_CONNECTED.store(false, Ordering::Relaxed);
        }

        let reason = LAST_DISCONNECT_REASON.load(Ordering::Relaxed);
        if is_credential_disconnect(reason) {
            credential_retries = credential_retries.saturating_add(1);
            warn!(
                "WiFi: credential-related disconnect {} ({}) retry {}/3",
                reason,
                disconnect_reason_name(reason),
                credential_retries
            );
            if credential_retries >= 3 {
                warn!("WiFi: restarting into provisioning mode after repeated credential failures");
                if let Err(e) = clear_credentials(&nvs) {
                    warn!("WiFi: failed to clear credentials before provisioning fallback: {e}");
                }
                std::thread::sleep(Duration::from_secs(1));
                unsafe { esp_idf_svc::sys::esp_restart() };
            }
        } else {
            credential_retries = 0;
            info!(
                "WiFi: reconnect attempt (reason: {} {}, backoff: {:?})",
                reason,
                disconnect_reason_name(reason),
                backoff
            );
        }

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
        if is_transient_disconnect(reason) || reason == 0 || reason == WIFI_REASON_UNSPECIFIED {
            std::thread::sleep(backoff);
            backoff = (backoff * 2).min(max_backoff);
        } else {
            std::thread::sleep(Duration::from_secs(2));
        }
    }
}

fn is_credential_disconnect(reason: u32) -> bool {
    matches!(
        reason,
        WIFI_REASON_AUTH_FAIL
            | WIFI_REASON_4WAY_HANDSHAKE_TIMEOUT
            | WIFI_REASON_MIC_FAILURE
    )
}

fn is_transient_disconnect(reason: u32) -> bool {
    matches!(
        reason,
        WIFI_REASON_BEACON_TIMEOUT
            | WIFI_REASON_ASSOC_EXPIRE
            | WIFI_REASON_NO_AP_FOUND
            | WIFI_REASON_NO_AP_FOUND_W_COMPATIBLE_SECURITY
            | WIFI_REASON_NO_AP_FOUND_IN_AUTHMODE_THRESHOLD
            | WIFI_REASON_NO_AP_FOUND_IN_RSSI_THRESHOLD
            | WIFI_REASON_UNSPECIFIED
    )
}

fn disconnect_reason_name(reason: u32) -> &'static str {
    match reason {
        WIFI_REASON_UNSPECIFIED => "unspecified",
        WIFI_REASON_ASSOC_EXPIRE => "assoc_expire",
        WIFI_REASON_MIC_FAILURE => "mic_failure",
        WIFI_REASON_4WAY_HANDSHAKE_TIMEOUT => "4way_handshake_timeout",
        WIFI_REASON_BEACON_TIMEOUT => "beacon_timeout",
        WIFI_REASON_NO_AP_FOUND => "no_ap_found",
        WIFI_REASON_AUTH_FAIL => "auth_fail",
        WIFI_REASON_NO_AP_FOUND_W_COMPATIBLE_SECURITY => "no_ap_found_compatible_security",
        WIFI_REASON_NO_AP_FOUND_IN_AUTHMODE_THRESHOLD => "no_ap_found_authmode_threshold",
        WIFI_REASON_NO_AP_FOUND_IN_RSSI_THRESHOLD => "no_ap_found_rssi_threshold",
        _ => "other",
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
        .stack_size(8192)
        .spawn(move || captive_dns_loop(ip))
        .expect("failed to spawn captive DNS thread");
    info!("Captive portal DNS started (all queries → {ip})");
}

fn captive_dns_loop(ip: std::net::Ipv4Addr) {
    use std::net::UdpSocket;

    let socket = match UdpSocket::bind("0.0.0.0:53") {
        Ok(s) => {
            info!("DNS server bound to 0.0.0.0:53");
            s
        }
        Err(e) => {
            warn!("DNS bind failed: {e}");
            return;
        }
    };

    let mut buf = [0u8; 512];
    let mut query_count: u32 = 0;
    loop {
        let (len, src) = match socket.recv_from(&mut buf) {
            Ok(r) => r,
            Err(e) => {
                warn!("DNS recv error: {e}");
                continue;
            }
        };

        query_count += 1;
        if query_count <= 20 || query_count % 100 == 0 {
            info!("DNS #{query_count} from {src} (len={len}) → {ip}");
        }

        // Minimum DNS header is 12 bytes
        if len < 12 {
            continue;
        }

        // Build response: copy the query, set QR flag
        let mut resp = Vec::with_capacity(len + 16);
        resp.extend_from_slice(&buf[..len]);

        // Response flags: QR=1, AA=1, RD=1, RA=1
        resp[2] = 0x81;
        resp[3] = 0x80;

        // Only answer Type A queries (0x0001). iOS sends AAAA/HTTPS queries too;
        // responding with a Type A answer to those produces malformed responses
        // that iOS rejects, breaking captive portal detection.
        let qtype = if len >= 16 {
            // Skip past the question name to find QTYPE
            let mut pos = 12;
            while pos < len && buf[pos] != 0 {
                pos += 1 + buf[pos] as usize;
            }
            if pos + 4 < len {
                u16::from_be_bytes([buf[pos + 1], buf[pos + 2]])
            } else {
                0
            }
        } else {
            0
        };

        if qtype == 1 {
            // Type A query — answer with our IP
            resp[6] = 0x00;
            resp[7] = 0x01; // ANCOUNT = 1

            resp.extend_from_slice(&[
                0xC0, 0x0C, // Name pointer → question name
                0x00, 0x01, // Type A
                0x00, 0x01, // Class IN
            ]);
            resp.extend_from_slice(&60u32.to_be_bytes()); // TTL = 60s
            resp.extend_from_slice(&[0x00, 0x04]); // RDLENGTH = 4
            resp.extend_from_slice(&ip.octets());
        } else {
            // Non-A query — respond with ANCOUNT=0 (no answer)
            resp[6] = 0x00;
            resp[7] = 0x00;
        }

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

/// Clear saved WiFi credentials from NVS so the next boot falls back to provisioning.
pub fn clear_credentials(nvs: &EspDefaultNvsPartition) -> Result<(), WifiError> {
    let nvs = EspNvs::<NvsDefault>::new(nvs.clone(), NVS_NAMESPACE, true)
        .map_err(|e| WifiError::Provision(format!("NVS open: {e}")))?;

    let _ = nvs.remove(NVS_KEY_SSID);
    let _ = nvs.remove(NVS_KEY_PASS);

    info!("WiFi credentials cleared from NVS");
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

/// Configure DHCP server for captive portal:
/// - Option 6 (DNS server) → AP IP so clients send DNS to us
/// - Option 114 (RFC 8910 captive portal URI) → http://<AP IP>
pub fn configure_dhcp_for_captive_portal(ip: std::net::Ipv4Addr) {
    unsafe {
        let netif = esp_idf_svc::sys::esp_netif_get_handle_from_ifkey(
            b"WIFI_AP_DEF\0".as_ptr() as _,
        );
        if netif.is_null() {
            warn!("DHCP captive portal: netif not found");
            return;
        }

        // Stop DHCP server to reconfigure
        esp_idf_svc::sys::esp_netif_dhcps_stop(netif);

        // Set DNS server for the AP interface so DHCP advertises it to clients
        let ip_bytes = ip.octets();
        let dns_info = esp_idf_svc::sys::esp_netif_dns_info_t {
            ip: esp_idf_svc::sys::esp_ip_addr_t {
                u_addr: esp_idf_svc::sys::_ip_addr__bindgen_ty_1 {
                    ip4: esp_idf_svc::sys::esp_ip4_addr_t {
                        addr: u32::from_le_bytes(ip_bytes),
                    },
                },
                type_: 0, // ESP_IPADDR_TYPE_V4
            },
        };
        let ret = esp_idf_svc::sys::esp_netif_set_dns_info(
            netif,
            esp_idf_svc::sys::esp_netif_dns_type_t_ESP_NETIF_DNS_MAIN,
            &dns_info as *const _ as *mut _,
        );
        if ret != esp_idf_svc::sys::ESP_OK as i32 {
            warn!("set DNS info failed: {ret}");
        } else {
            info!("DNS server set to {ip} for AP interface");
        }

        // Option 114: Captive portal URI
        let uri = format!("http://{ip}\0");
        let ret = esp_idf_svc::sys::esp_netif_dhcps_option(
            netif,
            esp_idf_svc::sys::esp_netif_dhcp_option_mode_t_ESP_NETIF_OP_SET,
            esp_idf_svc::sys::esp_netif_dhcp_option_id_t_ESP_NETIF_CAPTIVEPORTAL_URI,
            uri.as_ptr() as *mut _,
            (uri.len() - 1) as u32,
        );
        if ret != esp_idf_svc::sys::ESP_OK as i32 {
            warn!("DHCP Option 114 failed: {ret}");
        } else {
            info!("DHCP Option 114 set: http://{ip}");
        }

        // Restart DHCP server
        let ret = esp_idf_svc::sys::esp_netif_dhcps_start(netif);
        if ret != esp_idf_svc::sys::ESP_OK as i32 {
            warn!("DHCP restart failed: {ret}");
        }
    }
}

/// Provisioning HTML page — glass-card style matching open-sub-optimizer.
/// IMPORTANT: Do NOT include the word "Success" anywhere — iOS interprets it
/// as a connectivity check pass and closes the captive portal sheet.
pub const PROVISION_HTML: &str = r#"<!DOCTYPE html>
<html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1,user-scalable=no">
<title>MiniDSP WiFi Setup</title>
<style>
*{margin:0;padding:0;box-sizing:border-box}
html,body{height:100%;overflow:hidden}
body{font-family:-apple-system,BlinkMacSystemFont,"SF Pro Text","Helvetica Neue",system-ui,sans-serif;
background:#0B0B10;color:#E8E8ED;display:flex;align-items:center;justify-content:center;padding:1.5rem}
.card{background:rgba(28,20,40,0.35);border:1px solid rgba(180,180,255,0.07);border-radius:16px;
padding:2rem;width:100%;max-width:380px;backdrop-filter:blur(12px) saturate(150%);-webkit-backdrop-filter:blur(12px) saturate(150%)}
h1{font-size:1.25rem;font-weight:600;color:#E8E8ED;margin-bottom:0.25rem}
.subtitle{font-size:0.8rem;color:#A1A1A6;margin-bottom:1.5rem}
label{display:block;font-size:0.8rem;font-weight:600;color:#A1A1A6;text-transform:uppercase;letter-spacing:0.04em;margin-bottom:0.4rem}
input{width:100%;height:40px;padding:0.5rem 0.75rem;background:rgba(255,255,255,0.04);color:#E8E8ED;
border:1px solid rgba(255,255,255,0.08);border-radius:8px;font-size:0.95rem;outline:none;margin-bottom:1rem;
transition:border-color 0.15s}
input:focus{border-color:rgba(59,138,255,0.4)}
input::placeholder{color:#7C7C80}
button{width:100%;height:44px;background:rgba(59,138,255,0.15);color:#6BA7FF;
border:1px solid rgba(59,138,255,0.2);border-radius:10px;font-size:0.95rem;font-weight:500;
cursor:pointer;transition:background 0.15s,border-color 0.15s}
button:hover,button:active{background:rgba(59,138,255,0.25);border-color:rgba(59,138,255,0.35)}
</style></head><body>
<div class="card">
<h1>MiniDSP WiFi Setup</h1>
<p class="subtitle">Connect your MiniDSP controller to WiFi</p>
<form method="POST" action="/api/wifi/connect">
<label for="ssid">Network Name</label>
<input id="ssid" name="ssid" required autofocus placeholder="Your WiFi network">
<label for="password">Password</label>
<input id="password" name="password" type="password" placeholder="WiFi password">
<button type="submit">Connect</button>
</form>
</div>
</body></html>"#;
