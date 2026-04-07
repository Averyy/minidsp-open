//! HTTP server for ESP32-S3 using EspHttpServer (ESP-IDF httpd).
//!
//! Serves the same API as the desktop daemon. All handlers are blocking
//! (EspHttpServer runs in its own FreeRTOS task pool).
//!
//! Debug endpoints:
//!   GET  /debug/info  — JSON system diagnostics
//!   GET  /debug/log   — ring buffer log snapshot
//!   POST /debug/level — change runtime log level

use crate::device::DeviceState;
use crate::diagnostics;
use esp_idf_svc::http::server::{Configuration, EspHttpServer};
use esp_idf_svc::http::Method;
use esp_idf_svc::io::Write;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use log::info;
use minidsp_protocol::status::StatusSummary;
use std::sync::{Arc, Mutex};

/// Shared device state — None when USB disconnected, Some when connected.
pub type SharedDeviceState = Arc<Mutex<Option<Arc<DeviceState>>>>;

/// Start the HTTP server with all routes.
/// `port` should be 80 for SoftAP provisioning (captive portal), 5380 for normal STA mode.
/// `ap_ip` is the SoftAP IP for captive portal redirects (None in STA mode).
/// `beq_state` is the shared ezBEQ slot state (None in SoftAP mode).
pub fn start(
    state: SharedDeviceState,
    port: u16,
    nvs: EspDefaultNvsPartition,
    ap_ip: Option<std::net::Ipv4Addr>,
    beq_state: Option<crate::ezbeq::SharedEzBeqState>,
) -> Result<EspHttpServer<'static>, Box<dyn std::error::Error>> {
    let config = Configuration {
        http_port: port,
        // STA mode with ezBEQ enabled currently registers 58 handlers:
        // core device routes, debug routes, WS endpoints, ezBEQ API, UI/static
        // routes, and CORS/catch-all handlers. Keep some headroom so the HTTP
        // server doesn't fail with ESP_ERR_HTTPD_HANDLERS_FULL as routes evolve.
        max_uri_handlers: 64,
        stack_size: 16384,
        max_open_sockets: 13,
        uri_match_wildcard: true,
        ..Default::default()
    };
    let mut server = EspHttpServer::new(&config)?;

    // GET /devices — list connected devices
    {
        let st = state.clone();
        let port_copy = port;
        let ap_ip_copy = ap_ip;
        server.fn_handler("/devices", Method::Get, move |req| {
            let body = match get_device(&st) {
                Some(dev) => {
                    let info = dev.device_info();
                    let ws_url = esp32_device_ws_url(port_copy, ap_ip_copy);
                    serde_json::to_string(&serde_json::json!([{
                        "url": ws_url,
                        "product_name": info.product_name,
                        "version": {
                            "hw_id": info.version.hw_id,
                            "fw_major": info.version.fw_major,
                            "fw_minor": info.version.fw_minor,
                            "dsp_version": info.version.dsp_version,
                            "serial": info.version.serial,
                        }
                    }])).unwrap_or_else(|_| "[]".into())
                }
                None => "[]".into(),
            };
            send_json(req, 200, &body)
        })?;
    }

    // GET /devices/0 — device status (+ WebSocket compat polling when WS enabled)
    // When WS is enabled, this path is registered as ws_handler in register_ws_routes
    // to support both regular GET and WebSocket upgrade on the same path.
    #[cfg(not(esp_idf_httpd_ws_support))]
    {
        let st = state.clone();
        server.fn_handler("/devices/0", Method::Get, move |req| {
            let dev = match get_device(&st) {
                Some(d) => d,
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
            };
            match dev.get_status_summary() {
                Ok(status) => send_json(req, 200, &status_json(&status)),
                Err(e) => send_error(req, 500, &e.to_string()),
            }
        })?;
    }

    // POST /devices/0 — set master status
    {
        let st = state.clone();
        server.fn_handler("/devices/0", Method::Post, move |mut req| {
            let dev = match get_device(&st) {
                Some(d) => d,
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
            };
            let mut buf = [0u8; 1024];
            let len = req.read(&mut buf).unwrap_or(0);
            let json: serde_json::Value = match serde_json::from_slice(&buf[..len]) {
                Ok(v) => v,
                Err(_) => return send_error(req, 400, "invalid JSON"),
            };

            if let Some(vol) = json.get("volume").and_then(|v| v.as_f64()) {
                if let Err(e) = dev.set_volume_db(vol as f32) {
                    return send_error(req, 500, &format!("set_volume: {e}"));
                }
            }
            if let Some(mute) = json.get("mute").and_then(|v| v.as_bool()) {
                if let Err(e) = dev.set_mute(mute) {
                    return send_error(req, 500, &format!("set_mute: {e}"));
                }
            }
            if let Some(source) = json.get("source").and_then(|v| v.as_str()) {
                let src = match minidsp_protocol::source::Source::from_str_pascal(source)
                    .or_else(|| minidsp_protocol::source::Source::from_str_lowercase(source))
                {
                    Some(s) => s,
                    None => return send_error(req, 400, &format!("unknown source '{source}'")),
                };
                match dev.profile().sources.index_for_source(src) {
                    Some(idx) => {
                        if let Err(e) = dev.set_source(idx) {
                            return send_error(req, 500, &format!("set_source: {e}"));
                        }
                    }
                    None => return send_error(req, 400, "source not available on this device"),
                }
            }
            if let Some(preset) = json.get("preset").and_then(|v| v.as_u64()) {
                if let Err(e) = dev.set_preset(preset as u8) {
                    return send_error(req, 500, &format!("set_preset: {e}"));
                }
            }
            if let Some(dirac) = json.get("dirac").and_then(|v| v.as_bool()) {
                match dev.set_dirac_bypass(!dirac) {
                    Ok(()) => {}
                    Err(e) if e.to_string().contains("unsupported") => {}
                    Err(e) => return send_error(req, 500, &format!("set_dirac: {e}")),
                }
            }

            match dev.get_status_summary() {
                Ok(status) => send_json(req, 200, &master_status_json(&status)),
                Err(e) => send_error(req, 500, &e.to_string()),
            }
        })?;
    }

    // Compatibility routes: volume direction
    register_volume_routes(&mut server, state.clone())?;

    // Source and preset routes
    register_source_routes(&mut server, state.clone())?;
    register_preset_routes(&mut server, state.clone())?;

    // POST /devices/0/config — full config write (PEQ, gain, mute, delay, polarity, master)
    {
        let st = state.clone();
        server.fn_handler("/devices/0/config", Method::Post, move |mut req| {
            let dev = match get_device(&st) {
                Some(d) => d,
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
            };
            let mut buf = [0u8; 8192];
            let len = req.read(&mut buf).unwrap_or(0);
            let json: serde_json::Value = match serde_json::from_slice(&buf[..len]) {
                Ok(v) => v,
                Err(_) => return send_error(req, 400, "invalid JSON"),
            };

            // master_status: {preset, volume, mute, source, dirac}
            if let Some(ms) = json.get("master_status") {
                if let Some(preset) = ms.get("preset").and_then(|v| v.as_u64()) {
                    if let Err(e) = dev.set_preset(preset as u8) {
                        return send_error(req, 500, &format!("set_preset: {e}"));
                    }
                }
                if let Some(volume) = ms.get("volume").and_then(|v| v.as_f64()) {
                    if let Err(e) = dev.set_volume_db(volume as f32) {
                        return send_error(req, 500, &format!("set_volume: {e}"));
                    }
                }
                if let Some(mute) = ms.get("mute").and_then(|v| v.as_bool()) {
                    if let Err(e) = dev.set_mute(mute) {
                        return send_error(req, 500, &format!("set_mute: {e}"));
                    }
                }
                if let Some(source) = ms.get("source").and_then(|v| v.as_str()) {
                    let src = match minidsp_protocol::source::Source::from_str_pascal(source)
                        .or_else(|| minidsp_protocol::source::Source::from_str_lowercase(source))
                    {
                        Some(s) => s,
                        None => return send_error(req, 400, &format!("unknown source '{source}'")),
                    };
                    match dev.profile().sources.index_for_source(src) {
                        Some(idx) => {
                            if let Err(e) = dev.set_source(idx) {
                                return send_error(req, 500, &format!("set_source: {e}"));
                            }
                        }
                        None => return send_error(req, 400, "source not available on this device"),
                    }
                }
                if let Some(dirac) = ms.get("dirac").and_then(|v| v.as_bool()) {
                    // Ignore Unsupported (non-Dirac device), propagate transport errors
                    match dev.set_dirac_bypass(!dirac) {
                        Ok(()) => {}
                        Err(e) if e.to_string().contains("unsupported") => {}
                        Err(e) => return send_error(req, 500, &format!("set_dirac: {e}")),
                    }
                }
            }

            let table = match dev.profile().dsp_table {
                Some(t) => t,
                None => return send_empty_200(req),
            };

            // inputs: [{index, peq: [{index, coeff, bypass}], gain, mute}]
            if let Some(inputs) = json.get("inputs").and_then(|v| v.as_array()) {
                for input in inputs {
                    let idx = match input.get("index").and_then(|v| v.as_u64()) {
                        Some(i) => i as usize,
                        None => return send_error(req, 400, "input missing 'index' field"),
                    };
                    if let Some(input_ch) = table.inputs.get(idx) {
                        // PEQ bands
                        if let Some(peqs) = input.get("peq").and_then(|v| v.as_array()) {
                            for peq in peqs {
                                let band_idx = match peq.get("index").and_then(|v| v.as_u64()) {
                                    Some(i) => i as usize,
                                    None => return send_error(req, 400, "peq missing 'index' field"),
                                };
                                if let Some(band) = input_ch.peq.get(band_idx) {
                                    if let Some(coeff) = peq.get("coeff") {
                                        let c = [
                                            coeff.get("b0").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
                                            coeff.get("b1").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
                                            coeff.get("b2").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
                                            coeff.get("a1").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
                                            coeff.get("a2").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
                                        ];
                                        if c.iter().any(|v| !v.is_finite()) {
                                            return send_error(req, 400, "biquad coefficients must be finite");
                                        }
                                        if let Err(e) = dev.write_biquad(band.addr, &c) {
                                            return send_error(req, 500, &format!("write_biquad: {e}"));
                                        }
                                    }
                                    if let Some(bypass) = peq.get("bypass").and_then(|v| v.as_bool()) {
                                        if let Err(e) = dev.bypass_filter(band.addr, bypass) {
                                            return send_error(req, 500, &format!("bypass_filter: {e}"));
                                        }
                                    }
                                }
                            }
                        }
                        // Per-channel gain
                        if let Some(gain) = input.get("gain").and_then(|v| v.as_f64()) {
                            if !gain.is_finite() || gain.abs() > 120.0 {
                                return send_error(req, 400, "gain out of range (must be finite, |gain| <= 120 dB)");
                            }
                            if let Some(gain_addr) = input_ch.gain {
                                let linear = 10.0f32.powf(gain as f32 / 20.0);
                                if let Err(e) = dev.write_dsp(gain_addr, linear, true) {
                                    return send_error(req, 500, &format!("set_gain: {e}"));
                                }
                            }
                        }
                        // Per-channel mute
                        if let Some(mute) = input.get("mute").and_then(|v| v.as_bool()) {
                            if let Some(mute_addr) = input_ch.mute {
                                let val = if mute { 2.0 } else { 1.0 };
                                if let Err(e) = dev.write_dsp(mute_addr, val, true) {
                                    return send_error(req, 500, &format!("set_mute: {e}"));
                                }
                            }
                        }
                    }
                }
            }

            // outputs: [{index, peq: [...], gain, mute, delay, invert}]
            if let Some(outputs) = json.get("outputs").and_then(|v| v.as_array()) {
                for output in outputs {
                    let idx = match output.get("index").and_then(|v| v.as_u64()) {
                        Some(i) => i as usize,
                        None => return send_error(req, 400, "output missing 'index' field"),
                    };
                    if let Some(output_ch) = table.outputs.get(idx) {
                        // PEQ bands
                        if let Some(peqs) = output.get("peq").and_then(|v| v.as_array()) {
                            for peq in peqs {
                                let band_idx = match peq.get("index").and_then(|v| v.as_u64()) {
                                    Some(i) => i as usize,
                                    None => return send_error(req, 400, "peq missing 'index' field"),
                                };
                                if let Some(band) = output_ch.peq.get(band_idx) {
                                    if let Some(coeff) = peq.get("coeff") {
                                        let c = [
                                            coeff.get("b0").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
                                            coeff.get("b1").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
                                            coeff.get("b2").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
                                            coeff.get("a1").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
                                            coeff.get("a2").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
                                        ];
                                        if c.iter().any(|v| !v.is_finite()) {
                                            return send_error(req, 400, "biquad coefficients must be finite");
                                        }
                                        if let Err(e) = dev.write_biquad(band.addr, &c) {
                                            return send_error(req, 500, &format!("write_biquad: {e}"));
                                        }
                                    }
                                    if let Some(bypass) = peq.get("bypass").and_then(|v| v.as_bool()) {
                                        if let Err(e) = dev.bypass_filter(band.addr, bypass) {
                                            return send_error(req, 500, &format!("bypass_filter: {e}"));
                                        }
                                    }
                                }
                            }
                        }
                        // Per-channel gain
                        if let Some(gain) = output.get("gain").and_then(|v| v.as_f64()) {
                            if !gain.is_finite() || gain.abs() > 120.0 {
                                return send_error(req, 400, "gain out of range (must be finite, |gain| <= 120 dB)");
                            }
                            let linear = 10.0f32.powf(gain as f32 / 20.0);
                            if let Err(e) = dev.write_dsp(output_ch.gain, linear, true) {
                                return send_error(req, 500, &format!("set_gain: {e}"));
                            }
                        }
                        // Per-channel mute
                        if let Some(mute) = output.get("mute").and_then(|v| v.as_bool()) {
                            let val = if mute { 2.0 } else { 1.0 };
                            if let Err(e) = dev.write_dsp(output_ch.mute, val, true) {
                                return send_error(req, 500, &format!("set_mute: {e}"));
                            }
                        }
                        // Delay (Duration format: {secs, nanos} or float seconds)
                        if let Some(delay) = output.get("delay") {
                            let secs = if let Some(s) = delay.get("secs").and_then(|v| v.as_f64()) {
                                let nanos = delay.get("nanos").and_then(|v| v.as_f64()).unwrap_or(0.0);
                                s + nanos / 1_000_000_000.0
                            } else {
                                delay.as_f64().unwrap_or(0.0)
                            };
                            let sample_rate = dev.profile().sample_rate as f32;
                            let samples = (secs as f32 * sample_rate).round().clamp(0.0, 48000.0);
                            if let Err(e) = dev.write_dsp(output_ch.delay, samples, true) {
                                return send_error(req, 500, &format!("set_delay: {e}"));
                            }
                        }
                        // Polarity inversion (dialect-aware encoding)
                        if let Some(invert) = output.get("invert").and_then(|v| v.as_bool()) {
                            if let Err(e) = dev.set_invert(output_ch.polarity, invert) {
                                return send_error(req, 500, &format!("set_polarity: {e}"));
                            }
                        }
                    }
                }
            }

            send_empty_200(req)
        })?;
    }

    // Display, DRE, tokens, preset copy
    register_device_feature_routes(&mut server, state.clone())?;

    // Debug / diagnostics endpoints
    register_debug_routes(&mut server, state.clone(), nvs.clone())?;

    // WebSocket for status streaming (gated on ESP-IDF httpd WS support)
    #[cfg(esp_idf_httpd_ws_support)]
    register_ws_routes(&mut server, state.clone())?;

    // ezBEQ REST API + WebSocket (only in STA mode with beq_state)
    if let Some(ref beq) = beq_state {
        crate::ezbeq::register_routes(&mut server, state.clone(), beq.clone())?;
        #[cfg(esp_idf_httpd_ws_support)]
        register_ezbeq_ws(&mut server, state.clone(), beq.clone())?;
    }

    // GET /llms.txt
    server.fn_handler("/llms.txt", Method::Get, |req| {
        send_text(req, "# minidsp-open\nMiniDSP audio processor controller.\nHTTP API on port 5380.\n")
    })?;

    // CORS preflight handler — responds to OPTIONS on any path
    server.fn_handler("/*", Method::Options, |req| {
        send_cors_preflight(req)
    })?;

    // Root page — provisioning in SoftAP mode, serve UI or redirect in STA mode
    if ap_ip.is_some() {
        server.fn_handler("/", Method::Get, |req| {
            info!("serving provisioning page");
            send_html(req, crate::wifi::PROVISION_HTML)
        })?;
    } else {
        server.fn_handler("/", Method::Get, |req| {
            // Serve ezBEQ UI index.html if available, otherwise redirect to API
            let index = crate::storage::full_path("ui/index.html");
            if std::path::Path::new(&index).exists() {
                send_static_file(req, &index)
            } else {
                send_redirect(req, "/devices")
            }
        })?;
    }

    // POST /api/wifi/connect — WiFi provisioning form handler
    server.fn_handler("/api/wifi/connect", Method::Post, move |mut req| {
        let mut buf = [0u8; 256];
        let len = req.read(&mut buf).unwrap_or(0);
        let body = std::str::from_utf8(&buf[..len]).unwrap_or("");

        // Parse form-encoded body: ssid=...&password=...
        let mut ssid = String::new();
        let mut password = String::new();
        for pair in body.split('&') {
            if let Some((key, value)) = pair.split_once('=') {
                let decoded = urldecode(value);
                match key {
                    "ssid" => ssid = decoded,
                    "password" => password = decoded,
                    _ => {}
                }
            }
        }

        if ssid.is_empty() {
            return send_provision_error(req, "SSID is required");
        }

        // Save credentials to NVS (partition passed from main — not ::take())
        if let Err(e) = crate::wifi::save_credentials(&nvs, &ssid, &password) {
            return send_provision_error(req, &format!("Save failed: {e}"));
        }

        // Return success page, then restart
        let html = "<!DOCTYPE html><html><head><meta charset=\"utf-8\">\
                    <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
                    <title>WiFi Saved</title>\
                    <style>body{font-family:system-ui;max-width:400px;margin:40px auto;\
                    padding:0 20px;background:#1a1a2e;color:#e0e0e0}</style>\
                    </head><body>\
                    <h2 style=\"color:#53c28b\">WiFi credentials saved!</h2>\
                    <p>Device will restart and connect to your network.</p>\
                    </body></html>";
        let headers = [("Content-Type", "text/html")];
        let mut resp = req.into_response(200, None, &headers)?;
        resp.write_all(html.as_bytes())?;
        // Schedule restart after response is sent
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(2));
            unsafe { esp_idf_svc::sys::esp_restart() };
        });
        Ok(())
    })?;

    // Catch-all GET handler:
    // - SoftAP mode: redirect to captive portal
    // - STA mode: serve static files from LittleFS /data/ui/ (ezBEQ React SPA)
    // ESP-IDF httpd matches exact URIs first, so this won't shadow registered routes.
    if let Some(ip) = ap_ip {
        let portal_url = format!("http://{ip}/");
        server.fn_handler("/*", Method::Get, move |req| {
            let uri = req.uri().to_string();
            info!("captive portal redirect: {uri} → {portal_url}");
            send_redirect(req, &portal_url)
        })?;
    } else {
        server.fn_handler("/*", Method::Get, |req| {
            let uri = req.uri();
            // Strip query string for file path lookup
            let path = uri.split('?').next().unwrap_or(uri);

            // Never serve HTML for API paths — return 404 JSON instead
            if path.starts_with("/api/") || path.starts_with("/devices") || path.starts_with("/debug") {
                return send_json(req, 404, r#"{"error":"not found"}"#);
            }

            // Path traversal protection: reject ".." in any encoding
            if path.contains("..") || path.contains("%2e") || path.contains("%2E")
                || path.contains('\0') || path.contains("%00")
            {
                return send_json(req, 400, r#"{"error":"bad path"}"#);
            }

            let file_path = crate::storage::full_path(&format!("ui{path}"));

            if std::path::Path::new(&file_path).is_file() {
                send_static_file(req, &file_path)
            } else {
                // SPA fallback: serve index.html for client-side routing
                let index = crate::storage::full_path("ui/index.html");
                if std::path::Path::new(&index).exists() {
                    send_static_file(req, &index)
                } else {
                    send_json(req, 404, r#"{"error":"not found"}"#)
                }
            }
        })?;
    }

    info!("HTTP routes registered");
    Ok(server)
}

fn esp32_device_ws_url(port: u16, ap_ip: Option<std::net::Ipv4Addr>) -> String {
    let host = if let Some(ip) = ap_ip {
        ip.to_string()
    } else if let Some(ip) = get_sta_ip_string() {
        ip
    } else {
        "127.0.0.1".to_string()
    };
    format!("ws://{host}:{port}/devices/0/ws")
}

fn get_sta_ip_string() -> Option<String> {
    use esp_idf_svc::sys::{esp_netif_get_handle_from_ifkey, esp_netif_get_ip_info, esp_netif_ip_info_t};

    unsafe {
        let netif = esp_netif_get_handle_from_ifkey(b"WIFI_STA_DEF\0".as_ptr() as _);
        if netif.is_null() {
            return None;
        }
        let mut info = esp_netif_ip_info_t::default();
        if esp_netif_get_ip_info(netif, &mut info) != 0 {
            return None;
        }
        let octets = info.ip.addr.to_le_bytes();
        Some(format!("{}.{}.{}.{}", octets[0], octets[1], octets[2], octets[3]))
    }
}

// ---------------------------------------------------------------------------
// Debug / diagnostics routes
// ---------------------------------------------------------------------------

fn register_debug_routes(
    server: &mut EspHttpServer<'static>,
    state: SharedDeviceState,
    nvs: EspDefaultNvsPartition,
) -> Result<(), esp_idf_svc::sys::EspError> {
    // GET /debug/info — JSON system diagnostics
    {
        let st = state.clone();
        server.fn_handler("/debug/info", Method::Get, move |req| {
            let dev = get_device(&st);
            let info = diagnostics::SystemInfo::collect(dev.as_deref());
            let body = serde_json::to_string(&info).unwrap_or_default();
            send_json(req, 200, &body)
        })?;
    }

    // GET /debug/log — ring buffer log snapshot
    server.fn_handler("/debug/log", Method::Get, |req| {
        let snapshot = diagnostics::log_snapshot();
        send_text(req, &snapshot)
    })?;

    // POST /debug/level — change runtime log level
    server.fn_handler("/debug/level", Method::Post, |mut req| {
        let mut buf = [0u8; 128];
        let len = req.read(&mut buf).unwrap_or(0);

        let level_str = if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&buf[..len])
        {
            json.get("level")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        } else {
            // Try plain text body
            std::str::from_utf8(&buf[..len]).unwrap_or("").trim().to_string()
        };

        match diagnostics::set_log_level(&level_str) {
            Some(name) => {
                let body = format!(r#"{{"level":"{name}"}}"#);
                send_json(req, 200, &body)
            }
            None => send_error(req, 400, "invalid level (use: error, warn, info, debug, trace)"),
        }
    })?;

    // GET /debug/updater — current updater config/status
    {
        let nvs = nvs.clone();
        server.fn_handler("/debug/updater", Method::Get, move |req| {
            let body = serde_json::to_string(&serde_json::json!({
                "baseUrl": crate::updater::base_url(&nvs),
                "installedVersion": crate::updater::installed_version(&nvs),
                "hasCatalogueData": crate::updater::has_catalogue_data(),
            }))
            .unwrap_or_default();
            send_json(req, 200, &body)
        })?;
    }

    // POST /debug/updater — set or clear updater base URL override
    {
        let nvs = nvs.clone();
        server.fn_handler("/debug/updater", Method::Post, move |mut req| {
            let mut buf = [0u8; 512];
            let len = req.read(&mut buf).unwrap_or(0);
            let json: serde_json::Value = match serde_json::from_slice(&buf[..len]) {
                Ok(v) => v,
                Err(_) => return send_error(req, 400, "invalid JSON"),
            };

            let url = json
                .get("baseUrl")
                .or_else(|| json.get("base_url"))
                .and_then(|v| v.as_str())
                .unwrap_or("");

            match crate::updater::set_base_url(&nvs, url) {
                Ok(active_url) => {
                    let body = serde_json::to_string(&serde_json::json!({
                        "status": "ok",
                        "baseUrl": active_url,
                    }))
                    .unwrap_or_default();
                    send_json(req, 200, &body)
                }
                Err(e) => send_error(req, 400, &e.to_string()),
            }
        })?;
    }

    // POST /debug/updater/check — run update check immediately
    {
        let nvs = nvs.clone();
        server.fn_handler("/debug/updater/check", Method::Post, move |req| {
            let result = crate::updater::check_and_update(&nvs);
            let body = match result {
                crate::updater::UpdateResult::Current => serde_json::json!({
                    "status": "current",
                    "baseUrl": crate::updater::base_url(&nvs),
                }),
                crate::updater::UpdateResult::Updated { version, files } => serde_json::json!({
                    "status": "updated",
                    "baseUrl": crate::updater::base_url(&nvs),
                    "version": version,
                    "files": files,
                }),
                crate::updater::UpdateResult::NoStorage => serde_json::json!({
                    "status": "no_storage",
                    "baseUrl": crate::updater::base_url(&nvs),
                }),
                crate::updater::UpdateResult::Error(error) => serde_json::json!({
                    "status": "error",
                    "baseUrl": crate::updater::base_url(&nvs),
                    "error": error,
                }),
            };
            send_json(req, 200, &serde_json::to_string(&body).unwrap_or_default())
        })?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Device control routes
// ---------------------------------------------------------------------------

fn register_volume_routes(
    server: &mut EspHttpServer<'static>,
    state: SharedDeviceState,
) -> Result<(), esp_idf_svc::sys::EspError> {
    for dir in &["up", "down", "toggle_mute"] {
        let st = state.clone();
        let direction = dir.to_string();
        let path = format!("/devices/0/volume/{dir}");
        server.fn_handler(&path, Method::Post, move |req| {
            let dev = match get_device(&st) {
                Some(d) => d,
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
            };
            let ms = match dev.get_master_status() {
                Ok(ms) => ms,
                Err(e) => return send_error(req, 500, &e.to_string()),
            };
            let result = match direction.as_str() {
                "up" => dev.set_volume(ms.volume.saturating_sub(1)),
                "down" => dev.set_volume(ms.volume.saturating_add(1)),
                "toggle_mute" => dev.set_mute(!ms.is_muted()),
                _ => Ok(()),
            };
            if let Err(e) = result {
                return send_error(req, 500, &e.to_string());
            }
            match dev.get_status_summary() {
                Ok(status) => send_json(req, 200, &master_status_json(&status)),
                Err(e) => send_error(req, 500, &e.to_string()),
            }
        })?;
    }
    Ok(())
}

/// Register source routes for all known source names. The source is resolved
/// dynamically from the device profile at request time, so this works even if the
/// device wasn't connected at boot.
fn register_source_routes(
    server: &mut EspHttpServer<'static>,
    state: SharedDeviceState,
) -> Result<(), esp_idf_svc::sys::EspError> {
    // Register routes for all common MiniDSP sources (covers all device types)
    for name in &["analog", "toslink", "spdif", "usb", "aesebu", "hdmi"] {
        let st = state.clone();
        let source_name = name.to_string();
        let path = format!("/devices/0/source/{name}");
        server.fn_handler(&path, Method::Post, move |req| {
            let dev = match get_device(&st) {
                Some(d) => d,
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
            };
            // Resolve source name to index via the device profile
            let src = match minidsp_protocol::source::Source::from_str_lowercase(&source_name) {
                Some(s) => s,
                None => return send_error(req, 400, "unknown source"),
            };
            let idx = match dev.profile().sources.index_for_source(src) {
                Some(i) => i,
                None => return send_error(req, 400, "source not available on this device"),
            };
            if let Err(e) = dev.set_source(idx) {
                return send_error(req, 500, &e.to_string());
            }
            match dev.get_status_summary() {
                Ok(status) => send_json(req, 200, &master_status_json(&status)),
                Err(e) => send_error(req, 500, &e.to_string()),
            }
        })?;
    }
    Ok(())
}

fn register_preset_routes(
    server: &mut EspHttpServer<'static>,
    state: SharedDeviceState,
) -> Result<(), esp_idf_svc::sys::EspError> {
    for preset in 0..=3u8 {
        let st = state.clone();
        let path = format!("/devices/0/preset/{preset}");
        server.fn_handler(&path, Method::Post, move |req| {
            let dev = match get_device(&st) {
                Some(d) => d,
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
            };
            if let Err(e) = dev.set_preset(preset) {
                return send_error(req, 500, &e.to_string());
            }
            match dev.get_status_summary() {
                Ok(status) => send_json(req, 200, &master_status_json(&status)),
                Err(e) => send_error(req, 500, &e.to_string()),
            }
        })?;
    }
    Ok(())
}

fn register_device_feature_routes(
    server: &mut EspHttpServer<'static>,
    state: SharedDeviceState,
) -> Result<(), esp_idf_svc::sys::EspError> {
    // GET /devices/0/display
    {
        let st = state.clone();
        server.fn_handler("/devices/0/display", Method::Get, move |req| {
            let dev = match get_device(&st) {
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
                Some(d) if !d.profile().has_display => {
                    return send_json(req, 404, r#"{"error":"not supported"}"#)
                }
                Some(d) => d,
            };
            match dev.read_display() {
                Ok((idle_time, brightness)) => {
                    let body = format!(r#"{{"idle_time":{idle_time},"brightness":{brightness}}}"#);
                    send_json(req, 200, &body)
                }
                Err(e) => send_error(req, 500, &e.to_string()),
            }
        })?;
    }

    // POST /devices/0/display
    {
        let st = state.clone();
        server.fn_handler("/devices/0/display", Method::Post, move |mut req| {
            let dev = match get_device(&st) {
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
                Some(d) if !d.profile().has_display => {
                    return send_json(req, 404, r#"{"error":"not supported"}"#)
                }
                Some(d) => d,
            };
            let mut buf = [0u8; 256];
            let len = req.read(&mut buf).unwrap_or(0);
            if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&buf[..len]) {
                if let Some(b) = json.get("brightness").and_then(|v| v.as_u64()) {
                    let _ = dev.set_display_brightness(b as u8);
                }
                if let Some(t) = json.get("idle_time").and_then(|v| v.as_u64()) {
                    let _ = dev.set_display_idle_time(t as u8);
                }
            }
            send_json(req, 200, r#"{"status":"ok"}"#)
        })?;
    }

    // GET /devices/0/dre
    {
        let st = state.clone();
        server.fn_handler("/devices/0/dre", Method::Get, move |req| {
            let dev = match get_device(&st) {
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
                Some(d) if !d.profile().has_dre => {
                    return send_json(req, 404, r#"{"error":"not supported"}"#)
                }
                Some(d) => d,
            };
            match dev.read_dre() {
                Ok(enabled) => {
                    let body = format!(r#"{{"enabled":{enabled}}}"#);
                    send_json(req, 200, &body)
                }
                Err(e) => send_error(req, 500, &e.to_string()),
            }
        })?;
    }

    // POST /devices/0/dre
    {
        let st = state.clone();
        server.fn_handler("/devices/0/dre", Method::Post, move |mut req| {
            let dev = match get_device(&st) {
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
                Some(d) if !d.profile().has_dre => {
                    return send_json(req, 404, r#"{"error":"not supported"}"#)
                }
                Some(d) => d,
            };
            let mut buf = [0u8; 256];
            let len = req.read(&mut buf).unwrap_or(0);
            if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&buf[..len]) {
                if let Some(enabled) = json.get("enabled").and_then(|v| v.as_bool()) {
                    let _ = dev.set_dre(enabled);
                }
            }
            send_json(req, 200, r#"{"status":"ok"}"#)
        })?;
    }

    // GET /devices/0/tokens
    {
        let st = state.clone();
        server.fn_handler("/devices/0/tokens", Method::Get, move |req| {
            let dev = match get_device(&st) {
                Some(d) => d,
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
            };
            match dev.read_tokens() {
                Ok(tokens) => {
                    let body = serde_json::to_string(&serde_json::json!({"tokens": tokens}))
                        .unwrap_or_default();
                    send_json(req, 200, &body)
                }
                Err(e) => send_error(req, 500, &e.to_string()),
            }
        })?;
    }

    // POST /devices/0/preset/copy
    {
        let st = state.clone();
        server.fn_handler("/devices/0/preset/copy", Method::Post, move |req| {
            let dev = match get_device(&st) {
                Some(d) => d,
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
            };
            match dev.copy_preset() {
                Ok(()) => send_json(req, 200, r#"{"status":"ok"}"#),
                Err(e) => send_error(req, 500, &e.to_string()),
            }
        })?;
    }

    Ok(())
}

// -- Helpers --

/// Extract the current device from shared state.
pub fn get_device(state: &SharedDeviceState) -> Option<Arc<DeviceState>> {
    state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

pub fn send_json<C: esp_idf_svc::http::server::Connection>(
    req: esp_idf_svc::http::server::Request<C>,
    status: u16,
    body: &str,
) -> Result<(), C::Error> {
    let headers = [
        ("Content-Type", "application/json"),
        ("Access-Control-Allow-Origin", "*"),
    ];
    let mut resp = req.into_response(status, None, &headers)?;
    resp.write_all(body.as_bytes())?;
    Ok(())
}

fn send_text<C: esp_idf_svc::http::server::Connection>(
    req: esp_idf_svc::http::server::Request<C>,
    text: &str,
) -> Result<(), C::Error> {
    let headers = [
        ("Content-Type", "text/plain; charset=utf-8"),
        ("Access-Control-Allow-Origin", "*"),
    ];
    let mut resp = req.into_response(200, None, &headers)?;
    resp.write_all(text.as_bytes())?;
    Ok(())
}

fn send_html<C: esp_idf_svc::http::server::Connection>(
    req: esp_idf_svc::http::server::Request<C>,
    html: &str,
) -> Result<(), C::Error> {
    let headers = [
        ("Content-Type", "text/html"),
        ("Access-Control-Allow-Origin", "*"),
    ];
    let mut resp = req.into_response(200, None, &headers)?;
    resp.write_all(html.as_bytes())?;
    Ok(())
}

fn send_redirect<C: esp_idf_svc::http::server::Connection>(
    req: esp_idf_svc::http::server::Request<C>,
    location: &str,
) -> Result<(), C::Error> {
    let headers = [
        ("Location", location),
        ("Content-Type", "text/html"),
        ("Cache-Control", "no-store"),
    ];
    // iOS requires a non-empty body to trigger captive portal detection.
    // Do NOT include the word "Success" — iOS treats it as a connectivity check pass.
    let body = format!(
        "<html><body>Redirecting to <a href=\"{loc}\">setup page</a></body></html>",
        loc = location
    );
    let mut resp = req.into_response(302, Some("Found"), &headers)?;
    resp.write_all(body.as_bytes())?;
    Ok(())
}

fn send_cors_preflight<C: esp_idf_svc::http::server::Connection>(
    req: esp_idf_svc::http::server::Request<C>,
) -> Result<(), C::Error> {
    let headers = [
        ("Access-Control-Allow-Origin", "*"),
        ("Access-Control-Allow-Methods", "GET, POST, PUT, DELETE, PATCH, OPTIONS"),
        ("Access-Control-Allow-Headers", "Content-Type"),
        ("Access-Control-Max-Age", "86400"),
    ];
    let mut resp = req.into_response(204, None, &headers)?;
    resp.write_all(b"")?;
    Ok(())
}

fn send_empty_200<C: esp_idf_svc::http::server::Connection>(
    req: esp_idf_svc::http::server::Request<C>,
) -> Result<(), C::Error> {
    let headers = [
        ("Access-Control-Allow-Origin", "*"),
    ];
    let mut resp = req.into_response(200, None, &headers)?;
    resp.write_all(b"")?;
    Ok(())
}

pub fn send_error<C: esp_idf_svc::http::server::Connection>(
    req: esp_idf_svc::http::server::Request<C>,
    status: u16,
    message: &str,
) -> Result<(), C::Error> {
    // Escape message for JSON
    let body = format!(r#"{{"error":"{}"}}"#, message.replace('"', "\\\""));
    send_json(req, status, &body)
}

/// HTML error page for provisioning form submissions (not JSON).
fn send_provision_error<C: esp_idf_svc::http::server::Connection>(
    req: esp_idf_svc::http::server::Request<C>,
    message: &str,
) -> Result<(), C::Error> {
    let escaped = message.replace('<', "&lt;").replace('>', "&gt;");
    let html = format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>Error</title>\
         <style>body{{font-family:system-ui;max-width:400px;margin:40px auto;\
         padding:0 20px;background:#1a1a2e;color:#e0e0e0}}\
         a{{color:#53c28b}}</style>\
         </head><body>\
         <h2 style=\"color:#e74c3c\">Error</h2>\
         <p>{escaped}</p>\
         <p><a href=\"/\">Back to setup</a></p>\
         </body></html>"
    );
    let headers = [("Content-Type", "text/html")];
    let mut resp = req.into_response(400, None, &headers)?;
    resp.write_all(html.as_bytes())?;
    Ok(())
}

/// Build the GET status JSON — uses StatusSummary's Serialize for correct f32 precision
/// and skip_serializing_if on dirac (omitted for non-Dirac devices).
fn status_json(status: &StatusSummary) -> String {
    serde_json::to_string(status).unwrap_or_default()
}

/// Build the POST response JSON — flat object with dirac:null included (per compat contract).
/// Note: POST responses must include dirac:null for non-Dirac devices, which differs from
/// GET responses that omit the field. This is why we can't use StatusSummary's Serialize here.
fn master_status_json(status: &StatusSummary) -> String {
    // Build JSON manually to preserve f32 precision (not f64 via json!())
    // and include dirac as null when absent
    let mut map = serde_json::Map::new();
    map.insert("preset".into(), serde_json::json!(status.master.preset));
    map.insert("source".into(), serde_json::Value::String(status.master.source.clone()));
    map.insert("volume".into(), serde_json::to_value(status.master.volume).unwrap_or_default());
    map.insert("mute".into(), serde_json::json!(status.master.mute));
    map.insert("dirac".into(), match status.master.dirac {
        Some(d) => serde_json::json!(d),
        None => serde_json::Value::Null,
    });
    serde_json::to_string(&map).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// WebSocket support (status streaming)
// ---------------------------------------------------------------------------

#[cfg(esp_idf_httpd_ws_support)]
fn register_ws_routes(
    server: &mut EspHttpServer<'static>,
    state: SharedDeviceState,
) -> Result<(), esp_idf_svc::sys::EspError> {
    use esp_idf_svc::http::server::ws::EspHttpWsConnection;
    use esp_idf_svc::ws::FrameType;
    use std::sync::atomic::{AtomicBool, Ordering};

    // -- Compat endpoint: /devices/0 (regular GET + WebSocket state/levels polling) --
    // ezBEQ and ecosystem clients connect here with ?levels=true&poll=true
    {
        // Track active WebSocket session (fd + stop flag) to avoid cross-connection interference
        struct ActiveWs {
            stop: Arc<AtomicBool>,
            fd: i32,
        }
        let active_ws: Arc<Mutex<Option<ActiveWs>>> = Arc::new(Mutex::new(None));

        let st = state.clone();
        let ws_ref = active_ws.clone();

        server.ws_handler("/devices/0", None, move |ws| {
            if ws.is_new() {
                // Extract the raw request pointer for header inspection and HTTP response
                let raw_req = match ws {
                    EspHttpWsConnection::New(_, req) => *req,
                    _ => return Ok::<(), esp_idf_svc::sys::EspError>(()),
                };

                // Detect WebSocket upgrade vs regular GET by checking for WS handshake header.
                // ESP-IDF httpd calls our handler for BOTH — with handshake (WS) or without (GET).
                if !unsafe { ws_is_upgrade(raw_req) } {
                    // Regular GET — send JSON status response via raw httpd API
                    let (status, json) = match get_device(&st) {
                        Some(dev) => match dev.get_status_summary() {
                            Ok(s) => (200, status_json(&s)),
                            Err(e) => (500, format!(r#"{{"error":"{}"}}"#, e.to_string().replace('"', "\\\""))),
                        },
                        None => (503, r#"{"error":"no device"}"#.to_string()),
                    };
                    unsafe { ws_send_http_json(raw_req, status, &json) };
                    return Ok(());
                }

                // WebSocket upgrade — parse query params for poll interval
                let (_poll, levels) = unsafe { ws_parse_query(raw_req) };
                let interval_ms: u64 = if levels { 250 } else { 1000 };

                // Stop any previous polling thread
                if let Ok(mut guard) = ws_ref.lock() {
                    if let Some(prev) = guard.take() {
                        prev.stop.store(true, Ordering::Relaxed);
                    }
                }

                // Create detached sender for push-based streaming
                match ws.create_detached_sender() {
                    Ok(sender) => {
                        let stop = Arc::new(AtomicBool::new(false));
                        let fd = ws.session();
                        if let Ok(mut guard) = ws_ref.lock() {
                            *guard = Some(ActiveWs { stop: stop.clone(), fd });
                        }

                        let dev_state = st.clone();
                        if let Err(e) = std::thread::Builder::new()
                            .name("ws_compat".into())
                            .stack_size(16384)
                            .spawn(move || {
                                ws_poll_loop(sender, dev_state, stop, interval_ms);
                            })
                        {
                            log::warn!("WebSocket compat: failed to spawn poll thread: {e}");
                        }
                    }
                    Err(e) => log::warn!("WebSocket compat: failed to create sender: {e:?}"),
                }
                return Ok(());
            }

            if ws.is_closed() {
                // Only stop poll thread if the closing fd matches the active WS session
                let closing_fd = ws.session();
                if let Ok(mut guard) = ws_ref.lock() {
                    if guard.as_ref().is_some_and(|a| a.fd == closing_fd) {
                        if let Some(active) = guard.take() {
                            active.stop.store(true, Ordering::Relaxed);
                        }
                    }
                }
                return Ok(());
            }

            // Handle incoming WebSocket frames
            let mut buf = [0u8; 256];
            match ws.recv(&mut buf) {
                Ok((FrameType::Text(_), _)) | Ok((FrameType::Binary(_), _)) => {
                    let json = match get_device(&st) {
                        Some(dev) => match dev.get_status_summary() {
                            Ok(status) => status_json(&status),
                            Err(_) => r#"{"error":"device read failed"}"#.to_string(),
                        },
                        None => r#"{"error":"no device"}"#.to_string(),
                    };
                    let _ = ws.send(FrameType::Text(false), json.as_bytes());
                }
                Ok((FrameType::Close, _)) => {
                    let _ = ws.send(FrameType::Close, &[]);
                }
                Ok((FrameType::Ping, _)) => {
                    let _ = ws.send(FrameType::Pong, &[]);
                }
                _ => {}
            }
            Ok(())
        })?;

        info!("WebSocket compat endpoint registered: /devices/0");
    }

    // -- Raw frame bridge: /devices/0/ws --
    {
        struct ActiveWs {
            stop: Arc<AtomicBool>,
            fd: i32,
        }
        let active_ws: Arc<Mutex<Option<ActiveWs>>> = Arc::new(Mutex::new(None));

        let st = state.clone();
        let ws_ref = active_ws.clone();

        server.ws_handler("/devices/0/ws", None, move |ws| {
            // Raw frame bridge — NO status polling thread. Only responds to binary frames.
            if ws.is_new() {
                log::info!("WS raw bridge: client connected");
                return Ok::<(), esp_idf_svc::sys::EspError>(());
            }

            if ws.is_closed() {
                log::info!("WS raw bridge: client disconnected");
                return Ok(());
            }

            let mut buf = [0u8; 256];
            match ws.recv(&mut buf) {
                Ok((FrameType::Binary(_), len)) => {
                    // Raw frame bridge: decode framed packet, forward payload to device
                    if let Some(payload) = minidsp_protocol::packet::decode_frame(&buf[..len]) {
                        match get_device(&st) {
                            Some(dev) => {
                                match dev.raw_command(&payload) {
                                    Ok(resp) => {
                                        let frame = minidsp_protocol::packet::encode_frame(&resp);
                                        let _ = ws.send(FrameType::Binary(false), &frame);
                                    }
                                    Err(e) => {
                                        log::warn!("WS raw bridge: device error: {e}");
                                    }
                                }
                            }
                            None => {
                                log::warn!("WS raw bridge: no device connected");
                            }
                        }
                    } else {
                        log::warn!("WS raw bridge: invalid frame ({len} bytes)");
                    }
                }
                Ok((FrameType::Text(_), _)) => {
                    // Text frames: return status JSON (compat with clients that send text)
                    let json = match get_device(&st) {
                        Some(dev) => match dev.get_status_summary() {
                            Ok(status) => status_json(&status),
                            Err(_) => r#"{"error":"device read failed"}"#.to_string(),
                        },
                        None => r#"{"error":"no device"}"#.to_string(),
                    };
                    let _ = ws.send(FrameType::Text(false), json.as_bytes());
                }
                Ok((FrameType::Close, _)) => {
                    let _ = ws.send(FrameType::Close, &[]);
                }
                Ok((FrameType::Ping, _)) => {
                    let _ = ws.send(FrameType::Pong, &[]);
                }
                _ => {}
            }
            Ok(())
        })?;

        info!("WebSocket raw bridge registered: /devices/0/ws");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// WebSocket FFI helpers — access raw httpd_req_t for dual GET/WS handling
// ---------------------------------------------------------------------------

/// Check if a request was a WebSocket upgrade (Sec-WebSocket-Key header present).
/// When ESP-IDF httpd calls a ws_handler for a regular GET (no upgrade), the handshake
/// is NOT performed and this returns false.
#[cfg(esp_idf_httpd_ws_support)]
unsafe fn ws_is_upgrade(raw_req: *mut esp_idf_svc::sys::httpd_req_t) -> bool {
    esp_idf_svc::sys::httpd_req_get_hdr_value_len(
        raw_req,
        b"Sec-WebSocket-Key\0".as_ptr(),
    ) > 0
}

/// Parse query parameters from the request URI for WebSocket poll configuration.
#[cfg(esp_idf_httpd_ws_support)]
unsafe fn ws_parse_query(raw_req: *mut esp_idf_svc::sys::httpd_req_t) -> (bool, bool) {
    let uri = std::ffi::CStr::from_ptr((*raw_req).uri.as_ptr())
        .to_str()
        .unwrap_or("");
    (uri.contains("poll=true"), uri.contains("levels=true"))
}

/// Send a regular HTTP JSON response from within a ws_handler callback.
/// Used when the handler detects a non-WebSocket GET request.
#[cfg(esp_idf_httpd_ws_support)]
unsafe fn ws_send_http_json(
    raw_req: *mut esp_idf_svc::sys::httpd_req_t,
    status: u16,
    json: &str,
) {
    use esp_idf_svc::sys::*;

    httpd_resp_set_type(raw_req, b"application/json\0".as_ptr());
    httpd_resp_set_hdr(
        raw_req,
        b"Access-Control-Allow-Origin\0".as_ptr(),
        b"*\0".as_ptr(),
    );

    if status == 503 {
        httpd_resp_set_status(raw_req, b"503 Service Unavailable\0".as_ptr());
    } else if status == 500 {
        httpd_resp_set_status(raw_req, b"500 Internal Server Error\0".as_ptr());
    }

    let ret = httpd_resp_send(raw_req, json.as_ptr(), json.len() as isize);
    if ret != ESP_OK {
        log::warn!("ws_send_http_json: httpd_resp_send failed ({ret})");
    }
}

/// Background thread that pushes device status over WebSocket at a configurable interval.
#[cfg(esp_idf_httpd_ws_support)]
fn ws_poll_loop(
    mut sender: esp_idf_svc::http::server::ws::EspHttpWsDetachedSender,
    state: SharedDeviceState,
    stop: Arc<std::sync::atomic::AtomicBool>,
    interval_ms: u64,
) {
    use esp_idf_svc::ws::FrameType;
    use std::sync::atomic::Ordering;

    log::info!("WebSocket poll thread started (interval={interval_ms}ms)");

    // Send initial status immediately so clients don't wait for first tick
    if let Some(dev) = get_device(&state) {
        if let Ok(status) = dev.get_status_summary() {
            let json = status_json(&status);
            if sender.send(FrameType::Text(false), json.as_bytes()).is_err() {
                return;
            }
        }
    }

    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(interval_ms));

        if stop.load(Ordering::Relaxed) {
            break;
        }

        let json = match get_device(&state) {
            Some(dev) => match dev.get_status_summary() {
                Ok(status) => status_json(&status),
                Err(_) => continue,
            },
            None => r#"{"error":"no device"}"#.to_string(),
        };

        if sender.send(FrameType::Text(false), json.as_bytes()).is_err() {
            break; // Connection lost
        }
    }

    log::info!("WebSocket poll thread stopped");
}

// ---------------------------------------------------------------------------
// ezBEQ WebSocket handler (/ws)
// ---------------------------------------------------------------------------

/// Register the ezBEQ WebSocket endpoint at /ws.
///
/// Protocol (from ezBEQ React UI):
///   Server → Client on connect: {"message":"Catalogue","data":{version,count}}
///   Server → Client on connect: {"message":"DeviceState","data":{...}}
///   Client → Server: "load catalogue" → no-op (browser JS shim handles this)
///   Client → Server: "subscribe levels <name>" → periodic Levels messages
#[cfg(esp_idf_httpd_ws_support)]
fn register_ezbeq_ws(
    server: &mut EspHttpServer<'static>,
    state: SharedDeviceState,
    beq_state: crate::ezbeq::SharedEzBeqState,
) -> Result<(), esp_idf_svc::sys::EspError> {
    use esp_idf_svc::ws::FrameType;
    use std::sync::atomic::{AtomicBool, Ordering};

    let active_ws: Arc<Mutex<Option<ActiveWsBeq>>> = Arc::new(Mutex::new(None));

    let st = state.clone();
    let beq = beq_state.clone();
    let ws_ref = active_ws.clone();

    server.ws_handler("/ws", None, move |ws| {
        if ws.is_new() {
            // Stop any previous session
            if let Ok(mut guard) = ws_ref.lock() {
                if let Some(prev) = guard.take() {
                    prev.stop.store(true, Ordering::Relaxed);
                }
            }

            // Send initial Catalogue meta
            let catalogue_meta = build_catalogue_meta();
            let _ = ws.send(FrameType::Text(false), catalogue_meta.as_bytes());

            // Send initial DeviceState
            if let Some(dev) = get_device(&st) {
                let beq_guard = beq.lock().unwrap_or_else(|e| e.into_inner());
                let state_json = crate::ezbeq::device_state_json(&dev, &beq_guard);
                let _ = ws.send(FrameType::Text(false), state_json.as_bytes());
            }

            // Store a detached sender for REST → WS broadcasts
            let fd = ws.session();
            match ws.create_detached_sender() {
                Ok(sender) => {
                    log::info!("ezBEQ WS: detached sender created (fd={fd})");
                    if let Ok(mut beq_guard) = beq.lock() {
                        beq_guard.ws_sender = Some(sender);
                    }
                    // Record fd so is_closed() can match and clean up even without subscribe
                    if let Ok(mut guard) = ws_ref.lock() {
                        *guard = Some(ActiveWsBeq {
                            stop: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                            fd,
                        });
                    }
                }
                Err(e) => log::warn!("ezBEQ WS: create_detached_sender failed: {e:?}"),
            }

            return Ok::<(), esp_idf_svc::sys::EspError>(());
        }

        if ws.is_closed() {
            let closing_fd = ws.session();
            if let Ok(mut guard) = ws_ref.lock() {
                if guard.as_ref().is_some_and(|a| a.fd == closing_fd) {
                    if let Some(active) = guard.take() {
                        active.stop.store(true, Ordering::Relaxed);
                    }
                    // Only clear ws_sender if this is the active session's fd
                    if let Ok(mut beq_guard) = beq.lock() {
                        beq_guard.ws_sender = None;
                    }
                }
            }
            return Ok(());
        }

        // Handle incoming messages
        let mut buf = [0u8; 256];
        match ws.recv(&mut buf) {
            Ok((ft, len)) => {
                let msg = std::str::from_utf8(&buf[..len])
                    .unwrap_or("")
                    .trim_end_matches('\0')
                    .trim();
                log::info!("ezBEQ WS: ft={ft:?} len={len} msg='{msg}'");

                if msg == "load catalogue" {
                    // Browser JS shim handles catalogue loading directly from beqcatalogue.
                    // Send empty response as safety fallback (shim intercepts before this fires).
                    log::info!("load catalogue: handled by browser shim");
                    let empty = r#"{"message":"CatalogueEntries","data":[]}"#;
                    let _ = ws.send(FrameType::Text(false), empty.as_bytes());
                } else if msg.starts_with("subscribe levels") {
                    if let Ok(mut beq_guard) = beq.lock() {
                        if let Some(sender) = beq_guard.ws_sender.take() {
                            let stop = Arc::new(AtomicBool::new(false));
                            let fd = ws.session();
                            if let Ok(mut guard) = ws_ref.lock() {
                                *guard = Some(ActiveWsBeq { stop: stop.clone(), fd });
                            }

                            let dev_state = st.clone();
                            let beq_clone = beq.clone();
                            let ws_ref_clone = ws_ref.clone();
                            if let Err(e) = std::thread::Builder::new()
                                .name("beq_lvl".into())
                                .stack_size(8192)
                                .spawn(move || {
                                    push_levels(sender, dev_state, stop, beq_clone, fd, ws_ref_clone);
                                })
                            {
                                log::warn!("failed to spawn levels thread: {e}");
                            }
                        }
                    }
                }

                if matches!(ft, FrameType::Close) {
                    let _ = ws.send(FrameType::Close, &[]);
                } else if matches!(ft, FrameType::Ping) {
                    let _ = ws.send(FrameType::Pong, &[]);
                }
            }
            Err(e) => {
                log::warn!("ezBEQ WS recv error: {e:?}");
            }
        }
        Ok(())
    })?;

    info!("ezBEQ WebSocket registered: /ws");
    Ok(())
}

/// Build the Catalogue meta message sent on WebSocket connect.
#[cfg(esp_idf_httpd_ws_support)]
fn build_catalogue_meta() -> String {
    // Read installed version from data partition
    let version = crate::storage::read_file("version.txt")
        .and_then(|b| String::from_utf8(b).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "0".to_string());

    // Count entries from the binary index header
    let count = match std::fs::File::open(crate::storage::full_path("catalogue.bin")) {
        Ok(mut f) => {
            let mut header = [0u8; 12];
            if std::io::Read::read_exact(&mut f, &mut header).is_ok()
                && &header[0..4] == b"BEQ\x02"
            {
                u32::from_le_bytes(header[4..8].try_into().unwrap())
            } else {
                0
            }
        }
        Err(_) => 0,
    };

    serde_json::to_string(&serde_json::json!({
        "message": "Catalogue",
        "data": {
            "version": version,
            "loaded": 0,
            "count": count,
        }
    }))
    .unwrap_or_default()
}

/// Active WebSocket session tracker for ezBEQ (module-level for push_levels access).
#[cfg(esp_idf_httpd_ws_support)]
struct ActiveWsBeq {
    stop: Arc<std::sync::atomic::AtomicBool>,
    fd: i32,
}

/// Push level meter data over WebSocket at ~4Hz.
/// Returns the sender to beq_state when done ONLY if the session fd still matches
/// (prevents overwriting a new client's sender).
#[cfg(esp_idf_httpd_ws_support)]
fn push_levels(
    mut sender: esp_idf_svc::http::server::ws::EspHttpWsDetachedSender,
    state: SharedDeviceState,
    stop: Arc<std::sync::atomic::AtomicBool>,
    beq_state: crate::ezbeq::SharedEzBeqState,
    session_fd: i32,
    active_ws: Arc<Mutex<Option<ActiveWsBeq>>>,
) {
    use esp_idf_svc::ws::FrameType;
    use std::sync::atomic::Ordering;

    log::info!("levels push thread started");

    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(250));

        if stop.load(Ordering::Relaxed) {
            break;
        }

        let json = match get_device(&state) {
            Some(dev) => match dev.get_status_summary() {
                Ok(status) => {
                    let mut levels = serde_json::Map::new();
                    for (i, &v) in status.input_levels.iter().enumerate() {
                        levels.insert(
                            format!("I{i}"),
                            serde_json::Value::from(v),
                        );
                    }
                    for (i, &v) in status.output_levels.iter().enumerate() {
                        levels.insert(
                            format!("O{i}"),
                            serde_json::Value::from(v),
                        );
                    }
                    serde_json::to_string(&serde_json::json!({
                        "message": "Levels",
                        "data": {
                            "name": "master",
                            "ts": 0,
                            "levels": levels,
                        }
                    }))
                    .unwrap_or_default()
                }
                Err(_) => continue,
            },
            None => continue,
        };

        if sender.send(FrameType::Text(false), json.as_bytes()).is_err() {
            break;
        }
    }

    log::info!("levels push thread stopped");

    // Only return sender if the session is still the same (no new client replaced us)
    let still_active = active_ws.lock().ok()
        .and_then(|g| g.as_ref().map(|a| a.fd == session_fd))
        .unwrap_or(false);
    if still_active {
        if let Ok(mut beq_guard) = beq_state.lock() {
            beq_guard.ws_sender = Some(sender);
        }
    } else {
        log::info!("levels: session replaced, not restoring sender");
    }
}

/// Serve a static file from LittleFS with correct Content-Type.
/// Streams in 4KB chunks to avoid loading large files into memory.
fn send_static_file<C: esp_idf_svc::http::server::Connection>(
    req: esp_idf_svc::http::server::Request<C>,
    path: &str,
) -> Result<(), C::Error> {
    // Open file BEFORE sending 200 — if it fails, return 404
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => {
            return send_json(req, 404, r#"{"error":"not found"}"#);
        }
    };
    let content_type = match path.rsplit('.').next() {
        Some("html") => "text/html",
        Some("js") => "application/javascript",
        Some("css") => "text/css",
        Some("json") => "application/json",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("ttf") => "font/ttf",
        Some("map") => "application/json",
        _ => "application/octet-stream",
    };
    let headers = [
        ("Content-Type", content_type),
        ("Access-Control-Allow-Origin", "*"),
        ("Cache-Control", "public, max-age=3600"),
    ];
    let mut resp = req.into_response(200, None, &headers)?;
    let mut buf = [0u8; 4096];
    loop {
        let n = std::io::Read::read(&mut file, &mut buf).unwrap_or(0);
        if n == 0 {
            break;
        }
        resp.write_all(&buf[..n])?;
    }
    Ok(())
}

/// Simple percent-decoding for form values.
fn urldecode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.bytes();
    while let Some(b) = chars.next() {
        match b {
            b'+' => result.push(' '),
            b'%' => {
                let hi = chars.next().and_then(|c| (c as char).to_digit(16));
                let lo = chars.next().and_then(|c| (c as char).to_digit(16));
                if let (Some(h), Some(l)) = (hi, lo) {
                    result.push((h * 16 + l) as u8 as char);
                }
            }
            _ => result.push(b as char),
        }
    }
    result
}
