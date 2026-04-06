//! HTTP server for ESP32-S3 using EspHttpServer (ESP-IDF httpd).
//!
//! Serves the same API as the desktop daemon. All handlers are blocking
//! (EspHttpServer runs in its own FreeRTOS task pool).

use crate::device::DeviceState;
use esp_idf_svc::http::server::{Configuration, EspHttpServer};
use esp_idf_svc::http::Method;
use esp_idf_svc::io::Write;
use log::{error, info};
use std::sync::Arc;

/// Start the HTTP server with all routes.
pub fn start(
    state: Option<Arc<DeviceState>>,
) -> Result<EspHttpServer<'static>, Box<dyn std::error::Error>> {
    let config = Configuration {
        http_port: 5380,
        max_uri_handlers: 20,
        stack_size: 8192,
        ..Default::default()
    };
    let mut server = EspHttpServer::new(&config)?;

    // GET /devices — list connected devices
    {
        let st = state.clone();
        server.fn_handler("/devices", Method::Get, move |req| {
            let body = match &st {
                Some(dev) => {
                    let info = dev.device_info();
                    serde_json::to_string(&serde_json::json!([{
                        "url": info.url,
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

    // GET /devices/0 — device status
    {
        let st = state.clone();
        server.fn_handler("/devices/0", Method::Get, move |req| {
            let dev = match &st {
                Some(d) => d,
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
            };
            match dev.get_status_summary() {
                Ok(status) => {
                    let body = serde_json::to_string(&serde_json::json!({
                        "master": {
                            "preset": status.master.preset,
                            "source": status.master.source,
                            "volume": status.master.volume,
                            "mute": status.master.mute,
                            "dirac": status.master.dirac,
                        },
                        "input_levels": status.input_levels,
                        "output_levels": status.output_levels,
                    })).unwrap_or_default();
                    send_json(req, 200, &body)
                }
                Err(e) => send_error(req, 500, &e.to_string()),
            }
        })?;
    }

    // POST /devices/0 — set master status
    {
        let st = state.clone();
        server.fn_handler("/devices/0", Method::Post, move |mut req| {
            let dev = match &st {
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
                if let Some(src) = minidsp_protocol::source::Source::from_str_pascal(source)
                    .or_else(|| minidsp_protocol::source::Source::from_str_lowercase(source))
                {
                    if let Some(idx) = dev.profile().sources.index_for_source(src) {
                        if let Err(e) = dev.set_source(idx) {
                            return send_error(req, 500, &format!("set_source: {e}"));
                        }
                    }
                }
            }
            if let Some(preset) = json.get("preset").and_then(|v| v.as_u64()) {
                if let Err(e) = dev.set_preset(preset as u8) {
                    return send_error(req, 500, &format!("set_preset: {e}"));
                }
            }
            if let Some(dirac) = json.get("dirac").and_then(|v| v.as_bool()) {
                if let Err(e) = dev.set_dirac_bypass(!dirac) {
                    return send_error(req, 500, &format!("set_dirac: {e}"));
                }
            }

            match dev.get_status_summary() {
                Ok(status) => {
                    let body = serde_json::to_string(&serde_json::json!({
                        "master": {
                            "preset": status.master.preset,
                            "source": status.master.source,
                            "volume": status.master.volume,
                            "mute": status.master.mute,
                            "dirac": status.master.dirac,
                        }
                    })).unwrap_or_default();
                    send_json(req, 200, &body)
                }
                Err(e) => send_error(req, 500, &e.to_string()),
            }
        })?;
    }

    // Compatibility routes: volume direction
    register_volume_routes(&mut server, state.clone())?;

    // Source and preset routes
    register_source_routes(&mut server, state.clone())?;
    register_preset_routes(&mut server, state.clone())?;

    // Display, DRE, tokens, preset copy
    register_device_feature_routes(&mut server, state.clone())?;

    // GET /llms.txt
    server.fn_handler("/llms.txt", Method::Get, |req| {
        send_json(req, 200, "# minidsp-open\nMiniDSP audio processor controller.\nHTTP API on port 5380.\n")
    })?;

    // WiFi provisioning page
    server.fn_handler("/", Method::Get, |req| {
        send_json(req, 200, crate::wifi::PROVISION_HTML)
    })?;

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
            return send_error(req, 400, "SSID is required");
        }

        // Save credentials to NVS
        let nvs = esp_idf_svc::nvs::EspDefaultNvsPartition::take();
        match nvs {
            Ok(nvs) => {
                if let Err(e) = crate::wifi::save_credentials(&nvs, &ssid, &password) {
                    return send_error(req, 500, &format!("save failed: {e}"));
                }
            }
            Err(e) => {
                return send_error(req, 500, &format!("NVS error: {e}"));
            }
        }

        // Return success page, then restart
        let html = "<html><body><h2>WiFi credentials saved!</h2>\
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

    info!("HTTP routes registered");
    Ok(server)
}

fn register_volume_routes(
    server: &mut EspHttpServer<'static>,
    state: Option<Arc<DeviceState>>,
) -> Result<(), esp_idf_svc::sys::EspError> {
    for dir in &["up", "down", "toggle_mute"] {
        let st = state.clone();
        let direction = dir.to_string();
        let path = format!("/devices/0/volume/{dir}");
        server.fn_handler(&path, Method::Post, move |req| {
            let dev = match &st {
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
            match result {
                Ok(()) => send_json(req, 200, r#"{"status":"ok"}"#),
                Err(e) => send_error(req, 500, &e.to_string()),
            }
        })?;
    }
    Ok(())
}

fn register_source_routes(
    server: &mut EspHttpServer<'static>,
    state: Option<Arc<DeviceState>>,
) -> Result<(), esp_idf_svc::sys::EspError> {
    let sources: Vec<(u8, String)> = state
        .as_ref()
        .map(|s| {
            s.profile()
                .sources
                .sources()
                .iter()
                .map(|(idx, src)| (*idx, src.to_string()))
                .collect()
        })
        .unwrap_or_default();

    for (idx, name) in sources {
        let st = state.clone();
        let path = format!("/devices/0/source/{name}");
        server.fn_handler(&path, Method::Post, move |req| {
            let dev = match &st {
                Some(d) => d,
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
            };
            match dev.set_source(idx) {
                Ok(()) => send_json(req, 200, r#"{"status":"ok"}"#),
                Err(e) => send_error(req, 500, &e.to_string()),
            }
        })?;
    }
    Ok(())
}

fn register_preset_routes(
    server: &mut EspHttpServer<'static>,
    state: Option<Arc<DeviceState>>,
) -> Result<(), esp_idf_svc::sys::EspError> {
    for preset in 0..=3u8 {
        let st = state.clone();
        let path = format!("/devices/0/preset/{preset}");
        server.fn_handler(&path, Method::Post, move |req| {
            let dev = match &st {
                Some(d) => d,
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
            };
            match dev.set_preset(preset) {
                Ok(()) => send_json(req, 200, r#"{"status":"ok"}"#),
                Err(e) => send_error(req, 500, &e.to_string()),
            }
        })?;
    }
    Ok(())
}

fn register_device_feature_routes(
    server: &mut EspHttpServer<'static>,
    state: Option<Arc<DeviceState>>,
) -> Result<(), esp_idf_svc::sys::EspError> {
    // GET /devices/0/display
    {
        let st = state.clone();
        server.fn_handler("/devices/0/display", Method::Get, move |req| {
            let dev = match &st {
                Some(d) if d.profile().has_display => d,
                _ => return send_json(req, 404, r#"{"error":"not supported"}"#),
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
            let dev = match &st {
                Some(d) if d.profile().has_display => d,
                _ => return send_json(req, 404, r#"{"error":"not supported"}"#),
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
            let dev = match &st {
                Some(d) if d.profile().has_dre => d,
                _ => return send_json(req, 404, r#"{"error":"not supported"}"#),
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
            let dev = match &st {
                Some(d) if d.profile().has_dre => d,
                _ => return send_json(req, 404, r#"{"error":"not supported"}"#),
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
            let dev = match &st {
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
            let dev = match &st {
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

// -- Response helpers using std::io::Error (always Send+Sync) --

fn send_json<C: esp_idf_svc::http::server::Connection>(
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

fn send_error<C: esp_idf_svc::http::server::Connection>(
    req: esp_idf_svc::http::server::Request<C>,
    status: u16,
    message: &str,
) -> Result<(), C::Error> {
    // Escape message for JSON
    let body = format!(r#"{{"error":"{}"}}"#, message.replace('"', "\\\""));
    send_json(req, status, &body)
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
