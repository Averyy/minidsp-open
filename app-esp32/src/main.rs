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
mod ezbeq;
mod http_server;
mod storage;
mod updater;
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
        match http_server::start(device_state.clone(), 80, nvs.clone(), Some(ap_ip), None) {
            Ok(server) => {
                info!("captive portal running on port 80");
                Box::leak(Box::new(server));
            }
            Err(e) => error!("captive portal HTTP failed: {e}"),
        }
    }

    // Step 3: USB Host HID initialization (sets up host library + background threads)
    info!("step 3: USB Host HID setup");
    let bus = match usb_hid::UsbHidBus::init() {
        Ok(bus) => Some(Arc::new(bus)),
        Err(e) => {
            error!("USB HID init failed: {e} — USB will not be available");
            None
        }
    };

    // Step 4: Wait for MiniDSP device (up to 30s)
    if let Some(ref bus) = bus {
        match bus.wait_for_device(Duration::from_secs(30)) {
            Ok((_vid, _pid)) => {
                match device::DeviceState::probe(bus.clone()) {
                    Ok(state) => {
                        info!(
                            "device: {} (hw_id={}, dsp={})",
                            state.product_name(),
                            state.hw_id().hw_id,
                            state.dsp_version()
                        );
                        // Update ezBEQ state with actual device channel counts
                        if let Some(table) = state.profile().dsp_table {
                            let inputs = table.inputs.len() as u8;
                            let outputs = table.outputs.len() as u8;
                            *beq_state.lock().unwrap() = ezbeq::EzBeqState::new(inputs, outputs);
                        }
                        *device_state.lock().unwrap() = Some(Arc::new(state));
                    }
                    Err(e) => error!("device probe failed: {e}"),
                }
            }
            Err(e) => warn!("No MiniDSP device found: {e} — will retry in background"),
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
        match http_server::start(device_state.clone(), 5380, nvs.clone(), None, Some(beq_state.clone())) {
            Ok(server) => {
                info!("HTTP server running on port 5380");
                Box::leak(Box::new(server));
            }
            Err(e) => error!("HTTP server failed: {e}"),
        }
    }

    // Step 7: TCP bridge (port 5333) — Device Console compatibility
    if wifi_is_sta {
        let tcp_state = device_state.clone();
        if let Err(e) = std::thread::Builder::new()
            .name("tcp_brg".into())
            .stack_size(16384)
            .spawn(move || {
                tcp_bridge(tcp_state);
            })
        {
            error!("failed to spawn TCP bridge: {e}");
        }
    }

    // Step 8: UDP discovery broadcast (port 3999)
    if wifi_is_sta {
        let udp_state = device_state.clone();
        if let Err(e) = std::thread::Builder::new()
            .name("udp_disc".into())
            .stack_size(8192)
            .spawn(move || {
                udp_discovery(udp_state);
            })
        {
            error!("failed to spawn UDP discovery: {e}");
        }
    }

    // Step 9: Start WiFi monitor thread (auto-reconnect on disconnect)
    if let Some(handle) = wifi_handle {
        if handle.is_sta {
            let wifi_mutex = Arc::new(std::sync::Mutex::new(handle.wifi));
            wifi::start_monitor(wifi_mutex);

            // Background catalogue update (initial check after WiFi is stable, then every 24h)
            if let Err(e) = updater::start_background_updater(nvs.clone()) {
                error!("failed to spawn update thread: {e}");
            }
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

    // Main loop: diagnostics tick + USB device detection/reconnection
    let mut usb_backoff = Duration::from_secs(5);
    loop {
        diagnostics::tick();

        // Check for USB device — handles both initial detection and reconnection
        if let Some(ref bus) = bus {
            let has_device = device_state.lock().unwrap().is_some();

            if has_device && !bus.is_connected() {
                // Device was connected but is now gone
                warn!("USB device lost — clearing device state");
                *device_state.lock().unwrap() = None;
            }

            // Scan if we don't have a working device (regardless of is_connected,
            // since non-MiniDSP devices can set the flag)
            if device_state.lock().unwrap().is_none() {
                info!("USB: scanning for device (timeout: {usb_backoff:?})");
                match bus.wait_for_device(usb_backoff) {
                    Ok((_vid, _pid)) => {
                        match device::DeviceState::probe(bus.clone()) {
                            Ok(state) => {
                                info!(
                                    "USB device found: {} (hw_id={}, dsp={})",
                                    state.product_name(),
                                    state.hw_id().hw_id,
                                    state.dsp_version()
                                );
                                *device_state.lock().unwrap() = Some(Arc::new(state));
                                usb_backoff = Duration::from_secs(5);
                            }
                            Err(e) => error!("USB device probe failed: {e}"),
                        }
                    }
                    Err(_) => {
                        // No device found within timeout — back off
                        usb_backoff = (usb_backoff * 2).min(Duration::from_secs(60));
                    }
                }
            }
        }

        std::thread::sleep(Duration::from_secs(5));
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

/// TCP bridge server on port 5333 — Device Console compatibility.
/// Accepts one connection at a time, forwards length-prefixed frames to/from the device.
/// Wire format: client sends [len][payload][checksum], server responds [len][payload] (no checksum).
fn tcp_bridge(state: http_server::SharedDeviceState) {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = match TcpListener::bind("0.0.0.0:5333") {
        Ok(l) => l,
        Err(e) => {
            log::error!("TCP bridge: failed to bind port 5333: {e}");
            return;
        }
    };
    log::info!("TCP bridge listening on port 5333");

    for stream in listener.incoming() {
        let mut stream = match stream {
            Ok(s) => s,
            Err(e) => {
                log::warn!("TCP bridge: accept error: {e}");
                continue;
            }
        };
        log::info!("TCP bridge: client connected from {:?}", stream.peer_addr());

        // Set read timeout to detect disconnects
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(30)));

        // Persistent read buffer for TCP stream reassembly (handles zero-padding from WI-DG)
        let mut read_buf = Vec::with_capacity(256);
        let mut tmp = [0u8; 128];

        loop {
            // Try to decode a frame from buffered data first
            if let Some((payload, consumed)) = minidsp_protocol::packet::decode_tcp_frame(&read_buf) {
                read_buf.drain(..consumed);

                // Forward to device
                let dev = match http_server::get_device(&state) {
                    Some(d) => d,
                    None => {
                        log::warn!("TCP bridge: no device connected");
                        // Don't break — keep connection alive, device may reconnect
                        continue;
                    }
                };

                match dev.raw_command(&payload) {
                    Ok(resp) => {
                        // TCP response: [len][payload] — no checksum (server responses per protocol)
                        let resp_frame = minidsp_protocol::packet::encode_tcp_response(&resp);
                        if stream.write_all(&resp_frame).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        log::warn!("TCP bridge: device error: {e}");
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
        log::info!("TCP bridge: client disconnected");
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

    if let Some(dev) = http_server::get_device(state) {
        let hw = dev.hw_id();
        return Some(minidsp_protocol::discovery::DiscoveryPacket::from_device(
            ip,
            hw.hw_id,
            hw.fw_major,
            hw.fw_minor,
            dev.dsp_version(),
            dev.serial(),
            format!("minidsp-{:08x}", dev.serial()),
        ));
    }

    let serial = fallback_discovery_serial();
    Some(minidsp_protocol::discovery::DiscoveryPacket::from_device(
        ip,
        0,
        0,
        1,
        0,
        serial,
        format!("minidsp-bridge-{serial}"),
    ))
}

fn fallback_discovery_serial() -> u32 {
    let mac = get_sta_mac().unwrap_or([0, 0, 0, 0, 0, 0]);
    900_000 + ((mac[3] as u32) << 16) + ((mac[4] as u32) << 8) + mac[5] as u32
}

/// Get the ESP32's STA IP address.
fn get_sta_ip() -> Option<[u8; 4]> {
    unsafe {
        let netif = esp_idf_svc::sys::esp_netif_get_handle_from_ifkey(
            b"WIFI_STA_DEF\0".as_ptr() as *const _,
        );
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
