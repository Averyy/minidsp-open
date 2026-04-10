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
use crate::usb_audio;
use esp_idf_svc::http::server::{Configuration, EspHttpServer};
use esp_idf_svc::http::Method;
use esp_idf_svc::io::Write;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use log::{debug, info, warn};
use minidsp_protocol::playback::{
    BitDepth, PlayRequest, PlayResponse, PlaybackPlan, SyncRequest, SyncResponse,
};
use minidsp_protocol::source::Source;
use minidsp_protocol::status::StatusSummary;
use serde_json::json;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

/// Shared device state — None when USB disconnected, Some when connected.
pub type SharedDeviceState = Arc<Mutex<Option<Arc<DeviceState>>>>;

/// Start the HTTP server with all routes.
/// `port` should be 80 for SoftAP provisioning (captive portal), 5380 for normal STA mode.
/// `ap_ip` is the SoftAP IP for captive portal redirects (None in STA mode).
/// `beq_state` is the shared ezBEQ slot state (None in SoftAP mode).
pub fn start(
    state: SharedDeviceState,
    audio_state: usb_audio::SharedAudioState,
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
        // `/devices/0/play` does request parsing, source switching, and
        // playback-thread orchestration before handing back the response.
        // The default 16 KiB httpd task stack is not enough on real hardware.
        stack_size: 24 * 1024,
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
                    }]))
                    .unwrap_or_else(|_| "[]".into())
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
            let _op = dev.begin_operation();
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
            let _op = dev.begin_operation();

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

    {
        server.fn_handler("/sync", Method::Post, move |mut req| {
            let body = match read_request_body(&mut req, 512) {
                Ok(body) => body,
                Err(message) => return send_error(req, 413, message),
            };
            let sync_req: SyncRequest = match serde_json::from_slice(&body) {
                Ok(value) => value,
                Err(_) => return send_error(req, 400, "invalid JSON"),
            };
            let response = SyncResponse {
                client_time_us: sync_req.client_time_us,
                server_time_us: usb_audio::server_time_us(),
            };
            match serde_json::to_string(&response) {
                Ok(json) => send_json(req, 200, &json),
                Err(_) => send_error(req, 500, "failed to serialize response"),
            }
        })?;
    }

    {
        let st = state.clone();
        let audio = audio_state.clone();
        server.fn_handler("/devices/0/play", Method::Post, move |mut req| {
            let dev = match get_device(&st) {
                Some(d) => d,
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
            };
            if !dev.profile().has_usb_audio {
                return send_error(req, 400, "device does not support USB audio playback");
            }
            let audio_device = match audio.current_device() {
                Some(device) => device,
                None => return send_error(req, 503, "USB audio interface not connected"),
            };
            if !audio.try_begin_playback() {
                return send_error(req, 409, "playback already in progress");
            }

            // Check for diagnostic test modes via query parameter
            let uri = req.uri().to_string();
            let test_mode = uri
                .split_once('?')
                .map(|(_, q)| q)
                .unwrap_or("")
                .split('&')
                .find_map(|p| {
                    let (k, v) = p.split_once('=').unwrap_or((p, ""));
                    if k == "test" { Some(v.to_string()) } else { None }
                });

            if test_mode.as_deref() == Some("sine") {
                // Inline sine test: bypass PlaybackGenerator and PSRAM entirely.
                // Generates sin(2πft) into a stack buffer in a tight write loop,
                // identical to the standalone driver test harness.
                let usb_index = match dev.profile().sources.index_for_source(Source::Usb) {
                    Some(index) => index,
                    None => {
                        audio.finish_playback();
                        return send_error(req, 500, "USB source missing from device profile");
                    }
                };
                let (source_changed, previous_source, restore_source) = {
                    let _op = dev.begin_operation();
                    let current_source = match dev.current_source() {
                        Ok(Some(source)) => source,
                        _ => {
                            audio.finish_playback();
                            return send_error(req, 500, "current source unavailable");
                        }
                    };
                    if current_source == Source::Usb {
                        (false, None, None)
                    } else {
                        if let Err(err) = dev.set_source(usb_index) {
                            audio.finish_playback();
                            return send_error(req, 500, &format!("set_source: {err}"));
                        }
                        (true, Some(current_source.to_string()), Some(current_source))
                    }
                };

                let (tx, rx) = mpsc::sync_channel(1);
                let dev_for_thread = dev.clone();
                let audio_for_thread = audio.clone();
                if let Err(err) = std::thread::Builder::new()
                    .name("sine_test".into())
                    .stack_size(16 * 1024)
                    .spawn(move || {
                        run_inline_sine_test(
                            dev_for_thread,
                            audio_for_thread,
                            audio_device,
                            source_changed,
                            previous_source,
                            restore_source,
                            tx,
                        );
                    })
                {
                    if let Some(source) = restore_source {
                        if let Some(index) = dev.profile().sources.index_for_source(source) {
                            let _op = dev.begin_operation();
                            let _ = dev.set_source(index);
                        }
                    }
                    audio.finish_playback();
                    return send_error(req, 500, &format!("spawn sine test: {err}"));
                }

                return match rx.recv() {
                    Ok(Ok(response)) => match serde_json::to_string(&response) {
                        Ok(json) => send_json(req, 200, &json),
                        Err(_) => send_error(req, 500, "failed to serialize response"),
                    },
                    Ok(Err(message)) => {
                        audio.finish_playback();
                        send_error(req, 500, &message)
                    }
                    Err(_) => {
                        audio.finish_playback();
                        send_error(req, 500, "sine test thread ended unexpectedly")
                    }
                };
            }

            let body = match read_request_body(&mut req, 4096) {
                Ok(body) => body,
                Err(message) => {
                    audio.finish_playback();
                    return send_error(req, 413, message);
                }
            };
            let play_request = if body.is_empty() {
                PlayRequest::default()
            } else {
                match serde_json::from_slice::<PlayRequest>(&body) {
                    Ok(value) => value,
                    Err(_) => {
                        audio.finish_playback();
                        return send_error(req, 400, "invalid JSON");
                    }
                }
            };

            let plan = match PlaybackPlan::build(play_request) {
                Ok(plan) => plan,
                Err(err) => {
                    audio.finish_playback();
                    return send_error(req, 400, &err.to_string());
                }
            };

            let usb_index = match dev.profile().sources.index_for_source(Source::Usb) {
                Some(index) => index,
                None => {
                    audio.finish_playback();
                    return send_error(req, 500, "USB source missing from device profile");
                }
            };

            let (source_changed, previous_source, restore_source) = {
                let _op = dev.begin_operation();
                let current_source = match dev.current_source() {
                    Ok(Some(source)) => source,
                    Ok(None) => {
                        audio.finish_playback();
                        return send_error(req, 500, "current source unavailable");
                    }
                    Err(err) => {
                        audio.finish_playback();
                        return send_error(req, 500, &format!("read source: {err}"));
                    }
                };

                if current_source == Source::Usb {
                    (false, None, None)
                } else {
                    if let Err(err) = dev.set_source(usb_index) {
                        audio.finish_playback();
                        return send_error(req, 500, &format!("set_source: {err}"));
                    }
                    (
                        true,
                        Some(current_source.to_string()),
                        Some(current_source),
                    )
                }
            };

            let (tx, rx) = mpsc::sync_channel(1);
            let dev_for_thread = dev.clone();
            let audio_for_thread = audio.clone();
            if let Err(err) = std::thread::Builder::new()
                .name("playback".into())
                .stack_size(24 * 1024)
                .spawn(move || {
                    run_playback_thread(
                        dev_for_thread,
                        audio_for_thread,
                        audio_device,
                        plan,
                        source_changed,
                        previous_source,
                        restore_source,
                        tx,
                    );
                })
            {
                if let Some(source) = restore_source {
                    if let Some(index) = dev.profile().sources.index_for_source(source) {
                        let _op = dev.begin_operation();
                        if let Err(restore_err) = dev.set_source(index) {
                            warn!(
                                "failed to restore source after playback spawn failure: {restore_err}"
                            );
                        }
                    }
                }
                audio.finish_playback();
                return send_error(req, 500, &format!("spawn playback thread: {err}"));
            }

            match rx.recv() {
                Ok(Ok(response)) => match serde_json::to_string(&response) {
                    Ok(json) => send_json(req, 200, &json),
                    Err(_) => send_error(req, 500, "failed to serialize response"),
                },
                Ok(Err(message)) => {
                    audio.finish_playback();
                    send_error(req, 500, &message)
                }
                Err(_) => {
                    audio.finish_playback();
                    send_error(req, 500, "playback thread ended unexpectedly")
                }
            }
        })?;
    }

    // GET /devices/0/config — full config read-back
    {
        let st = state.clone();
        server.fn_handler("/devices/0/config", Method::Get, move |req| {
            let dev = match get_device(&st) {
                Some(d) => d,
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
            };
            let _op = dev.begin_operation();
            match dev.read_config() {
                Ok(config) => {
                    let body = serde_json::to_string(&config).unwrap_or_else(|_| "{}".into());
                    send_json(req, 200, &body)
                }
                Err(e) if e.to_string().contains("unsupported") => {
                    send_error(req, 501, "config read unsupported")
                }
                Err(e) => send_error(req, 500, &e.to_string()),
            }
        })?;
    }

    // POST /devices/0/config — full config write (PEQ, gain, mute, delay, polarity, master)
    {
        let st = state.clone();
        server.fn_handler("/devices/0/config", Method::Post, move |mut req| {
            let dev = match get_device(&st) {
                Some(d) => d,
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
            };
            let body = match read_request_body(&mut req, 8192) {
                Ok(body) => body,
                Err("request body too large") => {
                    return send_error(req, 413, "request body too large")
                }
                Err(_) => return send_error(req, 400, "failed to read request body"),
            };

            info!("POST /devices/0/config start: {} bytes", body.len());
            let json: serde_json::Value = match serde_json::from_slice(&body) {
                Ok(v) => v,
                Err(_) => return send_error(req, 400, "invalid JSON"),
            };
            let _op = dev.begin_operation();

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
                                    None => {
                                        return send_error(req, 400, "peq missing 'index' field")
                                    }
                                };
                                if let Some(band) = input_ch.peq.get(band_idx) {
                                    if let Some(coeff) = peq.get("coeff") {
                                        let c = [
                                            coeff.get("b0").and_then(|v| v.as_f64()).unwrap_or(0.0)
                                                as f32,
                                            coeff.get("b1").and_then(|v| v.as_f64()).unwrap_or(0.0)
                                                as f32,
                                            coeff.get("b2").and_then(|v| v.as_f64()).unwrap_or(0.0)
                                                as f32,
                                            coeff.get("a1").and_then(|v| v.as_f64()).unwrap_or(0.0)
                                                as f32,
                                            coeff.get("a2").and_then(|v| v.as_f64()).unwrap_or(0.0)
                                                as f32,
                                        ];
                                        if c.iter().any(|v| !v.is_finite()) {
                                            return send_error(
                                                req,
                                                400,
                                                "biquad coefficients must be finite",
                                            );
                                        }
                                        if let Err(e) = dev.write_biquad(band.addr, &c) {
                                            return send_error(
                                                req,
                                                500,
                                                &format!("write_biquad: {e}"),
                                            );
                                        }
                                    }
                                    if let Some(bypass) =
                                        peq.get("bypass").and_then(|v| v.as_bool())
                                    {
                                        if let Err(e) = dev.bypass_filter(band.addr, bypass) {
                                            return send_error(
                                                req,
                                                500,
                                                &format!("bypass_filter: {e}"),
                                            );
                                        }
                                    }
                                }
                            }
                        }
                        // Per-channel gain
                        if let Some(gain) = input.get("gain").and_then(|v| v.as_f64()) {
                            if !gain.is_finite() || gain.abs() > 120.0 {
                                return send_error(
                                    req,
                                    400,
                                    "gain out of range (must be finite, |gain| <= 120 dB)",
                                );
                            }
                            if let Some(gain_addr) = input_ch.gain {
                                if let Err(e) = dev.set_channel_gain(gain_addr, gain as f32) {
                                    return send_error(req, 500, &format!("set_gain: {e}"));
                                }
                            }
                        }
                        // Per-channel mute
                        if let Some(mute) = input.get("mute").and_then(|v| v.as_bool()) {
                            if let Some(mute_addr) = input_ch.mute {
                                if let Err(e) = dev.set_channel_mute(mute_addr, mute) {
                                    return send_error(req, 500, &format!("set_mute: {e}"));
                                }
                            }
                        }
                        if let Some(routes) = input.get("routing").and_then(|v| v.as_array()) {
                            for route in routes {
                                let output = match route.get("output").and_then(|v| v.as_u64()) {
                                    Some(v) => v as u8,
                                    None => {
                                        return send_error(
                                            req,
                                            400,
                                            "routing missing 'output' field",
                                        )
                                    }
                                };
                                let enabled = match route.get("enabled").and_then(|v| v.as_bool()) {
                                    Some(v) => v,
                                    None => {
                                        return send_error(
                                            req,
                                            400,
                                            "routing missing 'enabled' field",
                                        )
                                    }
                                };
                                let gain_db = match route.get("gain_db").and_then(|v| v.as_f64()) {
                                    Some(v) if v.is_finite() => v as f32,
                                    Some(_) => {
                                        return send_error(req, 400, "routing gain must be finite")
                                    }
                                    None => {
                                        return send_error(
                                            req,
                                            400,
                                            "routing missing 'gain_db' field",
                                        )
                                    }
                                };
                                if let Err(e) = dev.set_routing(idx as u8, output, enabled, gain_db)
                                {
                                    return send_error(req, 500, &format!("set_routing: {e}"));
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
                                    None => {
                                        return send_error(req, 400, "peq missing 'index' field")
                                    }
                                };
                                if let Some(band) = output_ch.peq.get(band_idx) {
                                    info!(
                                        "config write output[{idx}] peq[{band_idx}] addr=0x{:04x}",
                                        band.addr
                                    );
                                    if let Some(coeff) = peq.get("coeff") {
                                        let c = [
                                            coeff.get("b0").and_then(|v| v.as_f64()).unwrap_or(0.0)
                                                as f32,
                                            coeff.get("b1").and_then(|v| v.as_f64()).unwrap_or(0.0)
                                                as f32,
                                            coeff.get("b2").and_then(|v| v.as_f64()).unwrap_or(0.0)
                                                as f32,
                                            coeff.get("a1").and_then(|v| v.as_f64()).unwrap_or(0.0)
                                                as f32,
                                            coeff.get("a2").and_then(|v| v.as_f64()).unwrap_or(0.0)
                                                as f32,
                                        ];
                                        if c.iter().any(|v| !v.is_finite()) {
                                            return send_error(
                                                req,
                                                400,
                                                "biquad coefficients must be finite",
                                            );
                                        }
                                        if let Err(e) = dev.write_biquad(band.addr, &c) {
                                            return send_error(
                                                req,
                                                500,
                                                &format!("write_biquad: {e}"),
                                            );
                                        }
                                    }
                                    if let Some(bypass) =
                                        peq.get("bypass").and_then(|v| v.as_bool())
                                    {
                                        if let Err(e) = dev.bypass_filter(band.addr, bypass) {
                                            return send_error(
                                                req,
                                                500,
                                                &format!("bypass_filter: {e}"),
                                            );
                                        }
                                    }
                                }
                            }
                        }
                        // Per-channel gain
                        if let Some(gain) = output.get("gain").and_then(|v| v.as_f64()) {
                            debug!("config write output[{idx}] gain -> {gain}");
                            if !gain.is_finite() || gain.abs() > 120.0 {
                                return send_error(
                                    req,
                                    400,
                                    "gain out of range (must be finite, |gain| <= 120 dB)",
                                );
                            }
                            if let Err(e) = dev.set_channel_gain(output_ch.gain, gain as f32) {
                                return send_error(req, 500, &format!("set_gain: {e}"));
                            }
                        }
                        // Per-channel mute
                        if let Some(mute) = output.get("mute").and_then(|v| v.as_bool()) {
                            debug!("config write output[{idx}] mute -> {mute}");
                            if let Err(e) = dev.set_channel_mute(output_ch.mute, mute) {
                                return send_error(req, 500, &format!("set_mute: {e}"));
                            }
                        }
                        // Delay (Duration format: {secs, nanos} or float seconds)
                        if let Some(delay) = output.get("delay") {
                            let secs = if let Some(s) = delay.get("secs").and_then(|v| v.as_f64()) {
                                let nanos =
                                    delay.get("nanos").and_then(|v| v.as_f64()).unwrap_or(0.0);
                                s + nanos / 1_000_000_000.0
                            } else {
                                delay.as_f64().unwrap_or(0.0)
                            };
                            debug!("config write output[{idx}] delay_secs -> {secs}");
                            if let Some(delay_addr) = output_ch.delay {
                                let sample_rate = dev.profile().sample_rate as f32;
                                let samples =
                                    (secs as f32 * sample_rate).round().clamp(0.0, 48000.0);
                                if let Err(e) =
                                    dev.set_channel_delay_samples(delay_addr, samples as u32)
                                {
                                    return send_error(req, 500, &format!("set_delay: {e}"));
                                }
                            }
                        }
                        // Polarity inversion (dialect-aware encoding)
                        if let Some(invert) = output.get("invert").and_then(|v| v.as_bool()) {
                            debug!("config write output[{idx}] invert -> {invert}");
                            if let Some(polarity_addr) = output_ch.polarity {
                                if let Err(e) = dev.set_invert(polarity_addr, invert) {
                                    return send_error(req, 500, &format!("set_polarity: {e}"));
                                }
                            }
                        }
                    }
                }
            }

            info!("POST /devices/0/config completed");
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
        crate::ezbeq::register_routes(&mut server, state.clone(), beq.clone(), nvs.clone())?;
        #[cfg(esp_idf_httpd_ws_support)]
        register_ezbeq_ws(&mut server, state.clone(), beq.clone())?;
    }

    // GET /llms.txt
    server.fn_handler("/llms.txt", Method::Get, |req| {
        send_text(
            req,
            "# minidsp-open\nMiniDSP audio processor controller.\nHTTP API on port 5380.\n",
        )
    })?;

    // CORS preflight handler — responds to OPTIONS on any path
    server.fn_handler("/*", Method::Options, |req| send_cors_preflight(req))?;

    // Root page — provisioning in SoftAP mode, serve UI or redirect in STA mode
    if ap_ip.is_some() {
        server.fn_handler("/", Method::Get, |req| {
            info!("serving provisioning page");
            send_html(req, crate::wifi::PROVISION_HTML)
        })?;
    } else {
        let nvs_root = nvs.clone();
        server.fn_handler("/", Method::Get, move |req| {
            // Serve ezBEQ UI index.html if available, otherwise redirect to API
            let index = crate::storage::full_path("ui/index.html");
            if std::path::Path::new(&index).exists() {
                send_static_file(req, &index)
            } else {
                let started = crate::updater::trigger_background_refresh(
                    nvs_root.clone(),
                    "ui/index.html missing",
                );
                let body = if started {
                    r#"{"status":"initializing","error":"ui not ready; refresh started"}"#
                } else {
                    r#"{"status":"initializing","error":"ui not ready"}"#
                };
                send_json(req, 503, body)
            }
        })?;
    }

    // POST /api/wifi/connect — WiFi provisioning form handler
    let nvs_wifi = nvs.clone();
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
        if let Err(e) = crate::wifi::save_credentials(&nvs_wifi, &ssid, &password) {
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
        let nvs_static = nvs.clone();
        server.fn_handler("/*", Method::Get, move |req| {
            let uri = req.uri();
            // Strip query string for file path lookup
            let path = uri.split('?').next().unwrap_or(uri);

            // Never serve HTML for API paths — return 404 JSON instead
            if path.starts_with("/api/")
                || path.starts_with("/devices")
                || path.starts_with("/debug")
            {
                return send_json(req, 404, r#"{"error":"not found"}"#);
            }

            // Path traversal protection: reject ".." in any encoding
            if path.contains("..")
                || path.contains("%2e")
                || path.contains("%2E")
                || path.contains('\0')
                || path.contains("%00")
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
                    let started = crate::updater::trigger_background_refresh(
                        nvs_static.clone(),
                        "ui asset missing",
                    );
                    let body = if started {
                        r#"{"status":"initializing","error":"ui assets missing; refresh started"}"#
                    } else {
                        r#"{"status":"initializing","error":"ui assets missing"}"#
                    };
                    send_json(req, 503, body)
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
    use esp_idf_svc::sys::{
        esp_netif_get_handle_from_ifkey, esp_netif_get_ip_info, esp_netif_ip_info_t,
    };

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
        Some(format!(
            "{}.{}.{}.{}",
            octets[0], octets[1], octets[2], octets[3]
        ))
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

    // GET /debug/dsp?addr=0x20&count=4&format=floats|words
    {
        let st = state.clone();
        server.fn_handler("/debug/dsp", Method::Get, move |req| {
            let dev = match get_device(&st) {
                Some(d) => d,
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
            };

            let uri = req.uri().to_string();
            let query = uri.split_once('?').map(|(_, q)| q).unwrap_or("");
            let mut addr: Option<u16> = None;
            let mut count: u8 = 1;
            let mut format = "floats";

            for part in query.split('&') {
                let Some((key, value)) = part.split_once('=') else {
                    continue;
                };
                match key {
                    "addr" => {
                        addr = if let Some(hex) = value
                            .strip_prefix("0x")
                            .or_else(|| value.strip_prefix("0X"))
                        {
                            u16::from_str_radix(hex, 16).ok()
                        } else {
                            value.parse::<u16>().ok()
                        };
                    }
                    "count" => {
                        if let Ok(parsed) = value.parse::<u8>() {
                            count = parsed.clamp(1, 14);
                        }
                    }
                    "format" if value == "words" || value == "floats" => {
                        format = value;
                    }
                    _ => {}
                }
            }

            let Some(addr) = addr else {
                return send_error(req, 400, "missing addr query parameter");
            };
            let _op = dev.begin_operation();

            let body = match format {
                "words" => match dev.read_dsp_words(addr, count) {
                    Ok(values) => serde_json::to_string(&json!({
                        "addr": addr,
                        "count": count,
                        "format": "words",
                        "values": values,
                    }))
                    .unwrap_or_default(),
                    Err(e) => return send_error(req, 500, &e.to_string()),
                },
                _ => match dev.read_dsp_floats(addr, count) {
                    Ok(values) => serde_json::to_string(&json!({
                        "addr": addr,
                        "count": count,
                        "format": "floats",
                        "values": values,
                    }))
                    .unwrap_or_default(),
                    Err(e) => return send_error(req, 500, &e.to_string()),
                },
            };

            send_json(req, 200, &body)
        })?;
    }

    // POST /debug/level — change runtime log level
    server.fn_handler("/debug/level", Method::Post, |mut req| {
        let mut buf = [0u8; 128];
        let len = req.read(&mut buf).unwrap_or(0);

        let level_str = if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&buf[..len]) {
            json.get("level")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        } else {
            // Try plain text body
            std::str::from_utf8(&buf[..len])
                .unwrap_or("")
                .trim()
                .to_string()
        };

        match diagnostics::set_log_level(&level_str) {
            Some(name) => {
                let body = format!(r#"{{"level":"{name}"}}"#);
                send_json(req, 200, &body)
            }
            None => send_error(
                req,
                400,
                "invalid level (use: error, warn, info, debug, trace)",
            ),
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
                "isUpdating": crate::updater::is_updating(),
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
            match crate::updater::try_check_and_update(&nvs) {
                crate::updater::UpdateAttempt::Busy => {
                    let body = serde_json::json!({
                        "status": "busy",
                        "baseUrl": crate::updater::base_url(&nvs),
                    });
                    send_json(req, 409, &serde_json::to_string(&body).unwrap_or_default())
                }
                crate::updater::UpdateAttempt::Completed(result) => {
                    let body = match result {
                        crate::updater::UpdateResult::Current => serde_json::json!({
                            "status": "current",
                            "baseUrl": crate::updater::base_url(&nvs),
                        }),
                        crate::updater::UpdateResult::Updated { version, files } => {
                            serde_json::json!({
                                "status": "updated",
                                "baseUrl": crate::updater::base_url(&nvs),
                                "version": version,
                                "files": files,
                            })
                        }
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
                }
            }
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
            let _op = dev.begin_operation();
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
            let _op = dev.begin_operation();
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
            let _op = dev.begin_operation();
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
            let _op = dev.begin_operation();
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
            let _op = dev.begin_operation();
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
            let _op = dev.begin_operation();
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
            let _op = dev.begin_operation();
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
            let _op = dev.begin_operation();
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
            let _op = dev.begin_operation();
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
    state.lock().unwrap_or_else(|e| e.into_inner()).clone()
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
        (
            "Access-Control-Allow-Methods",
            "GET, POST, PUT, DELETE, PATCH, OPTIONS",
        ),
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
    let headers = [("Access-Control-Allow-Origin", "*")];
    let mut resp = req.into_response(200, None, &headers)?;
    resp.write_all(b"")?;
    Ok(())
}

fn read_request_body<C: esp_idf_svc::http::server::Connection>(
    req: &mut esp_idf_svc::http::server::Request<C>,
    max_len: usize,
) -> Result<Vec<u8>, &'static str> {
    let declared_len = req
        .header("Content-Length")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    if declared_len > max_len {
        return Err("request body too large");
    }

    let mut body = Vec::with_capacity(declared_len.min(max_len));
    let mut buf = [0u8; 512];

    loop {
        let remaining = if declared_len == 0 {
            max_len.saturating_sub(body.len())
        } else {
            declared_len.saturating_sub(body.len())
        };
        if remaining == 0 {
            break;
        }

        let read_len = remaining.min(buf.len());
        let read = req
            .read(&mut buf[..read_len])
            .map_err(|_| "failed to read request body")?;
        if read == 0 {
            break;
        }

        body.extend_from_slice(&buf[..read]);
        if body.len() > max_len {
            return Err("request body too large");
        }
    }

    Ok(body)
}

pub fn send_error<C: esp_idf_svc::http::server::Connection>(
    req: esp_idf_svc::http::server::Request<C>,
    status: u16,
    message: &str,
) -> Result<(), C::Error> {
    let body = serde_json::to_string(&json!({ "error": message }))
        .unwrap_or_else(|_| r#"{"error":"internal error"}"#.to_string());
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
    map.insert(
        "source".into(),
        serde_json::Value::String(status.master.source.clone()),
    );
    map.insert(
        "volume".into(),
        serde_json::to_value(status.master.volume).unwrap_or_default(),
    );
    map.insert("mute".into(), serde_json::json!(status.master.mute));
    map.insert(
        "dirac".into(),
        match status.master.dirac {
            Some(d) => serde_json::json!(d),
            None => serde_json::Value::Null,
        },
    );
    serde_json::to_string(&map).unwrap_or_default()
}

fn run_playback_thread(
    dev: Arc<DeviceState>,
    audio_state: usb_audio::SharedAudioState,
    audio_device: Arc<usb_audio::Uac2Device>,
    plan: PlaybackPlan,
    source_changed: bool,
    previous_source: Option<String>,
    restore_source: Option<Source>,
    started_tx: mpsc::SyncSender<Result<PlayResponse, String>>,
) {
    // Disable WiFi power save during playback — beacon wakeups cause
    // ~1-2 ms CPU stalls that can disrupt USB isochronous timing.
    unsafe {
        esp_idf_sys::esp_wifi_set_ps(esp_idf_sys::wifi_ps_type_t_WIFI_PS_NONE);
    }

    // Pre-generate the entire sweep into memory before starting the USB
    // audio stream. ESP32 float math (sin/cos/ln) is too slow to keep up
    // with real-time 48 kHz playback, so we generate everything upfront
    // into PSRAM (~1.8 MB for a 9.5 s sweep) and then just blast bytes
    // to the ring buffer during playback.
    let total_bytes = plan.offsets().total_samples * plan.layout().frame_bytes();
    info!(
        "pre-generating {} samples ({} bytes) at {} Hz {}-bit",
        plan.offsets().total_samples,
        total_bytes,
        plan.request.sample_rate,
        plan.request.bit_depth.bits()
    );
    let gen_started_us = usb_audio::server_time_us();
    let mut pcm_data = vec![0u8; total_bytes];
    let mut generator = plan.generator();
    let mut offset = 0;
    let mut yield_counter = 0u32;
    while !generator.is_finished() && offset < pcm_data.len() {
        let written = generator.fill_pcm_bytes(&mut pcm_data[offset..]);
        if written == 0 {
            break;
        }
        offset += written;
        yield_counter += 1;
        if yield_counter % 100 == 0 {
            std::thread::yield_now();
        }
    }
    let pcm_data = &pcm_data[..offset];
    let gen_elapsed_ms = (usb_audio::server_time_us() - gen_started_us) / 1000;
    info!(
        "pre-generation complete: {} bytes in {} ms",
        pcm_data.len(),
        gen_elapsed_ms
    );

    // Start the USB audio stream and write the pre-generated data
    let start_result = start_playback_pregenerated(&audio_device, &plan, pcm_data).map(
        |mut start| {
            start.response.source_changed = source_changed;
            start.response.previous_source = previous_source.clone();
            start
        },
    );

    match start_result {
        Ok(start) => {
            let playback_started_us = usb_audio::server_time_us();
            info!(
                "playback streaming started: {} bytes pre-generated",
                pcm_data.len()
            );
            let _ = started_tx.send(Ok(start.response.clone()));
            let mut write_time_us = start.first_write_time_us;
            let mut total_bytes_written = start.first_write_bytes;
            let mut last_write_bytes = start.first_write_bytes;
            let mut last_write_return_us = start.first_write_return_us;
            let mut write_latencies_us = vec![start.first_write_time_us];

            // Stream remaining pre-generated data in chunks
            let chunk_bytes = start.chunk_bytes;
            // Copy each chunk from PSRAM to a stack buffer (internal SRAM)
            // before writing to the ring buffer. xRingbufferSend holds a
            // critical section during memcpy — PSRAM SPI latency inside
            // that section can block the USB host task.
            let mut sram_buf = [0u8; 1920];
            let mut pos = start.first_write_bytes;
            while pos < pcm_data.len() {
                let end = (pos + chunk_bytes).min(pcm_data.len());
                let len = end - pos;
                sram_buf[..len].copy_from_slice(&pcm_data[pos..end]);
                let write_started_us = usb_audio::server_time_us();
                if let Err(err) = audio_device.write(&sram_buf[..len], 250) {
                    warn!("playback stream failed: {err}");
                    break;
                }
                let write_elapsed_us = usb_audio::server_time_us() - write_started_us;
                write_time_us += write_elapsed_us;
                write_latencies_us.push(write_elapsed_us);
                let written = end - pos;
                total_bytes_written += written;
                last_write_bytes = written;
                last_write_return_us = usb_audio::server_time_us();
                pos = end;
                if write_latencies_us.len() % 100 == 0 {
                    info!(
                        "playback progress: {} / {} bytes, write={} ms",
                        total_bytes_written,
                        pcm_data.len(),
                        write_time_us / 1000
                    );
                }
            }

            let fb_raw = audio_device.feedback_raw();
            let fb_hz = fb_raw >> 16;
            let fb_frac = ((fb_raw & 0xFFFF) as u64 * 10000) / 65536;
            info!("USB feedback at end of playback: raw=0x{fb_raw:08x} ({fb_hz}.{fb_frac:04} Hz)");
            let stop_started_us = usb_audio::server_time_us();
            if let Err(err) = audio_device.stop_stream() {
                warn!("UAC2 stop failed after playback: {err}");
            }
            let stop_finished_us = usb_audio::server_time_us();
            let (
                write_min_us,
                write_max_us,
                write_avg_us,
                write_p50_us,
                write_p95_us,
                write_p99_us,
            ) = summarize_write_latencies(&write_latencies_us);
            info!(
                "playback completed in {} ms (gen={} ms, write={} ms, expected_bytes={}, actual_bytes={}, writes={}, last_write_bytes={}, tx_need_data={}, last_write_after_start={} ms, stop_time={} ms, write_min={} us, write_max={} us, write_avg={} us, write_p50={} us, write_p95={} us, write_p99={} us)",
                (usb_audio::server_time_us() - playback_started_us) / 1000,
                gen_elapsed_ms,
                write_time_us / 1000,
                total_bytes,
                total_bytes_written,
                write_latencies_us.len(),
                last_write_bytes,
                usb_audio::tx_need_data_count(),
                (last_write_return_us - playback_started_us) / 1000,
                (stop_finished_us - stop_started_us) / 1000,
                write_min_us,
                write_max_us,
                write_avg_us,
                write_p50_us,
                write_p95_us,
                write_p99_us,
            );
        }
        Err(err) => {
            let _ = started_tx.send(Err(err));
        }
    }

    if let Some(source) = restore_source {
        if let Some(index) = dev.profile().sources.index_for_source(source) {
            let _op = dev.begin_operation();
            if let Err(err) = dev.set_source(index) {
                warn!("failed to restore source after playback: {err}");
            } else {
                info!("restored source after playback to {}", source);
            }
        }
    }

    audio_state.finish_playback();

    // Restore WiFi power save after playback
    unsafe {
        esp_idf_sys::esp_wifi_set_ps(esp_idf_sys::wifi_ps_type_t_WIFI_PS_MIN_MODEM);
    }
}

/// Inline sine test: generates sin(2πft) on the stack and writes to USB
/// in a tight loop, identical to the standalone driver test harness.
/// No PSRAM, no PlaybackGenerator, no pre-generation — isolates whether
/// blips come from the data pipeline or the system environment.
fn run_inline_sine_test(
    dev: Arc<DeviceState>,
    audio_state: usb_audio::SharedAudioState,
    audio_device: Arc<usb_audio::Uac2Device>,
    source_changed: bool,
    previous_source: Option<String>,
    restore_source: Option<Source>,
    started_tx: mpsc::SyncSender<Result<PlayResponse, String>>,
) {
    const SAMPLE_RATE: u32 = 48_000;
    const CHUNK_MS: u32 = 10;
    const FRAMES_PER_CHUNK: usize = (SAMPLE_RATE / (1000 / CHUNK_MS)) as usize; // 480
    const FRAME_BYTES: usize = 4; // 2ch × 16-bit
    const CHUNK_BYTES: usize = FRAMES_PER_CHUNK * FRAME_BYTES; // 1920
    const FREQUENCY_HZ: f32 = 1000.0;
    const AMPLITUDE: f32 = 0.5;
    const DURATION_SEC: u32 = 5;
    const TOTAL_CHUNKS: u32 = DURATION_SEC * (1000 / CHUNK_MS);
    const PREFILL_CHUNKS: u32 = 5; // 50ms

    unsafe {
        esp_idf_sys::esp_wifi_set_ps(esp_idf_sys::wifi_ps_type_t_WIFI_PS_NONE);
    }

    info!(
        "inline sine test: {} Hz, {} sec, {} Hz/{}-bit stereo",
        FREQUENCY_HZ, DURATION_SEC, SAMPLE_RATE, 16
    );

    usb_audio::reset_tx_need_data_count();
    if let Err(err) = audio_device.start_stream(SAMPLE_RATE, BitDepth::Pcm16) {
        let _ = started_tx.send(Err(err.to_string()));
        audio_state.finish_playback();
        unsafe {
            esp_idf_sys::esp_wifi_set_ps(esp_idf_sys::wifi_ps_type_t_WIFI_PS_MIN_MODEM);
        }
        return;
    }

    let phase_inc = 2.0 * core::f32::consts::PI * FREQUENCY_HZ / SAMPLE_RATE as f32;
    let mut phase: f32 = 0.0;
    let mut buf = [0u8; CHUNK_BYTES];

    // Pre-fill 50ms into the ring buffer, matching standalone
    for _ in 0..PREFILL_CHUNKS {
        fill_sine_16bit_stereo(&mut buf, &mut phase, phase_inc, AMPLITUDE);
        if let Err(err) = audio_device.write(&buf, 100) {
            let _ = audio_device.stop_stream();
            let _ = started_tx.send(Err(format!("prefill write: {err}")));
            audio_state.finish_playback();
            unsafe {
                esp_idf_sys::esp_wifi_set_ps(esp_idf_sys::wifi_ps_type_t_WIFI_PS_MIN_MODEM);
            }
            return;
        }
    }

    // Wait for stream to actually start (first SOF)
    let mut started_at_us = audio_device.start_time_us();
    for _ in 0..20 {
        if started_at_us != 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
        started_at_us = audio_device.start_time_us();
    }
    if started_at_us == 0 {
        started_at_us = usb_audio::server_time_us();
    }

    let _ = started_tx.send(Ok(PlayResponse {
        started_at_us,
        tone_burst_offset_samples: 0,
        sweep_offset_samples: 0,
        total_samples: FRAMES_PER_CHUNK * (TOTAL_CHUNKS + PREFILL_CHUNKS) as usize,
        sample_rate: SAMPLE_RATE,
        source_changed,
        previous_source: previous_source.clone(),
    }));

    // Tight write loop — generate inline, write immediately
    let stream_start_us = usb_audio::server_time_us();
    let mut writes: u32 = 0;
    for _ in 0..TOTAL_CHUNKS {
        fill_sine_16bit_stereo(&mut buf, &mut phase, phase_inc, AMPLITUDE);
        match audio_device.write(&buf, 100) {
            Ok(()) => writes += 1,
            Err(err) => {
                warn!("inline sine write failed at chunk {writes}: {err}");
                break;
            }
        }
    }
    let stream_elapsed_ms = (usb_audio::server_time_us() - stream_start_us) / 1000;

    let fb_raw = audio_device.feedback_raw();
    let fb_hz = fb_raw >> 16;
    let fb_frac = ((fb_raw & 0xFFFF) as u64 * 10000) / 65536;
    info!(
        "inline sine test complete: {} writes in {} ms, feedback=0x{:08x} ({}.{:04} Hz), tx_need_data={}",
        writes, stream_elapsed_ms, fb_raw, fb_hz, fb_frac, usb_audio::tx_need_data_count()
    );

    if let Err(err) = audio_device.stop_stream() {
        warn!("UAC2 stop failed after sine test: {err}");
    }

    if let Some(source) = restore_source {
        if let Some(index) = dev.profile().sources.index_for_source(source) {
            let _op = dev.begin_operation();
            if let Err(err) = dev.set_source(index) {
                warn!("failed to restore source after sine test: {err}");
            } else {
                info!("restored source after sine test to {}", source);
            }
        }
    }

    audio_state.finish_playback();
    unsafe {
        esp_idf_sys::esp_wifi_set_ps(esp_idf_sys::wifi_ps_type_t_WIFI_PS_MIN_MODEM);
    }
}

/// Fill a buffer with 16-bit stereo sine samples (same algorithm as
/// the standalone driver test's `tone_gen_fill`).
fn fill_sine_16bit_stereo(buf: &mut [u8], phase: &mut f32, phase_inc: f32, amplitude: f32) {
    let frames = buf.len() / 4; // 2ch × 2 bytes
    for i in 0..frames {
        let sample = amplitude * phase.sin();
        *phase += phase_inc;
        if *phase >= 2.0 * core::f32::consts::PI {
            *phase -= 2.0 * core::f32::consts::PI;
        }
        let val = (sample * 32767.0) as i16;
        let bytes = val.to_le_bytes();
        let offset = i * 4;
        buf[offset] = bytes[0];
        buf[offset + 1] = bytes[1];
        buf[offset + 2] = bytes[0];
        buf[offset + 3] = bytes[1];
    }
}

struct PlaybackStart {
    response: PlayResponse,
    chunk_bytes: usize,
    first_write_bytes: usize,
    first_write_time_us: i64,
    first_write_return_us: i64,
}

fn start_playback_pregenerated(
    audio_device: &usb_audio::Uac2Device,
    plan: &PlaybackPlan,
    pcm_data: &[u8],
) -> Result<PlaybackStart, String> {
    usb_audio::reset_tx_need_data_count();
    audio_device
        .start_stream(plan.request.sample_rate, plan.request.bit_depth)
        .map_err(|e| e.to_string())?;

    // Write 10ms chunks (~1920 bytes at 48kHz/16-bit stereo), matching
    // the standalone driver test harness's write pattern.
    let frames_per_10ms = (plan.request.sample_rate as usize) / 100;
    let chunk_bytes = frames_per_10ms * plan.layout().frame_bytes();

    // Pre-fill 50ms of audio into the ring buffer before the streaming
    // loop starts, matching the standalone driver test harness.
    let prefill_end = (chunk_bytes * 5).min(pcm_data.len());
    let mut prefill_pos = 0;
    let mut prefill_sram = [0u8; 1920];
    let first_write_started_us = usb_audio::server_time_us();
    while prefill_pos < prefill_end {
        let end = (prefill_pos + chunk_bytes).min(prefill_end);
        let len = end - prefill_pos;
        prefill_sram[..len].copy_from_slice(&pcm_data[prefill_pos..end]);
        if let Err(err) = audio_device.write(&prefill_sram[..len], 500) {
            let _ = audio_device.stop_stream();
            return Err(err.to_string());
        }
        prefill_pos = end;
    }
    let first_write_return_us = usb_audio::server_time_us();
    info!(
        "ring buffer pre-filled: {} bytes in {} ms",
        prefill_pos,
        (first_write_return_us - first_write_started_us) / 1000
    );

    let mut started_at_us = audio_device.start_time_us();
    for _ in 0..20 {
        if started_at_us != 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
        started_at_us = audio_device.start_time_us();
    }
    if started_at_us == 0 {
        started_at_us = usb_audio::server_time_us();
    }

    Ok(PlaybackStart {
        response: PlayResponse {
            started_at_us,
            tone_burst_offset_samples: plan.offsets().tone_burst_offset_samples,
            sweep_offset_samples: plan.offsets().sweep_offset_samples,
            total_samples: plan.offsets().total_samples,
            sample_rate: plan.request.sample_rate,
            source_changed: false,
            previous_source: None,
        },
        chunk_bytes,
        first_write_bytes: prefill_pos,
        first_write_time_us: first_write_return_us - first_write_started_us,
        first_write_return_us,
    })
}

fn summarize_write_latencies(samples: &[i64]) -> (i64, i64, i64, i64, i64, i64) {
    if samples.is_empty() {
        return (0, 0, 0, 0, 0, 0);
    }

    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let sum: i64 = sorted.iter().sum();
    let percentile = |n: usize| -> i64 {
        let idx = ((sorted.len() - 1) * n) / 100;
        sorted[idx]
    };

    (
        sorted[0],
        *sorted.last().unwrap_or(&0),
        sum / sorted.len() as i64,
        percentile(50),
        percentile(95),
        percentile(99),
    )
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
                        Some(dev) => {
                            let _op = dev.begin_operation();
                            match dev.get_status_summary() {
                                Ok(s) => (200, status_json(&s)),
                                Err(e) => (
                                    500,
                                    format!(
                                        r#"{{"error":"{}"}}"#,
                                        e.to_string().replace('"', "\\\"")
                                    ),
                                ),
                            }
                        }
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
                            *guard = Some(ActiveWs {
                                stop: stop.clone(),
                                fd,
                            });
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
                        Some(dev) => {
                            let _op = dev.begin_operation();
                            match dev.get_status_summary() {
                                Ok(status) => status_json(&status),
                                Err(_) => r#"{"error":"device read failed"}"#.to_string(),
                            }
                        }
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
        let st = state.clone();

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
                            Some(dev) => match {
                                let _op = dev.begin_operation();
                                dev.raw_command(&payload)
                            } {
                                Ok(resp) => {
                                    let frame = minidsp_protocol::packet::encode_frame(&resp);
                                    let _ = ws.send(FrameType::Binary(false), &frame);
                                }
                                Err(e) => {
                                    log::warn!("WS raw bridge: device error: {e}");
                                }
                            },
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
                        Some(dev) => {
                            let _op = dev.begin_operation();
                            match dev.get_status_summary() {
                                Ok(status) => status_json(&status),
                                Err(_) => r#"{"error":"device read failed"}"#.to_string(),
                            }
                        }
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
    esp_idf_svc::sys::httpd_req_get_hdr_value_len(raw_req, b"Sec-WebSocket-Key\0".as_ptr()) > 0
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
unsafe fn ws_send_http_json(raw_req: *mut esp_idf_svc::sys::httpd_req_t, status: u16, json: &str) {
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
        let _op = dev.begin_operation();
        if let Ok(status) = dev.get_status_summary() {
            let json = status_json(&status);
            if sender
                .send(FrameType::Text(false), json.as_bytes())
                .is_err()
            {
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
            Some(dev) => {
                let _op = dev.begin_operation();
                match dev.get_status_summary() {
                    Ok(status) => status_json(&status),
                    Err(_) => continue,
                }
            }
            None => r#"{"error":"no device"}"#.to_string(),
        };

        if sender
            .send(FrameType::Text(false), json.as_bytes())
            .is_err()
        {
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
                let _op = dev.begin_operation();
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
                    // Browser JS shim handles catalogue loading from our Pages mirror.
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
                                *guard = Some(ActiveWsBeq {
                                    stop: stop.clone(),
                                    fd,
                                });
                            }

                            let dev_state = st.clone();
                            let beq_clone = beq.clone();
                            let ws_ref_clone = ws_ref.clone();
                            if let Err(e) = std::thread::Builder::new()
                                .name("beq_lvl".into())
                                .stack_size(8192)
                                .spawn(move || {
                                    push_levels(
                                        sender,
                                        dev_state,
                                        stop,
                                        beq_clone,
                                        fd,
                                        ws_ref_clone,
                                    );
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
            if std::io::Read::read_exact(&mut f, &mut header).is_ok() && &header[0..4] == b"BEQ\x02"
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
            Some(dev) => {
                let _op = dev.begin_operation();
                match dev.get_status_summary() {
                    Ok(status) => {
                        let mut levels = serde_json::Map::new();
                        for (i, &v) in status.input_levels.iter().enumerate() {
                            levels.insert(format!("I{i}"), serde_json::Value::from(v));
                        }
                        for (i, &v) in status.output_levels.iter().enumerate() {
                            levels.insert(format!("O{i}"), serde_json::Value::from(v));
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
                }
            }
            None => continue,
        };

        if sender
            .send(FrameType::Text(false), json.as_bytes())
            .is_err()
        {
            break;
        }
    }

    log::info!("levels push thread stopped");

    // Only return sender if the session is still the same (no new client replaced us)
    let still_active = active_ws
        .lock()
        .ok()
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
