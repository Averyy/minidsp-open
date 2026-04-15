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
use crate::usb_hid::UsbHidError;
use esp_idf_hal::task::thread::{MallocCap, ThreadSpawnConfiguration};
use esp_idf_svc::http::client::{Configuration as HttpClientConfiguration, EspHttpConnection};
use esp_idf_svc::http::server::{Configuration, EspHttpServer};
use esp_idf_svc::http::Method;
use esp_idf_svc::io::Write;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use log::{debug, info, warn};
use minidsp_protocol::commands;
use minidsp_protocol::playback::{
    BitDepth, PlayRequest, PlayResponse, PlaybackPlan, SyncRequest, SyncResponse, ToneBurstParams,
};
use minidsp_protocol::source::Source;
use minidsp_protocol::status::{BmChannelPatch, BmMixerCellPatch, DeviceConfig, StatusSummary};
use serde_json::json;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const REBOOT_RECOVERY_TIMEOUT: Duration = Duration::from_secs(50);
const SOURCE_SWITCH_RECOVERY_TIMEOUT: Duration = Duration::from_secs(35);
const AUDIO_REOPEN_TIMEOUT: Duration = Duration::from_secs(5);
const AUDIO_CLOSE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_PLAYBACK_BYTES: usize = 4 * 1024 * 1024;
const PLAYBACK_THREAD_STACK_BYTES: usize = 18 * 1024;
const PLAYBACK_STREAM_TARGET_CHUNK_MS: usize = 2;
const PLAYBACK_PROGRESS_LOG_EVERY_WRITES: usize = 500;
pub(crate) const HID_BUSY_DURING_PLAYBACK: &str = "device busy during playback";
static PLAYBACK_STREAM_BUF: Mutex<[u8; 2880]> = Mutex::new([0u8; 2880]);

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
        // STA mode with ezBEQ enabled now registers into the 70s:
        // core device routes, debug routes, WS endpoints, ezBEQ API, UI/static
        // routes, and CORS/catch-all handlers. Keep real headroom so small
        // diagnostics additions don't tear down the entire HTTP server at boot.
        max_uri_handlers: 88,
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
        let audio = audio_state.clone();
        server.fn_handler("/devices/0", Method::Get, move |req| {
            let req = match reject_hid_during_playback(req, &audio)? {
                Some(req) => req,
                None => return Ok(()),
            };
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
        let audio = audio_state.clone();
        server.fn_handler("/devices/0", Method::Post, move |mut req| {
            req = match reject_hid_during_playback(req, &audio)? {
                Some(req) => req,
                None => return Ok(()),
            };
            let dev = match get_device(&st) {
                Some(d) => d,
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
            };
            let body = match read_request_body(&mut req, 1024) {
                Ok(b) => b,
                Err(msg) => return send_error(req, 413, msg),
            };
            let json: serde_json::Value = match serde_json::from_slice(&body) {
                Ok(v) => v,
                Err(_) => return send_error(req, 400, "invalid JSON"),
            };
            if let Some(vol) = json.get("volume").and_then(|v| v.as_f64()) {
                let result = {
                    let _op = dev.begin_operation();
                    dev.set_volume_db(vol as f32)
                };
                if let Err(e) = result {
                    return send_error(req, 500, &format!("set_volume: {e}"));
                }
            }
            if let Some(mute) = json.get("mute").and_then(|v| v.as_bool()) {
                let result = {
                    let _op = dev.begin_operation();
                    dev.set_mute(mute)
                };
                if let Err(e) = result {
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
                if let Err(e) = set_source_with_audio_coordination(
                    &st,
                    &audio,
                    &dev,
                    src,
                    SOURCE_SWITCH_RECOVERY_TIMEOUT,
                    "master status update",
                ) {
                    return send_error(req, 500, &e);
                }
            }
            if let Some(preset) = json.get("preset").and_then(|v| v.as_u64()) {
                if preset > u8::MAX as u64 {
                    return send_error(req, 400, "preset out of range");
                }
                let result = {
                    let _op = dev.begin_operation();
                    dev.set_preset(preset as u8)
                };
                if let Err(e) = result {
                    return send_error(req, 500, &format!("set_preset: {e}"));
                }
            }
            if let Some(dirac) = json.get("dirac").and_then(|v| v.as_bool()) {
                let result = {
                    let _op = dev.begin_operation();
                    dev.set_dirac_bypass(!dirac)
                };
                match result {
                    Ok(()) => {}
                    Err(e) if e.to_string().contains("unsupported") => {}
                    Err(e) => return send_error(req, 500, &format!("set_dirac: {e}")),
                }
            }

            let status = {
                let _op = dev.begin_operation();
                dev.get_status_summary()
            };
            match status {
                Ok(status) => send_json(req, 200, &master_status_json(&status)),
                Err(e) => send_error(req, 500, &e.to_string()),
            }
        })?;
    }

    // Compatibility routes: volume direction
    register_volume_routes(&mut server, state.clone(), audio_state.clone())?;

    // Source and preset routes
    register_source_routes(&mut server, state.clone(), audio_state.clone())?;
    register_preset_routes(&mut server, state.clone(), audio_state.clone())?;

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
            let uri = req.uri().to_string();
            let Some(device_id) = parse_play_path(&uri) else {
                return send_error(req, 404, "not found");
            };
            if device_id != 0 {
                return send_error(req, 404, "not found");
            }

            let dev = match get_device(&st) {
                Some(d) => d,
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
            };
            if !dev.profile().has_usb_audio {
                return send_error(req, 400, "device does not support USB audio playback");
            }
            let audio_device =
                match ensure_audio_device_available(&audio, "play request needs USB audio") {
                    Ok(device) => device,
                    Err(message) => return send_error(req, 503, &message),
                };
            if !audio.try_begin_playback() {
                return send_error(req, 409, "playback already in progress");
            }

            let test_mode = parse_query_param(&uri, "test");

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
                    Err(err) => {
                        audio.finish_playback();
                        return send_error(req, 400, &format!("invalid play request: {err}"));
                    }
                }
            };

            let plan = match test_mode.as_deref() {
                Some("marker") | Some("tail-marker") => {
                    let tone = ToneBurstParams {
                        frequency: play_request.marker_frequency_hz,
                        cycles: play_request.marker_cycles,
                    };
                    PlaybackPlan::diagnostic_tail_marker(
                        tone,
                        play_request.sample_rate,
                        play_request.bit_depth,
                        play_request.decay_sec,
                    )
                    .map_err(|err| err.to_string())
                }
                Some(other) => Err(format!("unknown play test mode: {other}")),
                None => PlaybackPlan::build(play_request).map_err(|err| err.to_string()),
            };
            let plan = match plan {
                Ok(plan) => plan,
                Err(message) => {
                    audio.finish_playback();
                    return send_error(req, 400, &message);
                }
            };
            let total_bytes = match validate_esp32_playback_size(&plan) {
                Ok(total_bytes) => total_bytes,
                Err(message) => {
                    audio.finish_playback();
                    return send_error(req, 413, &message);
                }
            };

            let (dev_for_thread, source_changed, previous_source, restore_source) =
                match switch_to_usb_source(&st, &dev) {
                    Ok(result) => result,
                    Err((code, msg)) => {
                        audio.finish_playback();
                        return send_error(req, code, &msg);
                    }
                };

            let (tx, rx) = mpsc::sync_channel(1);
            let audio_for_thread = audio.clone();
            let restore_source_for_error = restore_source;
            if let Err(err) = spawn_playback_thread(
                st.clone(),
                dev_for_thread.clone(),
                audio_for_thread,
                audio_device,
                Box::new(plan),
                total_bytes,
                source_changed,
                previous_source,
                restore_source,
                tx,
            ) {
                if let Err(restore_err) = restore_source_after_playback(
                    &st,
                    &audio,
                    &dev_for_thread,
                    restore_source_for_error,
                    "playback spawn failure",
                ) {
                    warn!("{restore_err}");
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
        let audio = audio_state.clone();
        server.fn_handler("/devices/0/config", Method::Get, move |req| {
            let req = match reject_hid_during_playback(req, &audio)? {
                Some(req) => req,
                None => return Ok(()),
            };
            let dev = match get_device(&st) {
                Some(d) => d,
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
            };
            let _op = dev.begin_operation();
            match dev.read_device_config() {
                Ok(config) => {
                    let body = config_json(&dev, config);
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
        let audio = audio_state.clone();
        let nvs_clone = nvs.clone();
        server.fn_handler("/devices/0/config", Method::Post, move |mut req| {
            req = match reject_hid_during_playback(req, &audio)? {
                Some(req) => req,
                None => return Ok(()),
            };
            let dev = match get_device(&st) {
                Some(d) => d,
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
            };
            let body = match read_request_body(&mut req, 128 * 1024) {
                Ok(body) => body,
                Err("request body too large") => {
                    return send_error(req, 413, "request body too large");
                }
                Err(_) => return send_error(req, 400, "failed to read request body"),
            };

            info!("POST /devices/0/config start: {} bytes", body.len());
            let json: serde_json::Value = match serde_json::from_slice(&body) {
                Ok(v) => v,
                Err(_) => return send_error(req, 400, "invalid JSON"),
            };

            let requested_source = if let Some(source) = json
                .get("master_status")
                .and_then(|ms| ms.get("source"))
                .and_then(|v| v.as_str())
            {
                match minidsp_protocol::source::Source::from_str_pascal(source)
                    .or_else(|| minidsp_protocol::source::Source::from_str_lowercase(source))
                {
                    Some(src) => Some(src),
                    None => return send_error(req, 400, &format!("unknown source '{source}'")),
                }
            } else {
                None
            };

            if let Some(source) = requested_source {
                if let Err(err) = set_source_with_audio_coordination(
                    &st,
                    &audio,
                    &dev,
                    source,
                    SOURCE_SWITCH_RECOVERY_TIMEOUT,
                    "config master_status update",
                ) {
                    return send_error(req, 500, &err);
                }
            }

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
                    let input_ch = match table.inputs.get(idx) {
                        Some(ch) => ch,
                        None => return send_error(req, 400, "input index out of range"),
                    };
                    // PEQ bands
                    if let Some(peqs) = input.get("peq").and_then(|v| v.as_array()) {
                        for peq in peqs {
                            let band_idx = match peq.get("index").and_then(|v| v.as_u64()) {
                                Some(i) => i as usize,
                                None => return send_error(req, 400, "peq missing 'index' field"),
                            };
                            let band = match input_ch.peq.get(band_idx) {
                                Some(band) => band,
                                None => {
                                    return send_error(req, 400, "input peq index out of range")
                                }
                            };
                            if let Some(coeff) = peq.get("coeff") {
                                let c = match parse_biquad_coefficients(coeff) {
                                    Ok(coeffs) => coeffs,
                                    Err(msg) => return send_error(req, 400, msg),
                                };
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
                                    return send_error(req, 400, "routing missing 'output' field");
                                }
                            };
                            let enabled = match route.get("enabled").and_then(|v| v.as_bool()) {
                                Some(v) => v,
                                None => {
                                    return send_error(req, 400, "routing missing 'enabled' field");
                                }
                            };
                            let gain_db = match route.get("gain_db").and_then(|v| v.as_f64()) {
                                Some(v) if v.is_finite() => v as f32,
                                Some(_) => {
                                    return send_error(req, 400, "routing gain must be finite");
                                }
                                None => {
                                    return send_error(req, 400, "routing missing 'gain_db' field");
                                }
                            };
                            if let Err(e) = dev.set_routing(idx as u8, output, enabled, gain_db) {
                                return send_error(req, 500, &format!("set_routing: {e}"));
                            }
                            if let Some(inverted) =
                                route.get("polarity_inverted").and_then(|v| v.as_bool())
                            {
                                if let Err(e) =
                                    dev.set_routing_polarity(idx as u8, output, inverted)
                                {
                                    return send_error(
                                        req,
                                        500,
                                        &format!("set_routing_polarity: {e}"),
                                    );
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
                    let output_ch = match table.outputs.get(idx) {
                        Some(ch) => ch,
                        None => return send_error(req, 400, "output index out of range"),
                    };
                    // PEQ bands
                    if let Some(peqs) = output.get("peq").and_then(|v| v.as_array()) {
                        for peq in peqs {
                            let band_idx = match peq.get("index").and_then(|v| v.as_u64()) {
                                Some(i) => i as usize,
                                None => return send_error(req, 400, "peq missing 'index' field"),
                            };
                            let band = match output_ch.peq.get(band_idx) {
                                Some(band) => band,
                                None => {
                                    return send_error(req, 400, "output peq index out of range")
                                }
                            };
                            info!(
                                "config write output[{idx}] peq[{band_idx}] addr=0x{:04x}",
                                band.addr
                            );
                            if let Some(coeff) = peq.get("coeff") {
                                let c = match parse_biquad_coefficients(coeff) {
                                    Ok(coeffs) => coeffs,
                                    Err(msg) => return send_error(req, 400, msg),
                                };
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
                            let nanos = delay.get("nanos").and_then(|v| v.as_f64()).unwrap_or(0.0);
                            s + nanos / 1_000_000_000.0
                        } else {
                            delay.as_f64().unwrap_or(0.0)
                        };
                        debug!("config write output[{idx}] delay_secs -> {secs}");
                        if let Some(delay_addr) = output_ch.delay {
                            let delay_ms = secs as f32 * 1000.0;
                            let samples = dev.profile().delay_ms_to_samples(delay_ms);
                            if let Err(e) = dev.set_channel_delay_samples(delay_addr, samples) {
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
                    if let Some(crossover_groups) =
                        output.get("crossover").and_then(|v| v.as_array())
                    {
                        if let Some(xover) = output_ch.crossover {
                            for group in crossover_groups {
                                let group_idx = match group.get("index").and_then(|v| v.as_u64()) {
                                    Some(v) => v as u8,
                                    None => {
                                        return send_error(
                                            req,
                                            400,
                                            "crossover missing 'index' field",
                                        );
                                    }
                                };
                                let base_addr = match group_idx {
                                    0 => xover.lpf_addr,
                                    1 => xover.hpf_addr,
                                    _ => {
                                        return send_error(
                                            req,
                                            400,
                                            "crossover index out of range",
                                        );
                                    }
                                };
                                if let Some(filters) =
                                    group.get("filters").and_then(|v| v.as_array())
                                {
                                    for filter in filters {
                                        let filter_idx =
                                            match filter.get("index").and_then(|v| v.as_u64()) {
                                                Some(v) if v < 4 => v as u16,
                                                Some(_) => {
                                                    return send_error(
                                                        req,
                                                        400,
                                                        "crossover filter index out of range",
                                                    );
                                                }
                                                None => {
                                                    return send_error(
                                                        req,
                                                        400,
                                                        "crossover filter missing 'index' field",
                                                    );
                                                }
                                            };
                                        if let Some(coeff) = filter.get("coeff") {
                                            let c = match parse_biquad_coefficients(coeff) {
                                                Ok(coeffs) => coeffs,
                                                Err(msg) => return send_error(req, 400, msg),
                                            };
                                            if let Err(e) =
                                                dev.write_biquad(base_addr + filter_idx * 5, &c)
                                            {
                                                return send_error(
                                                    req,
                                                    500,
                                                    &format!("write_crossover_biquad: {e}"),
                                                );
                                            }
                                        }
                                    }
                                }
                                if let Some(bypass) = group.get("bypass").and_then(|v| v.as_bool())
                                {
                                    if let Err(e) = dev.bypass_filter(base_addr, bypass) {
                                        return send_error(
                                            req,
                                            500,
                                            &format!("set_crossover_bypass: {e}"),
                                        );
                                    }
                                }
                            }
                        }
                    }
                    if let Some(fir) = output.get("fir") {
                        if let Some(fir_def) = output_ch.fir {
                            if let Some(bypass) = fir.get("bypass").and_then(|v| v.as_bool()) {
                                if let Err(e) = dev.set_status_bypass(fir_def.bypass, bypass) {
                                    return send_error(req, 500, &format!("set_fir_bypass: {e}"));
                                }
                            }
                            if let Some(coefficients) =
                                fir.get("coefficients").and_then(|v| v.as_array())
                            {
                                let mut taps = Vec::with_capacity(coefficients.len());
                                for coeff in coefficients {
                                    let Some(value) = coeff.as_f64() else {
                                        return send_error(
                                            req,
                                            400,
                                            "fir coefficients must be numbers",
                                        );
                                    };
                                    if !value.is_finite() {
                                        return send_error(
                                            req,
                                            400,
                                            "fir coefficients must be finite",
                                        );
                                    }
                                    taps.push(value as f32);
                                }
                                if let Err(e) = dev.load_fir_coefficients(fir_def, &taps) {
                                    return send_error(req, 500, &format!("load_fir: {e}"));
                                }
                            }
                        }
                    }
                    if let Some(compressor) = output.get("compressor") {
                        if let Some(comp) = output_ch.compressor {
                            if let Some(bypass) = compressor.get("bypass").and_then(|v| v.as_bool())
                            {
                                if let Err(e) = dev.set_status_bypass(comp.bypass, bypass) {
                                    return send_error(
                                        req,
                                        500,
                                        &format!("set_compressor_bypass: {e}"),
                                    );
                                }
                            }
                            for (name, addr) in [
                                ("threshold", comp.threshold),
                                ("gain", comp.gain),
                                ("ratio", comp.ratio),
                                ("knee", comp.knee),
                                ("attack", comp.attack),
                                ("release", comp.release),
                            ] {
                                if let Some(value) = compressor.get(name).and_then(|v| v.as_f64()) {
                                    if !value.is_finite() {
                                        return send_error(
                                            req,
                                            400,
                                            &format!("compressor {name} must be finite"),
                                        );
                                    }
                                    if let Err(e) = dev.write_dsp_verified(
                                        addr,
                                        value as f32,
                                        true,
                                        &format!("set compressor {name}"),
                                    ) {
                                        return send_error(
                                            req,
                                            500,
                                            &format!("set_compressor_{name}: {e}"),
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Bulk-commit boundary: once a full ezBEQ profile has landed, snapshot
            // the in-memory PEQ shadow to NVS so a reboot doesn't discard it on
            // selector-unreliable hardware. Errors are logged inside the helper.
            dev.save_peq_shadow_to_nvs(&nvs_clone);

            info!("POST /devices/0/config completed");
            send_empty_200(req)
        })?;
    }

    // Display, DRE, tokens, preset copy, measurement-mode
    register_device_feature_routes(
        &mut server,
        state.clone(),
        audio_state.clone(),
        nvs.clone(),
        beq_state.clone(),
    )?;

    // Debug / diagnostics endpoints
    register_debug_routes(&mut server, state.clone(), audio_state.clone(), nvs.clone())?;

    // WebSocket for status streaming (gated on ESP-IDF httpd WS support)
    #[cfg(esp_idf_httpd_ws_support)]
    register_ws_routes(&mut server, state.clone(), audio_state.clone())?;

    // ezBEQ REST API + WebSocket (only in STA mode with beq_state)
    if let Some(ref beq) = beq_state {
        crate::ezbeq::register_routes(
            &mut server,
            state.clone(),
            audio_state.clone(),
            beq.clone(),
            nvs.clone(),
        )?;
        #[cfg(esp_idf_httpd_ws_support)]
        register_ezbeq_ws(&mut server, state.clone(), audio_state.clone(), beq.clone())?;
    }

    // JSON fallback for unsupported /devices/* mutations. Register after all
    // concrete device routes so exact handlers and WS endpoints always win.
    register_device_api_fallbacks(&mut server)?;

    // GET /openapi.json
    server.fn_handler("/openapi.json", Method::Get, |req| {
        let spec = openapi_json();
        send_json(req, 200, &spec)
    })?;

    // GET /llms.txt
    server.fn_handler("/llms.txt", Method::Get, |req| {
        send_text(
            req,
            "# minidsp-open\nMiniDSP audio processor controller.\nHTTP API on port 5380.\n",
        )
    })?;

    // GET /database.json — same-origin proxy for the full ezBEQ catalogue JSON.
    // The board stores only the compact biquad index locally; proxy the larger
    // browser catalogue from the configured upstream so the UI avoids CORS issues
    // and still talks to the board origin only.
    {
        let nvs_catalogue = nvs.clone();
        server.fn_handler("/database.json", Method::Get, move |req| {
            proxy_catalogue_file(req, &nvs_catalogue, "database.json", "application/json")
        })?;
    }

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
        let body_bytes = match read_request_body(&mut req, 512) {
            Ok(b) => b,
            Err(msg) => return send_provision_error(req, msg),
        };
        let body = std::str::from_utf8(&body_bytes).unwrap_or("");

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
        let headers = [("Server", "miniDSP-Open"), ("Content-Type", "text/html")];
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
            let relative = path.trim_start_matches('/');

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

            if !relative.is_empty() {
                let storage_path = crate::storage::full_path(relative);
                if std::path::Path::new(&storage_path).is_file() {
                    return send_static_file(req, &storage_path);
                }
            }

            let file_path = crate::storage::full_path(&format!("ui{path}"));

            if std::path::Path::new(&file_path).is_file() {
                send_static_file(req, &file_path)
            } else if relative.contains('.') {
                send_json(req, 404, r#"{"error":"not found"}"#)
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
    audio_state: usb_audio::SharedAudioState,
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
        let audio = audio_state.clone();
        server.fn_handler("/debug/dsp", Method::Get, move |req| {
            let req = match reject_hid_during_playback(req, &audio)? {
                Some(req) => req,
                None => return Ok(()),
            };
            let dev = match get_device(&st) {
                Some(d) => d,
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
            };

            let uri = req.uri().to_string();
            let query = uri.split_once('?').map(|(_, q)| q).unwrap_or("");
            let mut addr: Option<u16> = None;
            let mut count: u8 = 1;
            let mut format: Option<&str> = None;

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
                        format = Some(value);
                    }
                    _ => {}
                }
            }

            let Some(addr) = addr else {
                return send_error(req, 400, "missing addr query parameter");
            };
            let _op = dev.begin_operation();

            let body = match format {
                Some("words") => match dev.read_dsp_words(addr, count) {
                    Ok(values) => serde_json::to_string(&json!({
                        "addr": addr,
                        "count": count,
                        "format": "words",
                        "values": values,
                    }))
                    .unwrap_or_default(),
                    Err(e) => return send_error(req, 500, &e.to_string()),
                },
                Some("floats") => match dev.read_dsp_floats(addr, count) {
                    Ok(values) => serde_json::to_string(&json!({
                        "addr": addr,
                        "count": count,
                        "format": "floats",
                        "values": values,
                    }))
                    .unwrap_or_default(),
                    Err(e) => return send_error(req, 500, &e.to_string()),
                },
                None => {
                    // Auto-pick: float decode is strict and rejects anything that
                    // doesn't look like a plausible DSP parameter (EEPROM pages,
                    // unused addresses, etc.). Try floats first, then fall back to
                    // words so /debug/dsp can inspect any address.
                    match dev.read_dsp_floats(addr, count) {
                        Ok(values) => serde_json::to_string(&json!({
                            "addr": addr,
                            "count": count,
                            "format": "floats",
                            "values": values,
                        }))
                        .unwrap_or_default(),
                        Err(_) => match dev.read_dsp_words(addr, count) {
                            Ok(values) => serde_json::to_string(&json!({
                                "addr": addr,
                                "count": count,
                                "format": "words",
                                "values": values,
                            }))
                            .unwrap_or_default(),
                            Err(e) => return send_error(req, 500, &e.to_string()),
                        },
                    }
                }
                _ => unreachable!(),
            };

            send_json(req, 200, &body)
        })?;
    }

    // POST /debug/level — change runtime log level
    server.fn_handler("/debug/level", Method::Post, |mut req| {
        let body = match read_request_body(&mut req, 256) {
            Ok(b) => b,
            Err(msg) => return send_error(req, 413, msg),
        };

        let level_str = if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&body) {
            json.get("level")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        } else {
            // Try plain text body
            std::str::from_utf8(&body).unwrap_or("").trim().to_string()
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
            let body = match read_request_body(&mut req, 512) {
                Ok(b) => b,
                Err(msg) => return send_error(req, 413, msg),
            };
            let json: serde_json::Value = match serde_json::from_slice(&body) {
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
    audio_state: usb_audio::SharedAudioState,
) -> Result<(), esp_idf_svc::sys::EspError> {
    let st = state.clone();
    let audio = audio_state.clone();
    server.fn_handler("/devices/0/volume/*", Method::Post, move |req| {
        let req = match reject_hid_during_playback(req, &audio)? {
            Some(req) => req,
            None => return Ok(()),
        };
        let dev = match get_device(&st) {
            Some(d) => d,
            None => return send_json(req, 503, r#"{"error":"no device"}"#),
        };
        let uri = req.uri().to_string();
        let Some(direction) = parse_volume_path(&uri) else {
            return send_error(req, 400, "invalid volume path");
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
            _ => return send_error(req, 400, "unknown volume direction"),
        };
        if let Err(e) = result {
            return send_error(req, 500, &e.to_string());
        }
        match dev.get_status_summary() {
            Ok(status) => send_json(req, 200, &master_status_json(&status)),
            Err(e) => send_error(req, 500, &e.to_string()),
        }
    })?;
    Ok(())
}

/// Register a wildcard source route. The source name is parsed from the URI
/// at request time so PascalCase names and device-specific sources (bluetooth,
/// rca/xlr, lan, etc.) work even though the connected device is unknown at
/// boot.
fn register_source_routes(
    server: &mut EspHttpServer<'static>,
    state: SharedDeviceState,
    audio_state: usb_audio::SharedAudioState,
) -> Result<(), esp_idf_svc::sys::EspError> {
    let st = state.clone();
    let audio = audio_state.clone();
    server.fn_handler("/devices/0/source/*", Method::Post, move |req| {
        let req = match reject_hid_during_playback(req, &audio)? {
            Some(req) => req,
            None => return Ok(()),
        };
        let dev = match get_device(&st) {
            Some(d) => d,
            None => return send_json(req, 503, r#"{"error":"no device"}"#),
        };
        let uri = req.uri().to_string();
        let Some(source_name) = parse_source_path(&uri) else {
            return send_error(req, 400, "invalid source path");
        };
        let src = match Source::from_str_lowercase(&source_name)
            .or_else(|| Source::from_str_pascal(&source_name))
        {
            Some(s) => s,
            None => return send_error(req, 400, "unknown source"),
        };
        match set_source_with_audio_coordination(
            &st,
            &audio,
            &dev,
            src,
            SOURCE_SWITCH_RECOVERY_TIMEOUT,
            "manual source change",
        ) {
            Ok((_, status)) => send_json(req, 200, &master_status_json(&status)),
            Err(err) => send_error(req, 500, &err),
        }
    })?;
    Ok(())
}

fn register_preset_routes(
    server: &mut EspHttpServer<'static>,
    state: SharedDeviceState,
    audio_state: usb_audio::SharedAudioState,
) -> Result<(), esp_idf_svc::sys::EspError> {
    for preset in 0..=3u8 {
        let st = state.clone();
        let audio = audio_state.clone();
        let path = format!("/devices/0/preset/{preset}");
        server.fn_handler(&path, Method::Post, move |req| {
            let req = match reject_hid_during_playback(req, &audio)? {
                Some(req) => req,
                None => return Ok(()),
            };
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
    audio_state: usb_audio::SharedAudioState,
    nvs: EspDefaultNvsPartition,
    beq_state: Option<crate::ezbeq::SharedEzBeqState>,
) -> Result<(), esp_idf_svc::sys::EspError> {
    // GET /devices/0/display
    {
        let st = state.clone();
        let audio = audio_state.clone();
        server.fn_handler("/devices/0/display", Method::Get, move |req| {
            let req = match reject_hid_during_playback(req, &audio)? {
                Some(req) => req,
                None => return Ok(()),
            };
            let dev = match get_device(&st) {
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
                Some(d) if !d.profile().has_display => {
                    return send_json(req, 404, r#"{"error":"not supported"}"#);
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
        let audio = audio_state.clone();
        server.fn_handler("/devices/0/display", Method::Post, move |mut req| {
            req = match reject_hid_during_playback(req, &audio)? {
                Some(req) => req,
                None => return Ok(()),
            };
            let dev = match get_device(&st) {
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
                Some(d) if !d.profile().has_display => {
                    return send_json(req, 404, r#"{"error":"not supported"}"#);
                }
                Some(d) => d,
            };
            let body = match read_request_body(&mut req, 512) {
                Ok(b) => b,
                Err(msg) => return send_error(req, 413, msg),
            };
            let json: serde_json::Value = match serde_json::from_slice(&body) {
                Ok(v) => v,
                Err(_) => return send_error(req, 400, "invalid JSON"),
            };
            let _op = dev.begin_operation();
            if let Some(b) = json.get("brightness").and_then(|v| v.as_u64()) {
                if b > u8::MAX as u64 {
                    return send_error(req, 400, "brightness out of range (0-255)");
                }
                if let Err(e) = dev.set_display_brightness(b as u8) {
                    return send_error(req, 500, &format!("set_display_brightness: {e}"));
                }
            }
            if let Some(t) = json.get("idle_time").and_then(|v| v.as_u64()) {
                if t > u8::MAX as u64 {
                    return send_error(req, 400, "idle_time out of range (0-255)");
                }
                if let Err(e) = dev.set_display_idle_time(t as u8) {
                    return send_error(req, 500, &format!("set_display_idle_time: {e}"));
                }
            }
            send_json(req, 200, r#"{"status":"ok"}"#)
        })?;
    }

    // GET /devices/0/dre
    {
        let st = state.clone();
        let audio = audio_state.clone();
        server.fn_handler("/devices/0/dre", Method::Get, move |req| {
            let req = match reject_hid_during_playback(req, &audio)? {
                Some(req) => req,
                None => return Ok(()),
            };
            let dev = match get_device(&st) {
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
                Some(d) if !d.profile().has_dre => {
                    return send_json(req, 404, r#"{"error":"not supported"}"#);
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
        let audio = audio_state.clone();
        server.fn_handler("/devices/0/dre", Method::Post, move |mut req| {
            req = match reject_hid_during_playback(req, &audio)? {
                Some(req) => req,
                None => return Ok(()),
            };
            let dev = match get_device(&st) {
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
                Some(d) if !d.profile().has_dre => {
                    return send_json(req, 404, r#"{"error":"not supported"}"#);
                }
                Some(d) => d,
            };
            let body = match read_request_body(&mut req, 256) {
                Ok(b) => b,
                Err(msg) => return send_error(req, 413, msg),
            };
            let json: serde_json::Value = match serde_json::from_slice(&body) {
                Ok(v) => v,
                Err(_) => return send_error(req, 400, "invalid JSON"),
            };
            let _op = dev.begin_operation();
            if let Some(enabled) = json.get("enabled").and_then(|v| v.as_bool()) {
                if let Err(e) = dev.set_dre(enabled) {
                    return send_error(req, 500, &format!("set_dre: {e}"));
                }
            }
            send_json(req, 200, r#"{"status":"ok"}"#)
        })?;
    }

    // GET /devices/0/tokens
    {
        let st = state.clone();
        let audio = audio_state.clone();
        server.fn_handler("/devices/0/tokens", Method::Get, move |req| {
            let req = match reject_hid_during_playback(req, &audio)? {
                Some(req) => req,
                None => return Ok(()),
            };
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

    // GET /devices/0/verify-program
    {
        let st = state.clone();
        let audio = audio_state.clone();
        server.fn_handler("/devices/0/verify-program", Method::Get, move |req| {
            let req = match reject_hid_during_playback(req, &audio)? {
                Some(req) => req,
                None => return Ok(()),
            };
            let dev = match get_device(&st) {
                Some(d) => d,
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
            };
            let _op = dev.begin_operation();
            match dev.read_flash_data(0xFF, 0xA3, 9) {
                Ok(bytes) => {
                    let status = bytes.first().copied().unwrap_or(0);
                    let body = serde_json::to_string(&serde_json::json!({
                        "status": status,
                        "ok": minidsp_protocol::eeprom::is_verify_program_ok(status),
                        "bytes": bytes,
                    }))
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
        let audio = audio_state.clone();
        server.fn_handler("/devices/0/preset/copy", Method::Post, move |req| {
            let req = match reject_hid_during_playback(req, &audio)? {
                Some(req) => req,
                None => return Ok(()),
            };
            let dev = match get_device(&st) {
                Some(d) => d,
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
            };
            let _op = dev.begin_operation();
            match dev.copy_preset() {
                Ok(()) => send_empty_200(req),
                Err(e) => send_error(req, 500, &e.to_string()),
            }
        })?;
    }

    // POST /devices/0/preset/reset
    {
        let st = state.clone();
        let audio = audio_state.clone();
        server.fn_handler("/devices/0/preset/reset", Method::Post, move |req| {
            let req = match reject_hid_during_playback(req, &audio)? {
                Some(req) => req,
                None => return Ok(()),
            };
            let dev = match get_device(&st) {
                Some(d) => d,
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
            };
            if !dev.supports_default_preset_reset() {
                return send_error(req, 501, "default preset reset not supported");
            }
            match dev.reset_active_preset_to_defaults() {
                Ok(preset) => {
                    let body = serde_json::to_string(&json!({
                        "status": "preset_reset",
                        "preset": preset,
                    }))
                    .unwrap_or_default();
                    send_json(req, 200, &body)
                }
                Err(e) => send_error(req, 500, &e.to_string()),
            }
        })?;
    }

    // POST /devices/0/preset/{preset}/reset
    {
        let st = state.clone();
        let audio = audio_state.clone();
        // Safe because the handler captures only owned `Arc` state and the
        // server owns the closure for the same lifetime as `state`.
        unsafe {
            server.fn_handler_nonstatic("/devices/0/preset/*/reset", Method::Post, move |req| {
                let req = match reject_hid_during_playback(req, &audio)? {
                    Some(req) => req,
                    None => return Ok(()),
                };
                let dev = match get_device(&st) {
                    Some(d) => d,
                    None => return send_json(req, 503, r#"{"error":"no device"}"#),
                };
                if !dev.supports_default_preset_reset() {
                    return send_error(req, 501, "default preset reset not supported");
                }
                let path = req.uri().trim_matches('/');
                let parts: Vec<_> = path.split('/').collect();
                let Some(&preset_text) = parts.get(3) else {
                    return send_error(req, 400, "missing preset");
                };
                let Ok(preset) = preset_text.parse::<u8>() else {
                    return send_error(req, 400, "invalid preset");
                };
                match dev.reset_preset_to_defaults(preset) {
                    Ok(()) => {
                        let body = serde_json::to_string(&json!({
                            "status": "preset_reset",
                            "preset": preset,
                        }))
                        .unwrap_or_default();
                        send_json(req, 200, &body)
                    }
                    Err(e) => send_error(req, 500, &e.to_string()),
                }
            })?;
        }
    }

    // POST /devices/0/preset/reset-all
    {
        let st = state.clone();
        let audio = audio_state.clone();
        server.fn_handler("/devices/0/preset/reset-all", Method::Post, move |req| {
            let req = match reject_hid_during_playback(req, &audio)? {
                Some(req) => req,
                None => return Ok(()),
            };
            let dev = match get_device(&st) {
                Some(d) => d,
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
            };
            if !dev.supports_default_preset_reset() {
                return send_error(req, 501, "default preset reset not supported");
            }
            match dev.initialize_all_presets_to_defaults(true) {
                Ok(()) => match wait_for_rebooted_device(&st, &dev, REBOOT_RECOVERY_TIMEOUT) {
                    Ok(()) => send_json(
                        req,
                        200,
                        r#"{"status":"all_presets_reset","rebooted":true}"#,
                    ),
                    Err(e) => send_error(req, 500, &e),
                },
                Err(e) => send_error(req, 500, &e.to_string()),
            }
        })?;
    }

    // POST /devices/0/reboot
    {
        let st = state.clone();
        let audio = audio_state.clone();
        server.fn_handler("/devices/0/reboot", Method::Post, move |req| {
            let req = match reject_hid_during_playback(req, &audio)? {
                Some(req) => req,
                None => return Ok(()),
            };
            let dev = match get_device(&st) {
                Some(d) => d,
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
            };
            match dev.reboot_device() {
                Ok(()) => match wait_for_rebooted_device(&st, &dev, REBOOT_RECOVERY_TIMEOUT) {
                    Ok(()) => send_json(req, 200, r#"{"status":"rebooted"}"#),
                    Err(e) => send_error(req, 500, &e),
                },
                Err(e) => send_error(req, 500, &e.to_string()),
            }
        })?;
    }

    // POST /devices/0/measurement-mode — snapshot PEQ state and bypass every
    // band so a REW sweep hits the device unfiltered.
    {
        let st = state.clone();
        let audio = audio_state.clone();
        let nvs = nvs.clone();
        server.fn_handler("/devices/0/measurement-mode", Method::Post, move |req| {
            let req = match reject_hid_during_playback(req, &audio)? {
                Some(req) => req,
                None => return Ok(()),
            };
            let dev = match get_device(&st) {
                Some(d) => d,
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
            };
            let _op = dev.begin_operation();
            match dev.enter_measurement_mode() {
                Ok(()) => {
                    dev.save_peq_shadow_to_nvs(&nvs);
                    send_json(req, 200, r#"{"status":"measurement_mode_active"}"#)
                }
                Err(e) => send_error(req, 500, &e.to_string()),
            }
        })?;
    }

    // POST /devices/0/measurement-mode/restore — roll back PEQ state to the
    // snapshot taken by the last POST /measurement-mode.
    {
        let st = state.clone();
        let audio = audio_state.clone();
        let nvs = nvs.clone();
        server.fn_handler(
            "/devices/0/measurement-mode/restore",
            Method::Post,
            move |req| {
                let req = match reject_hid_during_playback(req, &audio)? {
                    Some(req) => req,
                    None => return Ok(()),
                };
                let dev = match get_device(&st) {
                    Some(d) => d,
                    None => return send_json(req, 503, r#"{"error":"no device"}"#),
                };
                let _op = dev.begin_operation();
                match dev.restore_measurement_mode() {
                    Ok(()) => {
                        dev.save_peq_shadow_to_nvs(&nvs);
                        send_json(req, 200, r#"{"status":"measurement_mode_restored"}"#)
                    }
                    Err(e) => send_error(req, 500, &e.to_string()),
                }
            },
        )?;
    }

    // POST /devices/0/peq/reset
    {
        let st = state.clone();
        let audio = audio_state.clone();
        let nvs = nvs.clone();
        let beq = beq_state.clone();
        server.fn_handler("/devices/0/peq/reset", Method::Post, move |req| {
            let req = match reject_hid_during_playback(req, &audio)? {
                Some(req) => req,
                None => return Ok(()),
            };
            let dev = match get_device(&st) {
                Some(d) => d,
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
            };
            let _op = dev.begin_operation();
            match dev.reset_all_peq_to_defaults() {
                Ok(touched) => {
                    dev.save_peq_shadow_to_nvs(&nvs);
                    if let Some(ref beq_state) = beq {
                        if let Ok(master) = dev.get_master_status() {
                            sync_ezbeq_slot_after_peq_reset(
                                &dev,
                                beq_state.clone(),
                                master.preset as usize,
                            );
                        }
                    }
                    let body = serde_json::to_string(&json!({
                        "status": "peq_reset",
                        "bands_reset": touched,
                        "default_frequencies_hz": dev.default_peq_frequencies(),
                    }))
                    .unwrap_or_default();
                    send_json(req, 200, &body)
                }
                Err(e) if e.to_string().contains("unsupported") => {
                    send_error(req, 501, "config read unsupported")
                }
                Err(e) => send_error(req, 500, &e.to_string()),
            }
        })?;
    }

    // GET /devices/0/peq/default-frequencies
    {
        let st = state.clone();
        server.fn_handler(
            "/devices/0/peq/default-frequencies",
            Method::Get,
            move |req| {
                let dev = match get_device(&st) {
                    Some(d) => d,
                    None => return send_json(req, 503, r#"{"error":"no device"}"#),
                };
                let body = serde_json::to_string(&json!({
                    "frequencies_hz": dev.default_peq_frequencies(),
                }))
                .unwrap_or_default();
                send_json(req, 200, &body)
            },
        )?;
    }

    // GET /devices/0/bm
    {
        let st = state.clone();
        let audio = audio_state.clone();
        server.fn_handler("/devices/0/bm", Method::Get, move |req| {
            let req = match reject_hid_during_playback(req, &audio)? {
                Some(req) => req,
                None => return Ok(()),
            };
            let dev = match get_device(&st) {
                Some(d) => d,
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
            };
            if !dev.supports_bass_management() {
                return send_error(req, 501, "device does not support bass management");
            }
            let _op = dev.begin_operation();
            match dev.read_bass_management_state() {
                Ok(state) => {
                    let body = serde_json::to_string(&state).unwrap_or_default();
                    send_json(req, 200, &body)
                }
                Err(e) => send_error(req, 500, &e.to_string()),
            }
        })?;
    }

    // POST /devices/0/bm/channel/{ch}
    {
        let st = state.clone();
        let audio = audio_state.clone();
        server.fn_handler("/devices/0/bm/channel/*", Method::Post, move |mut req| {
            req = match reject_hid_during_playback(req, &audio)? {
                Some(req) => req,
                None => return Ok(()),
            };
            let dev = match get_device(&st) {
                Some(d) => d,
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
            };
            if !dev.supports_bass_management() {
                return send_error(req, 501, "device does not support bass management");
            }
            let uri = req.uri().to_string();
            let Some(ch) = parse_bm_channel_path(&uri) else {
                return send_error(req, 400, "invalid bm channel path");
            };
            let body = match read_request_body(&mut req, 2048) {
                Ok(b) => b,
                Err(msg) => return send_error(req, 413, msg),
            };
            let patch = match serde_json::from_slice::<BmChannelPatch>(&body) {
                Ok(v) => v,
                Err(_) => return send_error(req, 400, "invalid JSON"),
            };
            let _op = dev.begin_operation();
            match dev
                .set_bm_channel(ch, patch)
                .and_then(|()| dev.read_bm_channel(ch))
            {
                Ok(channel) => {
                    let body = serde_json::to_string(&channel).unwrap_or_default();
                    send_json(req, 200, &body)
                }
                Err(e) => send_error(req, 500, &e.to_string()),
            }
        })?;
    }

    // POST /devices/0/bm/mixer/{row}/{col}
    {
        let st = state.clone();
        let audio = audio_state.clone();
        server.fn_handler("/devices/0/bm/mixer/*", Method::Post, move |mut req| {
            req = match reject_hid_during_playback(req, &audio)? {
                Some(req) => req,
                None => return Ok(()),
            };
            let dev = match get_device(&st) {
                Some(d) => d,
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
            };
            if !dev.supports_bass_management() {
                return send_error(req, 501, "device does not support bass management");
            }
            let uri = req.uri().to_string();
            let Some((row, col)) = parse_bm_mixer_path(&uri) else {
                return send_error(req, 400, "invalid bm mixer path");
            };
            let body = match read_request_body(&mut req, 2048) {
                Ok(b) => b,
                Err(msg) => return send_error(req, 413, msg),
            };
            let patch = match serde_json::from_slice::<BmMixerCellPatch>(&body) {
                Ok(v) => v,
                Err(_) => return send_error(req, 400, "invalid JSON"),
            };
            let _op = dev.begin_operation();
            match dev
                .set_bm_mixer_cell(row, col, patch)
                .and_then(|()| dev.read_bm_mixer_cell(row, col))
            {
                Ok(cell) => {
                    let body = serde_json::to_string(&cell).unwrap_or_default();
                    send_json(req, 200, &body)
                }
                Err(e) => send_error(req, 500, &e.to_string()),
            }
        })?;
    }

    Ok(())
}

fn register_device_api_fallbacks(
    server: &mut EspHttpServer<'static>,
) -> Result<(), esp_idf_svc::sys::EspError> {
    for method in [Method::Post, Method::Put, Method::Patch, Method::Delete] {
        server.fn_handler("/devices/*", method, move |req| {
            let status = match parse_device_index(req.uri()) {
                Some(0) => 405,
                Some(_) => 404,
                None => 404,
            };
            let message = if status == 405 {
                "method not allowed"
            } else {
                "not found"
            };
            send_error(req, status, message)
        })?;
    }
    Ok(())
}

fn sync_ezbeq_slot_after_peq_reset(
    dev: &DeviceState,
    beq_state: crate::ezbeq::SharedEzBeqState,
    preset_idx: usize,
) {
    if preset_idx >= 4 {
        return;
    }

    let mut guard = beq_state.lock().unwrap_or_else(|e| e.into_inner());
    for (idx, slot) in guard.slots.iter_mut().enumerate() {
        slot.active = idx == preset_idx;
    }

    let slot = &mut guard.slots[preset_idx];
    slot.last = "Empty".into();
    slot.author = None;
    for gain in &mut slot.gains {
        gain.value = 0.0;
    }
    for mute in &mut slot.mutes {
        mute.value = false;
    }

    #[cfg(esp_idf_httpd_ws_support)]
    let ws_update = guard
        .ws_sender
        .as_ref()
        .cloned()
        .map(|sender| (sender, crate::ezbeq::device_state_json(dev, &guard)));
    drop(guard);

    #[cfg(esp_idf_httpd_ws_support)]
    if let Some((sender, json)) = ws_update {
        crate::ezbeq::broadcast_state(beq_state, sender, json);
    }
}

// -- Helpers --

/// Extract the current device from shared state.
pub fn get_device(state: &SharedDeviceState) -> Option<Arc<DeviceState>> {
    state.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn wait_for_rebooted_device(
    state: &SharedDeviceState,
    previous: &Arc<DeviceState>,
    timeout: Duration,
) -> Result<(), String> {
    const POLL_INTERVAL: Duration = Duration::from_millis(100);

    let started = Instant::now();
    let deadline = Instant::now() + timeout;
    let expected_hw_id = previous.hw_id().hw_id;
    let expected_dsp = previous.dsp_version();
    let expected_serial = previous.serial();

    loop {
        if let Some(current) = get_device(state) {
            let same_identity = current.hw_id().hw_id == expected_hw_id
                && current.dsp_version() == expected_dsp
                && (expected_serial == 0 || current.serial() == expected_serial);

            if same_identity && !Arc::ptr_eq(&current, previous) {
                info!(
                    "HTTP reboot wait: device recovered after {:?}",
                    started.elapsed()
                );
                return Ok(());
            }
        }

        if Instant::now() >= deadline {
            warn!(
                "HTTP reboot wait: timed out after {:?} waiting for hw_id={} dsp={} serial={}",
                started.elapsed(),
                expected_hw_id,
                expected_dsp,
                expected_serial
            );
            return Err("device did not recover after reboot".into());
        }

        std::thread::sleep(POLL_INTERVAL);
    }
}

fn parse_bm_channel_path(uri: &str) -> Option<u8> {
    let parts = path_segments(uri);
    match parts.as_slice() {
        ["devices", "0", "bm", "channel", ch] => ch.parse().ok(),
        _ => None,
    }
}

fn parse_bm_mixer_path(uri: &str) -> Option<(u8, u8)> {
    let parts = path_segments(uri);
    match parts.as_slice() {
        ["devices", "0", "bm", "mixer", row, col] => Some((row.parse().ok()?, col.parse().ok()?)),
        _ => None,
    }
}

fn parse_device_index(uri: &str) -> Option<usize> {
    let parts = path_segments(uri);
    match parts.as_slice() {
        ["devices", idx, ..] => idx.parse().ok(),
        _ => None,
    }
}

fn path_segments(uri: &str) -> Vec<&str> {
    uri.split('?')
        .next()
        .unwrap_or(uri)
        .trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect()
}

fn parse_source_path(uri: &str) -> Option<String> {
    let parts = path_segments(uri);
    match parts.as_slice() {
        ["devices", "0", "source", source] => Some((*source).to_string()),
        _ => None,
    }
}

fn parse_play_path(uri: &str) -> Option<usize> {
    let parts = path_segments(uri);
    match parts.as_slice() {
        ["devices", idx, "play"] => idx.parse().ok(),
        _ => None,
    }
}

fn parse_volume_path(uri: &str) -> Option<String> {
    let parts = path_segments(uri);
    match parts.as_slice() {
        ["devices", "0", "volume", direction] => Some((*direction).to_string()),
        _ => None,
    }
}

fn parse_query_param(uri: &str, key: &str) -> Option<String> {
    uri.split_once('?').and_then(|(_, query)| {
        query.split('&').find_map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            (name == key).then(|| urldecode(value))
        })
    })
}

fn config_json(dev: &DeviceState, config: DeviceConfig) -> String {
    let Some(table) = dev.profile().dsp_table else {
        return "{}".into();
    };

    let inputs = config
        .inputs
        .into_iter()
        .map(|input| {
            let mut input_obj = serde_json::Map::new();
            input_obj.insert("index".into(), json!(input.index));
            input_obj.insert(
                "peq".into(),
                json!(input
                    .peq
                    .into_iter()
                    .map(|peq| json!({
                        "index": peq.index,
                        "coeff": {
                            "b0": peq.coeff.b0,
                            "b1": peq.coeff.b1,
                            "b2": peq.coeff.b2,
                            "a1": peq.coeff.a1,
                            "a2": peq.coeff.a2,
                        },
                        "bypass": peq.bypass,
                    }))
                    .collect::<Vec<_>>()),
            );
            input_obj.insert(
                "gain".into(),
                input.gain.map_or(serde_json::Value::Null, |v| json!(v)),
            );
            input_obj.insert(
                "mute".into(),
                input.mute.map_or(serde_json::Value::Null, |v| json!(v)),
            );
            input_obj.insert(
                "routing".into(),
                json!(input
                    .routing
                    .into_iter()
                    .map(|route| {
                        let mut route_obj = serde_json::Map::new();
                        route_obj.insert("output".into(), json!(route.output));
                        route_obj.insert("enabled".into(), json!(route.enabled));
                        route_obj.insert("gain_db".into(), json!(route.gain_db));
                        if let Some(inverted) = route.polarity_inverted {
                            route_obj.insert("polarity_inverted".into(), json!(inverted));
                        }
                        serde_json::Value::Object(route_obj)
                    })
                    .collect::<Vec<_>>()),
            );
            serde_json::Value::Object(input_obj)
        })
        .collect::<Vec<_>>();

    let outputs = config
        .outputs
        .into_iter()
        .enumerate()
        .map(|(idx, output)| {
            let invert = table
                .outputs
                .get(idx)
                .and_then(|ch| ch.polarity)
                .and_then(|addr| dev.read_channel_invert(addr).ok());

            let mut output_obj = serde_json::Map::new();
            output_obj.insert("index".into(), json!(output.index));
            output_obj.insert(
                "peq".into(),
                json!(output
                    .peq
                    .into_iter()
                    .map(|peq| json!({
                        "index": peq.index,
                        "coeff": {
                            "b0": peq.coeff.b0,
                            "b1": peq.coeff.b1,
                            "b2": peq.coeff.b2,
                            "a1": peq.coeff.a1,
                            "a2": peq.coeff.a2,
                        },
                        "bypass": peq.bypass,
                    }))
                    .collect::<Vec<_>>()),
            );
            output_obj.insert("gain".into(), json!(output.gain));
            output_obj.insert("mute".into(), json!(output.mute));
            output_obj.insert(
                "delay".into(),
                json!(std::time::Duration::from_secs_f32(output.delay / 1000.0)),
            );
            output_obj.insert(
                "invert".into(),
                invert.map_or(serde_json::Value::Null, |v| json!(v)),
            );
            if !output.crossover.is_empty() {
                output_obj.insert(
                    "crossover".into(),
                    json!(output
                        .crossover
                        .into_iter()
                        .map(|group| json!({
                            "index": group.index,
                            "filters": group
                                .filters
                                .into_iter()
                                .map(|filter| json!({
                                    "index": filter.index,
                                    "coeff": {
                                        "b0": filter.coeff.b0,
                                        "b1": filter.coeff.b1,
                                        "b2": filter.coeff.b2,
                                        "a1": filter.coeff.a1,
                                        "a2": filter.coeff.a2,
                                    }
                                }))
                                .collect::<Vec<_>>(),
                            "bypass": group.bypass,
                        }))
                        .collect::<Vec<_>>()),
                );
            }
            if let Some(fir) = output.fir {
                output_obj.insert(
                    "fir".into(),
                    json!({
                        "bypass": fir.bypass,
                        "active_taps": fir.active_taps,
                    }),
                );
            }
            if let Some(compressor) = output.compressor {
                output_obj.insert(
                    "compressor".into(),
                    json!({
                        "bypass": compressor.bypass,
                        "threshold": compressor.threshold,
                        "gain": compressor.gain,
                        "ratio": compressor.ratio,
                        "knee": compressor.knee,
                        "attack": compressor.attack,
                        "release": compressor.release,
                    }),
                );
            }
            serde_json::Value::Object(output_obj)
        })
        .collect::<Vec<_>>();

    let master_status = json!({
        "master_status": {
            "preset": config.master_status.preset,
            "source": config.master_status.source,
            "volume": config.master_status.volume,
            "mute": config.master_status.mute,
            "dirac": config.master_status.dirac,
        },
    });

    let mut body = String::with_capacity(8192);
    body.push('{');
    body.push_str("\"master_status\":");
    body.push_str(
        &serde_json::to_string(&master_status["master_status"]).unwrap_or_else(|_| "{}".into()),
    );
    body.push_str(",\"inputs\":[");
    for (idx, input) in inputs.iter().enumerate() {
        if idx > 0 {
            body.push(',');
        }
        body.push_str(&serde_json::to_string(input).unwrap_or_else(|_| "null".into()));
        if idx + 1 < inputs.len() {
            // Cross a FreeRTOS tick so the HTTP worker does not starve IDLE
            // while formatting large config responses.
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    body.push_str("],\"outputs\":[");
    for (idx, output) in outputs.iter().enumerate() {
        if idx > 0 {
            body.push(',');
        }
        body.push_str(&serde_json::to_string(output).unwrap_or_else(|_| "null".into()));
        if idx + 1 < outputs.len() {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    body.push_str("]}");
    body
}

fn openapi_json() -> String {
    serde_json::to_string_pretty(&json!({
        "openapi": "3.0.3",
        "info": {
            "title": "minidsp-open ESP32 API",
            "description": "MiniDSP controller HTTP API served by the ESP32 firmware.",
            "version": "0.1.0",
            "license": { "name": "MIT" }
        },
        "paths": {
            "/devices": { "get": { "summary": "List connected devices" } },
            "/devices/0": {
                "get": { "summary": "Get device status" },
                "post": { "summary": "Set master status" }
            },
            "/devices/0/config": {
                "get": { "summary": "Read full device config" },
                "post": { "summary": "Write device config" }
            },
            "/devices/0/volume/{direction}": {
                "post": { "summary": "Volume up/down/toggle_mute" }
            },
            "/devices/0/source/{source}": {
                "post": { "summary": "Set input source" }
            },
            "/devices/0/preset/{preset}": {
                "post": { "summary": "Switch preset" }
            },
            "/devices/0/display": {
                "get": { "summary": "Get display settings" },
                "post": { "summary": "Set display brightness/idle time" }
            },
            "/devices/0/dre": {
                "get": { "summary": "Get DRE state" },
                "post": { "summary": "Set DRE enabled/disabled" }
            },
            "/devices/0/measurement-mode": {
                "post": { "summary": "Enter measurement mode" }
            },
            "/devices/0/measurement-mode/restore": {
                "post": { "summary": "Restore measurement mode" }
            },
            "/devices/0/tokens": {
                "get": { "summary": "Read modification tokens" }
            },
            "/devices/0/verify-program": {
                "get": { "summary": "Read the verify-program status byte and trailing EEPROM bytes at 0xFFA3" }
            },
            "/devices/0/preset/copy": {
                "post": { "summary": "Copy the active preset to the remaining preset slots" }
            },
            "/devices/0/preset/reset": {
                "post": { "summary": "Reset the active preset to the official default image" }
            },
            "/devices/0/preset/{preset}/reset": {
                "post": { "summary": "Reset a specific preset slot to the official default image" }
            },
            "/devices/0/preset/reset-all": {
                "post": { "summary": "Reset preset 1, copy it to all slots, and reboot the device" }
            },
            "/devices/0/reboot": {
                "post": { "summary": "Reboot the MiniDSP unit" }
            },
            "/devices/0/peq/reset": {
                "post": { "summary": "Reset every PEQ band to defaults" }
            },
            "/devices/0/peq/default-frequencies": {
                "get": { "summary": "Return the default PEQ frequency schedule" }
            },
            "/devices/0/bm": {
                "get": { "summary": "Read bass-management state" }
            },
            "/devices/0/bm/channel/{ch}": {
                "post": { "summary": "Patch one bass-management channel" }
            },
            "/devices/0/bm/mixer/{row}/{col}": {
                "post": { "summary": "Patch one bass-management mixer cell" }
            },
            "/devices/{id}/play": openapi_play_path(),
            "/sync": openapi_sync_path(),
            "/openapi.json": {
                "get": { "summary": "This OpenAPI specification" }
            },
            "/llms.txt": {
                "get": { "summary": "LLM discovery file" }
            }
        }
    }))
    .unwrap_or_else(|_| "{}".into())
}

fn openapi_play_path() -> serde_json::Value {
    json!({
        "post": {
            "summary": "Play the shared measurement sweep and optional acoustic markers",
            "operationId": "playDevice",
            "parameters": [
                { "name": "id", "in": "path", "required": true, "schema": { "type": "integer" } },
                { "name": "test", "in": "query", "required": false, "schema": { "type": "string", "enum": ["marker", "tail-marker"] } }
            ],
            "requestBody": {
                "content": {
                    "application/json": {
                        "schema": {
                            "type": "object",
                            "description": "Canonical playback request. The signal always includes the fixed 2 s low-level preamble. By default it also includes one 80 Hz marker before the sweep, a 50 ms gap, the sweep, another 50 ms gap, one 80 Hz marker after the sweep, and 2 s of post-silence. Setting markerCycles to 0 disables both marker sections and removes their gaps.",
                            "properties": {
                                "sweepDurationSec": { "type": "number", "default": 5.46, "exclusiveMinimum": 0.0 },
                                "sweepStartHz": { "type": "number", "default": 5.0, "exclusiveMinimum": 0.0 },
                                "sweepEndHz": { "type": "number", "default": 400.0, "exclusiveMinimum": 0.0 },
                                "sweepFadeSec": { "type": "number", "default": 0.02, "minimum": 0.0 },
                                "markerBefore": { "type": "boolean", "default": true },
                                "markerAfter": { "type": "boolean", "default": true },
                                "markerGapMs": { "type": "integer", "default": 50, "minimum": 0, "maximum": 2000 },
                                "markerFrequencyHz": { "type": "number", "default": 80.0, "exclusiveMinimum": 0.0 },
                                "markerCycles": { "type": "number", "default": 1.0, "minimum": 0.0 },
                                "decaySec": { "type": "number", "default": 2.0, "minimum": 0.0 },
                                "sampleRate": { "type": "integer", "default": 48000, "minimum": 1 },
                                "bitDepth": { "type": "string", "enum": ["16", "24"], "default": "16" }
                            },
                            "additionalProperties": false
                        }
                    }
                }
            },
            "responses": {
                "200": { "description": "Playback started; response includes startedAtUs, offsets, and source-switch metadata" },
                "400": { "description": "Invalid playback request" },
                "404": { "description": "Device not found" },
                "409": { "description": "Playback already in progress" },
                "413": { "description": "Playback request exceeds the ESP32 pre-generation buffer limit" }
            }
        }
    })
}

fn openapi_sync_path() -> serde_json::Value {
    json!({
        "post": {
            "summary": "Read the platform playback clock used by /play.startedAtUs",
            "operationId": "syncClock",
            "requestBody": {
                "content": {
                    "application/json": {
                        "schema": { "type": "object", "description": "Body: { clientTimeUs: i64 }" }
                    }
                }
            },
            "responses": {
                "200": { "description": "Echoed clientTimeUs plus serverTimeUs from the same clock basis as /play.startedAtUs" }
            }
        }
    })
}

pub fn send_json<C: esp_idf_svc::http::server::Connection>(
    req: esp_idf_svc::http::server::Request<C>,
    status: u16,
    body: &str,
) -> Result<(), C::Error> {
    let headers = [
        ("Server", "miniDSP-Open"),
        ("Content-Type", "application/json"),
        ("Access-Control-Allow-Origin", "*"),
    ];
    let mut resp = req.into_response(status, None, &headers)?;
    resp.write_all(body.as_bytes())?;
    Ok(())
}

pub(crate) fn reject_hid_during_playback<C: esp_idf_svc::http::server::Connection>(
    req: esp_idf_svc::http::server::Request<C>,
    audio: &usb_audio::SharedAudioState,
) -> Result<Option<esp_idf_svc::http::server::Request<C>>, C::Error> {
    if audio.is_playing() {
        send_error(req, 409, HID_BUSY_DURING_PLAYBACK)?;
        Ok(None)
    } else {
        Ok(Some(req))
    }
}

fn playback_busy_json() -> String {
    format!(r#"{{"error":"{HID_BUSY_DURING_PLAYBACK}"}}"#)
}

fn send_text<C: esp_idf_svc::http::server::Connection>(
    req: esp_idf_svc::http::server::Request<C>,
    text: &str,
) -> Result<(), C::Error> {
    let headers = [
        ("Server", "miniDSP-Open"),
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
        ("Server", "miniDSP-Open"),
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
        ("Server", "miniDSP-Open"),
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
        ("Server", "miniDSP-Open"),
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
    let headers = [
        ("Server", "miniDSP-Open"),
        ("Access-Control-Allow-Origin", "*"),
    ];
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

fn parse_biquad_coefficients(coeff: &serde_json::Value) -> Result<[f32; 5], &'static str> {
    let mut out = [0.0f32; 5];
    for (idx, key) in ["b0", "b1", "b2", "a1", "a2"].iter().enumerate() {
        let value = coeff
            .get(*key)
            .and_then(|v| v.as_f64())
            .ok_or("biquad coefficients must include numeric b0/b1/b2/a1/a2 fields")?;
        if !value.is_finite() {
            return Err("biquad coefficients must be finite");
        }
        out[idx] = value as f32;
    }
    Ok(out)
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
    let headers = [("Server", "miniDSP-Open"), ("Content-Type", "text/html")];
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

/// Switch the device source to USB for playback, returning the previous source
/// info needed to restore it after playback completes.
fn switch_to_usb_source(
    state: &SharedDeviceState,
    dev: &Arc<DeviceState>,
) -> Result<(Arc<DeviceState>, bool, Option<String>, Option<Source>), (u16, String)> {
    let usb_index = dev
        .profile()
        .sources
        .index_for_source(Source::Usb)
        .ok_or((500, "USB source missing from device profile".to_string()))?;

    let current_source = {
        let _op = dev.begin_operation();
        dev.current_source()
            .ok()
            .flatten()
            .ok_or((500, "current source unavailable".to_string()))?
    };

    if current_source == Source::Usb {
        return Ok((dev.clone(), false, None, None));
    }

    let (recovered, _) = set_source_index_with_recovery(
        state,
        dev,
        usb_index,
        Source::Usb,
        SOURCE_SWITCH_RECOVERY_TIMEOUT,
    )
    .map_err(|err| (500, err))?;

    Ok((
        recovered,
        true,
        Some(format_previous_source(current_source)),
        Some(current_source),
    ))
}

fn source_switch_error_is_recoverable(err: &UsbHidError) -> bool {
    match err {
        UsbHidError::Timeout | UsbHidError::Disconnected | UsbHidError::Transfer(_) => true,
        UsbHidError::Protocol(message) => message.starts_with("set source did not persist"),
        _ => false,
    }
}

fn same_device_identity(current: &Arc<DeviceState>, previous: &Arc<DeviceState>) -> bool {
    current.hw_id().hw_id == previous.hw_id().hw_id
        && current.dsp_version() == previous.dsp_version()
        && (previous.serial() == 0 || current.serial() == previous.serial())
}

fn wait_for_source_state(
    state: &SharedDeviceState,
    previous: &Arc<DeviceState>,
    target: Source,
    timeout: Duration,
) -> Result<(Arc<DeviceState>, StatusSummary), String> {
    const POLL_INTERVAL: Duration = Duration::from_millis(100);

    let deadline = Instant::now() + timeout;
    loop {
        if let Some(current) = get_device(state) {
            if same_device_identity(&current, previous) {
                let status = {
                    let _op = current.begin_operation();
                    match current.current_source() {
                        Ok(Some(source)) if source == target => current.get_status_summary().ok(),
                        _ => None,
                    }
                };
                if let Some(status) = status {
                    return Ok((current, status));
                }
            }
        }

        if Instant::now() >= deadline {
            return Err(format!(
                "source did not settle to {} after {:?}",
                target.pascal_case(),
                timeout
            ));
        }

        std::thread::sleep(POLL_INTERVAL);
    }
}

fn set_source_index_with_recovery(
    state: &SharedDeviceState,
    dev: &Arc<DeviceState>,
    target_index: u8,
    target: Source,
    timeout: Duration,
) -> Result<(Arc<DeviceState>, StatusSummary), String> {
    {
        let _op = dev.begin_operation();
        match dev.set_source(target_index) {
            Ok(()) => {}
            Err(err) if source_switch_error_is_recoverable(&err) => {
                warn!(
                    "source switch to {} dropped transport ({}); waiting for recovery",
                    target.pascal_case(),
                    err
                );
            }
            Err(err) => return Err(err.to_string()),
        }
    }

    wait_for_source_state(state, dev, target, timeout)
}

fn set_source_with_recovery(
    state: &SharedDeviceState,
    dev: &Arc<DeviceState>,
    target: Source,
    timeout: Duration,
) -> Result<(Arc<DeviceState>, StatusSummary), String> {
    let current = {
        let _op = dev.begin_operation();
        dev.current_source()
            .map_err(|err| err.to_string())?
            .ok_or_else(|| "current source unavailable".to_string())?
    };

    if current == target {
        let status = {
            let _op = dev.begin_operation();
            dev.get_status_summary().map_err(|err| err.to_string())?
        };
        return Ok((dev.clone(), status));
    }

    let target_index = dev
        .profile()
        .sources
        .index_for_source(target)
        .ok_or_else(|| "source not available on this device".to_string())?;
    set_source_index_with_recovery(state, dev, target_index, target, timeout)
}

fn ensure_audio_device_available(
    audio: &usb_audio::SharedAudioState,
    reason: &str,
) -> Result<Arc<usb_audio::Uac2Device>, String> {
    if let Some(device) = audio.current_device() {
        return Ok(device);
    }

    audio.resume_opening(reason);
    let _ = audio.reopen_last_known(reason);
    audio
        .wait_for_current_device(AUDIO_REOPEN_TIMEOUT)
        .ok_or_else(|| "USB audio interface not connected".to_string())
}

fn set_source_with_audio_coordination(
    state: &SharedDeviceState,
    audio: &usb_audio::SharedAudioState,
    dev: &Arc<DeviceState>,
    target: Source,
    timeout: Duration,
    context: &str,
) -> Result<(Arc<DeviceState>, StatusSummary), String> {
    let current = {
        let _op = dev.begin_operation();
        dev.current_source()
            .map_err(|err| err.to_string())?
            .ok_or_else(|| "current source unavailable".to_string())?
    };

    let switching_away_from_usb = current == Source::Usb && target != Source::Usb;
    if switching_away_from_usb {
        audio.pause_and_close_current(context);
        if !audio.wait_for_cleanup_complete(AUDIO_CLOSE_TIMEOUT) {
            let recovery_reason = format!("{context} audio close timeout");
            audio.resume_opening(&recovery_reason);
            let _ = audio.reopen_last_known(&recovery_reason);
            return Err(format!(
                "timed out waiting for USB audio teardown before switching source to {}",
                target.pascal_case()
            ));
        }
    }

    let result = set_source_with_recovery(state, dev, target, timeout);
    let result = match result {
        Ok(ok) if target == Source::Usb => {
            ensure_audio_device_available(audio, context).map_err(|err| {
                format!("source switched to Usb but audio interface did not recover: {err}")
            })?;
            Ok(ok)
        }
        Err(err) if switching_away_from_usb => {
            let recovery_reason = format!("{context} failed");
            audio.resume_opening(&recovery_reason);
            let _ = audio.reopen_last_known(&recovery_reason);
            Err(err)
        }
        other => other,
    };
    result
}

fn raw_source_from_payload(
    dev: &Arc<DeviceState>,
    payload: &[u8],
) -> Result<Option<Source>, UsbHidError> {
    if payload.first().copied() != Some(commands::CMD_SET_SOURCE) {
        return Ok(None);
    }

    let Some(&source_index) = payload.get(1) else {
        return Err(UsbHidError::Protocol(
            "set source: missing source index".to_string(),
        ));
    };

    dev.profile()
        .sources
        .source_for_index(source_index)
        .ok_or_else(|| {
            UsbHidError::Protocol(format!(
                "source {source_index} not available on this device"
            ))
        })
        .map(Some)
}

pub(crate) fn raw_bridge_command_with_audio_coordination(
    state: &SharedDeviceState,
    audio: &usb_audio::SharedAudioState,
    dev: &Arc<DeviceState>,
    payload: &[u8],
) -> Result<Vec<u8>, UsbHidError> {
    if let Some(target) = raw_source_from_payload(dev, payload)? {
        set_source_with_audio_coordination(
            state,
            audio,
            dev,
            target,
            SOURCE_SWITCH_RECOVERY_TIMEOUT,
            "raw source change",
        )
        .map_err(UsbHidError::Protocol)?;
        return Ok(payload[..2.min(payload.len())].to_vec());
    }

    let _op = dev.begin_operation();
    dev.raw_command(payload)
}

pub(crate) fn tcp_bridge_command_with_audio_coordination(
    state: &SharedDeviceState,
    audio: &usb_audio::SharedAudioState,
    dev: &Arc<DeviceState>,
    payload: &[u8],
) -> Result<Vec<u8>, UsbHidError> {
    let raw_resp = raw_bridge_command_with_audio_coordination(state, audio, dev, payload)?;
    Ok(
        minidsp_protocol::packet::encode_device_console_tcp_response(
            payload,
            &raw_resp,
            dev.dialect(),
        ),
    )
}

fn restore_source_after_playback(
    state: &SharedDeviceState,
    audio: &usb_audio::SharedAudioState,
    dev: &Arc<DeviceState>,
    source: Option<Source>,
    context: &str,
) -> Result<(), String> {
    let Some(source) = source else {
        return Ok(());
    };

    match set_source_with_audio_coordination(
        state,
        audio,
        dev,
        source,
        SOURCE_SWITCH_RECOVERY_TIMEOUT,
        context,
    ) {
        Ok(_) => {
            info!(
                "restored source after {context} to {}",
                source.pascal_case()
            );
            Ok(())
        }
        Err(err) => Err(format!("failed to restore source after {context}: {err}")),
    }
}

fn format_previous_source(source: Source) -> String {
    source.pascal_case().to_string()
}

fn playback_total_bytes(plan: &PlaybackPlan) -> Result<usize, &'static str> {
    plan.offsets()
        .total_samples
        .checked_mul(plan.layout().frame_bytes())
        .ok_or("playback request exceeds addressable buffer size")
}

fn validate_esp32_playback_size(plan: &PlaybackPlan) -> Result<usize, String> {
    let total_bytes = playback_total_bytes(plan).map_err(str::to_string)?;
    if total_bytes > MAX_PLAYBACK_BYTES {
        return Err(format!(
            "playback request exceeds ESP32 buffer limit ({total_bytes} bytes > {MAX_PLAYBACK_BYTES} bytes)"
        ));
    }
    Ok(total_bytes)
}

/// RAII guard that disables WiFi power save on creation and restores it on drop.
/// Ensures PS is always restored even on early returns or panics.
struct WifiPsGuard;

impl WifiPsGuard {
    fn disable_ps() -> Self {
        unsafe {
            esp_idf_sys::esp_wifi_set_ps(esp_idf_sys::wifi_ps_type_t_WIFI_PS_NONE);
        }
        Self
    }
}

impl Drop for WifiPsGuard {
    fn drop(&mut self) {
        unsafe {
            esp_idf_sys::esp_wifi_set_ps(esp_idf_sys::wifi_ps_type_t_WIFI_PS_MIN_MODEM);
        }
    }
}

fn copy_thread_spawn_configuration(conf: &ThreadSpawnConfiguration) -> ThreadSpawnConfiguration {
    ThreadSpawnConfiguration {
        name: conf.name,
        stack_size: conf.stack_size,
        priority: conf.priority,
        inherit: conf.inherit,
        pin_to_core: conf.pin_to_core,
        stack_alloc_caps: conf.stack_alloc_caps,
    }
}

fn spawn_playback_thread(
    state: SharedDeviceState,
    dev: Arc<DeviceState>,
    audio_state: usb_audio::SharedAudioState,
    audio_device: Arc<usb_audio::Uac2Device>,
    plan: Box<PlaybackPlan>,
    total_bytes: usize,
    source_changed: bool,
    previous_source: Option<String>,
    restore_source: Option<Source>,
    started_tx: mpsc::SyncSender<Result<PlayResponse, String>>,
) -> Result<(), String> {
    let previous_conf = ThreadSpawnConfiguration::get();
    let mut playback_conf = previous_conf
        .as_ref()
        .map(copy_thread_spawn_configuration)
        .unwrap_or_default();
    playback_conf.name = Some(c"playback");
    playback_conf.stack_size = PLAYBACK_THREAD_STACK_BYTES;
    playback_conf.stack_alloc_caps = MallocCap::Spiram | MallocCap::Cap8bit;
    playback_conf
        .set()
        .map_err(|err| format!("set playback thread config: {err}"))?;

    let spawn_result = std::thread::Builder::new()
        .name("playback".into())
        .stack_size(PLAYBACK_THREAD_STACK_BYTES)
        .spawn(move || {
            run_playback_thread(
                state,
                dev,
                audio_state,
                audio_device,
                plan,
                total_bytes,
                source_changed,
                previous_source,
                restore_source,
                started_tx,
            );
        });

    let restore_result = previous_conf
        .as_ref()
        .map(ThreadSpawnConfiguration::set)
        .unwrap_or_else(|| ThreadSpawnConfiguration::default().set());
    if let Err(err) = restore_result {
        warn!("failed to restore pthread config after playback spawn: {err}");
    }

    spawn_result.map(|_| ()).map_err(|err| err.to_string())
}

fn run_playback_thread(
    state: SharedDeviceState,
    dev: Arc<DeviceState>,
    audio_state: usb_audio::SharedAudioState,
    audio_device: Arc<usb_audio::Uac2Device>,
    plan: Box<PlaybackPlan>,
    total_bytes: usize,
    source_changed: bool,
    previous_source: Option<String>,
    restore_source: Option<Source>,
    started_tx: mpsc::SyncSender<Result<PlayResponse, String>>,
) {
    // Disable WiFi power save during playback — beacon wakeups cause
    // ~1-2 ms CPU stalls that can disrupt USB isochronous timing.
    let _wifi_ps = WifiPsGuard::disable_ps();

    // Pre-generate the entire sweep into memory before starting the USB
    // audio stream. ESP32 float math (sin/cos/ln) is too slow to keep up
    // with real-time 48 kHz playback, so we generate everything upfront
    // into PSRAM (~1.8 MB for a 9.5 s sweep) and then just blast bytes
    // to the ring buffer during playback.
    info!(
        "pre-generating {} samples ({} bytes) at {} Hz {}-bit",
        plan.offsets().total_samples,
        total_bytes,
        plan.request.sample_rate,
        plan.request.bit_depth.bits()
    );
    let gen_started_us = usb_audio::server_time_us();
    let mut pcm_data = vec![0u8; total_bytes];
    let mut generator = Box::new(plan.generator());
    let mut offset = 0;
    // Generate in 10ms slices so fill_pcm_bytes returns after each chunk
    // instead of consuming the entire remaining buffer in one call.
    // Without this, IDLE starves for the full pre-gen duration (~11s),
    // triggering the task watchdog. yield_now() only yields to equal/
    // higher priority tasks so IDLE never runs — sleep(10ms) crosses a
    // FreeRTOS tick boundary and calls vTaskDelay, letting IDLE reset
    // the watchdog.
    const PREGEN_YIELD_INTERVAL_CHUNKS: u32 = 10;
    const PREGEN_YIELD_DURATION_MS: u64 = 10;
    let gen_chunk_bytes = (plan.request.sample_rate as usize / 100) * plan.layout().frame_bytes();
    let mut yield_counter = 0u32;
    while !generator.is_finished() && offset < pcm_data.len() {
        let end = (offset + gen_chunk_bytes).min(pcm_data.len());
        let written = generator.fill_pcm_bytes(&mut pcm_data[offset..end]);
        if written == 0 {
            break;
        }
        offset += written;
        yield_counter += 1;
        if yield_counter % PREGEN_YIELD_INTERVAL_CHUNKS == 0 {
            // 10ms minimum to cross one FreeRTOS tick boundary and
            // actually call vTaskDelay — sub-tick sleeps busy-wait.
            std::thread::sleep(std::time::Duration::from_millis(PREGEN_YIELD_DURATION_MS));
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
    let start_result =
        start_playback_pregenerated(&audio_device, &plan, pcm_data).map(|mut start| {
            start.response.source_changed = source_changed;
            start.response.previous_source = previous_source.clone();
            start
        });
    let allow_source_restore;

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
            // Copy each chunk from PSRAM to a fixed internal-SRAM buffer
            // before writing to the ring buffer. xRingbufferSend holds a
            // critical section during memcpy — PSRAM SPI latency inside
            // that section can block the USB host task. This buffer stays
            // off the playback thread stack so the thread can run with a
            // smaller stack budget on the constrained ESP32 pthread pool.
            // Keep the shared SRAM scratch buffer larger than the current
            // streaming chunk so future 24-bit or higher-rate paths still fit.
            let mut sram_buf = PLAYBACK_STREAM_BUF
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let mut pos = start.first_write_bytes;
            let mut stream_failed = false;
            while pos < pcm_data.len() {
                let end = (pos + chunk_bytes).min(pcm_data.len());
                let len = end - pos;
                sram_buf[..len].copy_from_slice(&pcm_data[pos..end]);
                let write_started_us = usb_audio::server_time_us();
                if let Err(err) = audio_device.write(&sram_buf[..len], 250) {
                    warn!("playback stream failed: {err}");
                    stream_failed = true;
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
                if write_latencies_us.len() % PLAYBACK_PROGRESS_LOG_EVERY_WRITES == 0 {
                    info!(
                        "playback progress: {} / {} bytes, write={} ms",
                        total_bytes_written,
                        pcm_data.len(),
                        write_time_us / 1000
                    );
                }
            }

            let stream_connected = audio_device.is_connected();
            allow_source_restore = !stream_failed && stream_connected;
            log_audio_feedback_if_connected(&audio_device, "playback");
            let stop_started_us = usb_audio::server_time_us();
            stop_audio_stream_if_connected(&audio_device, "playback");
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
            allow_source_restore = false;
            let _ = started_tx.send(Err(err));
        }
    }

    if allow_source_restore {
        if let Err(err) =
            restore_source_after_playback(&state, &audio_state, &dev, restore_source, "playback")
        {
            warn!("{err}");
        }
    } else if restore_source.is_some() {
        warn!("skipping source restore after interrupted or disconnected playback");
    }

    audio_state.finish_playback();
    // _wifi_ps guard restores WiFi PS on drop
}

fn log_audio_feedback_if_connected(audio_device: &usb_audio::Uac2Device, context: &str) {
    if !audio_device.is_connected() {
        warn!("{context}: skipping USB feedback read after disconnect");
        return;
    }

    let fb_raw = audio_device.feedback_raw();
    let fb_hz = fb_raw >> 16;
    let fb_frac = ((fb_raw & 0xFFFF) as u64 * 10000) / 65536;
    info!("USB feedback at end of {context}: raw=0x{fb_raw:08x} ({fb_hz}.{fb_frac:04} Hz)");
}

fn stop_audio_stream_if_connected(audio_device: &usb_audio::Uac2Device, context: &str) {
    if !audio_device.is_connected() {
        warn!("{context}: skipping UAC2 stop after disconnect");
        return;
    }

    if let Err(err) = audio_device.stop_stream() {
        warn!("UAC2 stop failed after {context}: {err}");
    }
}

/// Inline sine test: generates sin(2πft) on the stack and writes to USB
/// in a tight loop, identical to the standalone driver test harness.
/// No PSRAM, no PlaybackGenerator, no pre-generation — isolates whether
/// blips come from the data pipeline or the system environment.
#[allow(dead_code)]
fn run_inline_sine_test(
    state: SharedDeviceState,
    dev: Arc<DeviceState>,
    audio_state: usb_audio::SharedAudioState,
    audio_device: Arc<usb_audio::Uac2Device>,
    source_changed: bool,
    previous_source: Option<String>,
    restore_source: Option<Source>,
    started_tx: mpsc::SyncSender<Result<PlayResponse, String>>,
) {
    const SAMPLE_RATE: u32 = 48_000;
    const CHUNK_MS: u32 = PLAYBACK_STREAM_TARGET_CHUNK_MS as u32;
    const FRAMES_PER_CHUNK: usize = 96; // 2 ms at 48 kHz, matching basic_playback.
    const FRAME_BYTES: usize = 4; // 2ch × 16-bit
    const CHUNK_BYTES: usize = FRAMES_PER_CHUNK * FRAME_BYTES; // 384
    const FREQUENCY_HZ: f32 = 50.0;
    const AMPLITUDE: f32 = 0.5;
    const DURATION_SEC: u32 = 5;
    const TOTAL_CHUNKS: u32 = DURATION_SEC * (1000 / CHUNK_MS);
    const PREFILL_CHUNKS: u32 = 50 / CHUNK_MS; // 50 ms
    const PREFILL_WRITE_TIMEOUT_MS: u32 = 500;
    const STREAM_WRITE_TIMEOUT_MS: u32 = 250;
    const STREAM_WRITE_MAX_ATTEMPTS: usize = 4;
    const STREAM_WRITE_RETRY_DELAY_MS: u64 = 10;

    let _wifi_ps = WifiPsGuard::disable_ps();

    info!(
        "inline sine test: {} Hz, {} sec, {} Hz/{}-bit stereo",
        FREQUENCY_HZ, DURATION_SEC, SAMPLE_RATE, 16
    );

    usb_audio::reset_tx_need_data_count();
    if let Err(err) = audio_device.start_stream(SAMPLE_RATE, BitDepth::Pcm16) {
        let _ = started_tx.send(Err(err.to_string()));
        audio_state.finish_playback();
        return;
    }

    let phase_inc = 2.0 * core::f32::consts::PI * FREQUENCY_HZ / SAMPLE_RATE as f32;
    let mut phase: f32 = 0.0;
    let mut buf = [0u8; CHUNK_BYTES];

    // Pre-fill 50ms into the ring buffer, matching standalone
    for _ in 0..PREFILL_CHUNKS {
        fill_sine_16bit_stereo(&mut buf, &mut phase, phase_inc, AMPLITUDE);
        if let Err(err) = audio_device.write(&buf, PREFILL_WRITE_TIMEOUT_MS) {
            stop_audio_stream_if_connected(&audio_device, "inline sine prefill");
            let _ = started_tx.send(Err(format!("prefill write: {err}")));
            audio_state.finish_playback();
            return;
        }
    }

    // Wait for stream to actually start (first SOF)
    let mut started_at_us = if audio_device.is_connected() {
        audio_device.start_time_us()
    } else {
        0
    };
    for _ in 0..20 {
        if started_at_us != 0 || !audio_device.is_connected() {
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
        tail_tone_burst_offset_samples: None,
        total_samples: FRAMES_PER_CHUNK * (TOTAL_CHUNKS + PREFILL_CHUNKS) as usize,
        sample_rate: SAMPLE_RATE,
        source_changed,
        previous_source: previous_source.clone(),
    }));

    // Tight write loop — generate inline, write immediately
    let stream_start_us = usb_audio::server_time_us();
    let mut writes: u32 = 0;
    let mut stream_failed = false;
    for _ in 0..TOTAL_CHUNKS {
        fill_sine_16bit_stereo(&mut buf, &mut phase, phase_inc, AMPLITUDE);
        match write_audio_chunk_with_retry(
            &audio_device,
            &buf,
            STREAM_WRITE_TIMEOUT_MS,
            STREAM_WRITE_MAX_ATTEMPTS,
            STREAM_WRITE_RETRY_DELAY_MS,
            "inline sine",
        ) {
            Ok(()) => writes += 1,
            Err(err) => {
                warn!("inline sine write failed at chunk {writes}: {err}");
                stream_failed = true;
                break;
            }
        }
    }
    let stream_elapsed_ms = (usb_audio::server_time_us() - stream_start_us) / 1000;

    let stream_connected = audio_device.is_connected();
    let allow_source_restore = !stream_failed && stream_connected;
    log_audio_feedback_if_connected(&audio_device, "inline sine");
    info!(
        "inline sine test complete: {} writes in {} ms, tx_need_data={}",
        writes,
        stream_elapsed_ms,
        usb_audio::tx_need_data_count()
    );

    stop_audio_stream_if_connected(&audio_device, "inline sine");

    if allow_source_restore {
        if let Err(err) =
            restore_source_after_playback(&state, &audio_state, &dev, restore_source, "sine test")
        {
            warn!("{err}");
        }
    } else if restore_source.is_some() {
        warn!("skipping source restore after interrupted or disconnected sine test");
    }

    audio_state.finish_playback();
    // _wifi_ps guard restores WiFi PS on drop
}

/// Fill a buffer with 16-bit stereo sine samples (same algorithm as
/// the standalone driver test's `tone_gen_fill`).
#[allow(dead_code)]
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

#[allow(dead_code)]
fn write_audio_chunk_with_retry(
    audio_device: &usb_audio::Uac2Device,
    data: &[u8],
    timeout_ms: u32,
    max_attempts: usize,
    retry_delay_ms: u64,
    context: &str,
) -> Result<(), String> {
    let mut last_error = None;
    for attempt in 1..=max_attempts {
        match audio_device.write(data, timeout_ms) {
            Ok(()) => return Ok(()),
            Err(err) => {
                last_error = Some(err.to_string());
                if attempt == max_attempts {
                    break;
                }
                warn!("{context}: write attempt {attempt}/{max_attempts} failed: {err}; retrying");
                std::thread::sleep(Duration::from_millis(retry_delay_ms));
            }
        }
    }

    Err(last_error.unwrap_or_else(|| "unknown write failure".into()))
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
    const PREFILL_MS: usize = 150;

    usb_audio::reset_tx_need_data_count();
    audio_device
        .start_stream(plan.request.sample_rate, plan.request.bit_depth)
        .map_err(|e| e.to_string())?;

    // Stream in ~2 ms chunks (96 frames at 48 kHz), matching the cadence
    // used by the standalone UAC2 playback harness when it sounds clean.
    let frames_per_chunk = playback_stream_chunk_frames(plan.request.sample_rate);
    let chunk_bytes = frames_per_chunk * plan.layout().frame_bytes();

    // Pre-fill well past the ring buffer's half-full low-watermark so
    // playback starts with enough slack to absorb WiFi/USB jitter across
    // repeated runs. At 48 KiB this keeps the buffer comfortably above the
    // default threshold while still leaving headroom for immediate writes.
    let prefill_chunks = PREFILL_MS.div_ceil(PLAYBACK_STREAM_TARGET_CHUNK_MS).max(1);
    let prefill_end = (chunk_bytes * prefill_chunks).min(pcm_data.len());
    let mut prefill_pos = 0;
    let mut prefill_sram = [0u8; 2880];
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
            tail_tone_burst_offset_samples: plan.offsets().tail_tone_burst_offset_samples,
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

fn playback_stream_chunk_frames(sample_rate: u32) -> usize {
    ((sample_rate as usize) * PLAYBACK_STREAM_TARGET_CHUNK_MS / 1000).max(1)
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
    audio_state: usb_audio::SharedAudioState,
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
        let audio = audio_state.clone();
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
                    let (status, json) = if audio.is_playing() {
                        (409, playback_busy_json())
                    } else {
                        match get_device(&st) {
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
                        }
                    };
                    unsafe { ws_send_http_json(raw_req, status, &json) };
                    return Ok(());
                }

                // WebSocket upgrade — parse query params for poll interval
                let (poll, levels) = unsafe { ws_parse_query(raw_req) };
                let interval_ms: u64 = if levels {
                    250
                } else if poll {
                    1000
                } else {
                    0
                };

                // Stop any previous polling thread
                if let Ok(mut guard) = ws_ref.lock() {
                    if let Some(prev) = guard.take() {
                        prev.stop.store(true, Ordering::Relaxed);
                    }
                }

                if interval_ms > 0 {
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
                            let audio_state = audio.clone();
                            if let Err(e) = std::thread::Builder::new()
                                .name("ws_compat".into())
                                .stack_size(8192)
                                .spawn(move || {
                                    ws_poll_loop(sender, dev_state, audio_state, stop, interval_ms);
                                })
                            {
                                log::warn!("WebSocket compat: failed to spawn poll thread: {e}");
                            }
                        }
                        Err(e) => log::warn!("WebSocket compat: failed to create sender: {e:?}"),
                    }
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
            match ws_recv_frame(ws, "WebSocket compat", 512)? {
                Some((FrameType::Text(_), _)) | Some((FrameType::Binary(_), _)) => {
                    let json = if audio.is_playing() {
                        playback_busy_json()
                    } else {
                        match get_device(&st) {
                            Some(dev) => {
                                let _op = dev.begin_operation();
                                match dev.get_status_summary() {
                                    Ok(status) => status_json(&status),
                                    Err(_) => r#"{"error":"device read failed"}"#.to_string(),
                                }
                            }
                            None => r#"{"error":"no device"}"#.to_string(),
                        }
                    };
                    ws_send_logged(
                        ws,
                        "WebSocket compat",
                        FrameType::Text(false),
                        json.as_bytes(),
                    )?;
                }
                Some((FrameType::Close, _)) => {
                    ws_send_logged(ws, "WebSocket compat", FrameType::Close, &[])?;
                }
                Some((FrameType::Ping, _)) => {
                    ws_send_logged(ws, "WebSocket compat", FrameType::Pong, &[])?;
                }
                Some((FrameType::Pong, _))
                | Some((FrameType::SocketClose, _))
                | Some((FrameType::Continue(_), _))
                | None => {}
            }
            Ok(())
        })?;

        info!("WebSocket compat endpoint registered: /devices/0");
    }

    // -- Raw frame bridge: /devices/0/ws --
    {
        let st = state.clone();
        let audio = audio_state.clone();

        server.ws_handler("/devices/0/ws", None, move |ws| {
            // Raw frame bridge — NO status polling thread. Only responds to binary frames.
            if ws.is_new() {
                log::debug!("WS raw bridge: client connected (fd={})", ws.session());
                return Ok::<(), esp_idf_svc::sys::EspError>(());
            }

            if ws.is_closed() {
                log::debug!("WS raw bridge: client disconnected (fd={})", ws.session());
                return Ok(());
            }

            match ws_recv_frame(ws, "WS raw bridge", 512)? {
                Some((FrameType::Binary(_), frame)) => {
                    if audio.is_playing() {
                        log::warn!("WS raw bridge: rejecting command during playback");
                        let busy = minidsp_protocol::packet::encode_frame(&[
                            minidsp_protocol::commands::RESP_ERROR,
                        ]);
                        ws_send_logged(ws, "WS raw bridge", FrameType::Binary(false), &busy)?;
                        return Ok(());
                    }
                    // Raw frame bridge: decode framed packet, forward payload to device
                    if let Some(payload) = minidsp_protocol::packet::decode_frame(&frame) {
                        match get_device(&st) {
                            Some(dev) => match raw_bridge_command_with_audio_coordination(
                                &st, &audio, &dev, &payload,
                            ) {
                                Ok(resp) => {
                                    let frame = minidsp_protocol::packet::encode_frame(&resp);
                                    ws_send_logged(
                                        ws,
                                        "WS raw bridge",
                                        FrameType::Binary(false),
                                        &frame,
                                    )?;
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
                        log::warn!("WS raw bridge: invalid frame ({} bytes)", frame.len());
                    }
                }
                Some((FrameType::Text(_), frame)) => {
                    log::info!(
                        "WS raw bridge: ignoring unexpected text frame ({} bytes)",
                        frame.len()
                    );
                }
                Some((FrameType::Close, _)) => {
                    ws_send_logged(ws, "WS raw bridge", FrameType::Close, &[])?;
                }
                Some((FrameType::Ping, _)) => {
                    ws_send_logged(ws, "WS raw bridge", FrameType::Pong, &[])?;
                }
                Some((FrameType::Pong, _))
                | Some((FrameType::SocketClose, _))
                | Some((FrameType::Continue(_), _))
                | None => {}
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

    let status_text = match status {
        200 => b"200 OK\0".as_slice(),
        400 => b"400 Bad Request\0".as_slice(),
        404 => b"404 Not Found\0".as_slice(),
        409 => b"409 Conflict\0".as_slice(),
        500 => b"500 Internal Server Error\0".as_slice(),
        503 => b"503 Service Unavailable\0".as_slice(),
        _ if status >= 500 => b"500 Internal Server Error\0".as_slice(),
        _ if status >= 400 => b"400 Bad Request\0".as_slice(),
        _ => b"200 OK\0".as_slice(),
    };
    httpd_resp_set_status(raw_req, status_text.as_ptr());

    let ret = httpd_resp_send(raw_req, json.as_ptr(), json.len() as isize);
    if ret != ESP_OK {
        log::warn!("ws_send_http_json: httpd_resp_send failed ({ret})");
    }
}

#[cfg(esp_idf_httpd_ws_support)]
fn ws_recv_frame(
    ws: &mut esp_idf_svc::http::server::ws::EspHttpWsConnection,
    context: &str,
    max_len: usize,
) -> Result<Option<(esp_idf_svc::ws::FrameType, Vec<u8>)>, esp_idf_svc::sys::EspError> {
    use esp_idf_svc::ws::FrameType;

    let (frame_type, len) = match ws.recv(&mut []) {
        Ok(frame) => frame,
        Err(e) => {
            log::warn!("{context}: recv header failed: {e:?}");
            return Err(e);
        }
    };

    if len > max_len {
        log::warn!("{context}: frame too large ({len} bytes > {max_len})");
        let _ = ws.send(FrameType::Close, &[]);
        return Ok(None);
    }

    let mut payload = vec![0u8; len];
    if len == 0 {
        return Ok(Some((frame_type, payload)));
    }

    match ws.recv(&mut payload) {
        Ok((confirmed_type, confirmed_len)) => {
            payload.truncate(confirmed_len);
            Ok(Some((confirmed_type, payload)))
        }
        Err(e) => {
            log::warn!("{context}: recv payload failed: {e:?}");
            Err(e)
        }
    }
}

#[cfg(esp_idf_httpd_ws_support)]
fn ws_send_logged(
    ws: &mut esp_idf_svc::http::server::ws::EspHttpWsConnection,
    context: &str,
    frame_type: esp_idf_svc::ws::FrameType,
    payload: &[u8],
) -> Result<(), esp_idf_svc::sys::EspError> {
    match ws.send(frame_type, payload) {
        Ok(()) => Ok(()),
        Err(e) => {
            log::warn!("{context}: send failed: {e:?}");
            Err(e)
        }
    }
}

/// Background thread that pushes device status over WebSocket at a configurable interval.
#[cfg(esp_idf_httpd_ws_support)]
fn ws_poll_loop(
    mut sender: esp_idf_svc::http::server::ws::EspHttpWsDetachedSender,
    state: SharedDeviceState,
    audio_state: usb_audio::SharedAudioState,
    stop: Arc<std::sync::atomic::AtomicBool>,
    interval_ms: u64,
) {
    use esp_idf_svc::ws::FrameType;
    use std::sync::atomic::Ordering;

    log::info!("WebSocket poll thread started (interval={interval_ms}ms)");

    // Send initial status immediately so clients don't wait for first tick
    if audio_state.is_playing() {
        let json = playback_busy_json();
        if sender
            .send(FrameType::Text(false), json.as_bytes())
            .is_err()
        {
            return;
        }
    } else if let Some(dev) = get_device(&state) {
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
        let mut remaining_ms = interval_ms;
        while remaining_ms > 0 && !stop.load(Ordering::Relaxed) {
            let sleep_ms = remaining_ms.min(50);
            std::thread::sleep(std::time::Duration::from_millis(sleep_ms));
            remaining_ms = remaining_ms.saturating_sub(sleep_ms);
        }

        if stop.load(Ordering::Relaxed) {
            break;
        }

        let json = if audio_state.is_playing() {
            playback_busy_json()
        } else {
            match get_device(&state) {
                Some(dev) => {
                    let _op = dev.begin_operation();
                    match dev.get_status_summary() {
                        Ok(status) => status_json(&status),
                        Err(_) => continue,
                    }
                }
                None => r#"{"error":"no device"}"#.to_string(),
            }
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
    audio_state: usb_audio::SharedAudioState,
    beq_state: crate::ezbeq::SharedEzBeqState,
) -> Result<(), esp_idf_svc::sys::EspError> {
    use esp_idf_svc::ws::FrameType;
    use std::sync::atomic::{AtomicBool, Ordering};

    let active_ws: Arc<Mutex<Option<ActiveWsBeq>>> = Arc::new(Mutex::new(None));

    let st = state.clone();
    let audio = audio_state.clone();
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
            ws_send_logged(
                ws,
                "ezBEQ WS",
                FrameType::Text(false),
                catalogue_meta.as_bytes(),
            )?;

            // Send initial DeviceState
            if audio.is_playing() {
                log::info!("ezBEQ WS: deferring initial DeviceState while playback is active");
            } else if let Some(dev) = get_device(&st) {
                let _op = dev.begin_operation();
                let beq_guard = beq.lock().unwrap_or_else(|e| e.into_inner());
                let state_json = crate::ezbeq::device_state_json(&dev, &beq_guard);
                ws_send_logged(
                    ws,
                    "ezBEQ WS",
                    FrameType::Text(false),
                    state_json.as_bytes(),
                )?;
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
        match ws_recv_frame(ws, "ezBEQ WS", 512)? {
            Some((ft, frame)) => {
                let msg = std::str::from_utf8(&frame)
                    .unwrap_or("")
                    .trim_end_matches('\0')
                    .trim();
                log::info!("ezBEQ WS: ft={ft:?} len={} msg='{msg}'", frame.len());

                if msg == "load catalogue" {
                    // Browser JS shim handles catalogue loading from our Pages mirror.
                    // Send empty response as safety fallback (shim intercepts before this fires).
                    log::info!("load catalogue: handled by browser shim");
                    let empty = r#"{"message":"CatalogueEntries","data":[]}"#;
                    ws_send_logged(ws, "ezBEQ WS", FrameType::Text(false), empty.as_bytes())?;
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
                            let audio_state = audio.clone();
                            let beq_clone = beq.clone();
                            let ws_ref_clone = ws_ref.clone();
                            if let Err(e) = std::thread::Builder::new()
                                .name("beq_lvl".into())
                                .stack_size(8192)
                                .spawn(move || {
                                    push_levels(
                                        sender,
                                        dev_state,
                                        audio_state,
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
                    ws_send_logged(ws, "ezBEQ WS", FrameType::Close, &[])?;
                } else if matches!(ft, FrameType::Ping) {
                    ws_send_logged(ws, "ezBEQ WS", FrameType::Pong, &[])?;
                }
            }
            None => {}
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
    audio_state: usb_audio::SharedAudioState,
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

        if audio_state.is_playing() {
            continue;
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
        Some("txt") => "text/plain; charset=utf-8",
        Some("webmanifest") => "application/manifest+json",
        Some("xml") => "application/xml",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("ttf") => "font/ttf",
        Some("map") => "application/json",
        _ => "application/octet-stream",
    };
    let cache_control = if path.ends_with("index.html")
        || path.ends_with("ezbeq-shim.js")
        || path.ends_with("version.txt")
    {
        "no-store"
    } else {
        "public, max-age=3600"
    };
    let headers = [
        ("Server", "miniDSP-Open"),
        ("Content-Type", content_type),
        ("Access-Control-Allow-Origin", "*"),
        ("Cache-Control", cache_control),
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

fn proxy_catalogue_file<C: esp_idf_svc::http::server::Connection>(
    req: esp_idf_svc::http::server::Request<C>,
    nvs: &EspDefaultNvsPartition,
    relative_path: &str,
    content_type: &str,
) -> Result<(), C::Error> {
    let base_url = crate::updater::base_url(nvs);
    let url = format!("{base_url}/{relative_path}");
    let config = HttpClientConfiguration {
        buffer_size: Some(4096),
        timeout: Some(Duration::from_secs(120)),
        crt_bundle_attach: Some(esp_idf_svc::sys::esp_crt_bundle_attach),
        ..Default::default()
    };
    let mut conn = match EspHttpConnection::new(&config) {
        Ok(conn) => conn,
        Err(e) => {
            warn!("catalogue proxy: failed to create client for {url}: {e}");
            return send_error(req, 502, "catalogue proxy unavailable");
        }
    };
    if let Err(e) = conn.initiate_request(Method::Get, &url, &[]) {
        warn!("catalogue proxy: request start failed for {url}: {e}");
        return send_error(req, 502, "catalogue proxy request failed");
    }
    if let Err(e) = conn.initiate_response() {
        warn!("catalogue proxy: response start failed for {url}: {e}");
        return send_error(req, 502, "catalogue proxy response failed");
    }

    let status = conn.status();
    if status != 200 {
        warn!("catalogue proxy: upstream {url} returned HTTP {status}");
        return send_error(req, 502, &format!("catalogue upstream HTTP {status}"));
    }
    let content_length = conn
        .header("Content-Length")
        .and_then(|len| len.parse::<usize>().ok())
        .map(|len| len.to_string());
    let mut resp = if let Some(ref len) = content_length {
        let headers = [
            ("Server", "miniDSP-Open"),
            ("Content-Type", content_type),
            ("Access-Control-Allow-Origin", "*"),
            ("Cache-Control", "no-store"),
            ("Content-Length", len.as_str()),
        ];
        req.into_response(200, None, &headers)?
    } else {
        let headers = [
            ("Server", "miniDSP-Open"),
            ("Content-Type", content_type),
            ("Access-Control-Allow-Origin", "*"),
            ("Cache-Control", "no-store"),
        ];
        req.into_response(200, None, &headers)?
    };
    let mut buf = [0u8; 4096];
    let mut total_sent = 0usize;
    let expected_len = content_length
        .as_deref()
        .and_then(|len| len.parse::<usize>().ok());
    loop {
        let n = match conn.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                warn!("catalogue proxy: upstream read failed for {url}: {e}");
                break;
            }
        };
        total_sent = total_sent.saturating_add(n);
        if let Err(e) = resp.write_all(&buf[..n]) {
            warn!("catalogue proxy: downstream write failed for {url}: {e}");
            break;
        }
        if expected_len.is_some_and(|len| total_sent >= len) {
            break;
        }
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

#[cfg(test)]
mod tests {
    use super::{
        format_previous_source, openapi_json, parse_bm_channel_path, parse_bm_mixer_path,
        parse_device_index, parse_play_path, parse_query_param, parse_source_path,
        parse_volume_path, validate_esp32_playback_size,
    };
    use minidsp_protocol::playback::{PlayRequest, PlaybackPlan};
    use minidsp_protocol::source::Source;

    #[test]
    fn parses_bm_channel_path() {
        assert_eq!(parse_bm_channel_path("/devices/0/bm/channel/7"), Some(7));
        assert_eq!(
            parse_bm_channel_path("/devices/0/bm/channel/3?x=1"),
            Some(3)
        );
        assert_eq!(parse_bm_channel_path("/devices/0/bm/channel"), None);
    }

    #[test]
    fn parses_bm_mixer_path() {
        assert_eq!(parse_bm_mixer_path("/devices/0/bm/mixer/8/7"), Some((8, 7)));
        assert_eq!(
            parse_bm_mixer_path("/devices/0/bm/mixer/1/2?x=1"),
            Some((1, 2))
        );
        assert_eq!(parse_bm_mixer_path("/devices/0/bm/mixer/1"), None);
    }

    #[test]
    fn parses_device_index() {
        assert_eq!(parse_device_index("/devices/0"), Some(0));
        assert_eq!(parse_device_index("/devices/17/config?x=1"), Some(17));
        assert_eq!(parse_device_index("/devices"), None);
        assert_eq!(parse_device_index("/api/2/devices"), None);
    }

    #[test]
    fn parses_source_path() {
        assert_eq!(
            parse_source_path("/devices/0/source/Usb?x=1"),
            Some("Usb".into())
        );
        assert_eq!(
            parse_source_path("/devices/0/source/bluetooth"),
            Some("bluetooth".into())
        );
        assert_eq!(parse_source_path("/devices/0/source"), None);
    }

    #[test]
    fn parses_play_path() {
        assert_eq!(parse_play_path("/devices/0/play"), Some(0));
        assert_eq!(parse_play_path("/devices/17/play?test=marker"), Some(17));
        assert_eq!(parse_play_path("/devices/0/play/extra"), None);
    }

    #[test]
    fn parses_play_query_param() {
        assert_eq!(
            parse_query_param("/devices/0/play?test=tail-marker&x=1", "test"),
            Some("tail-marker".into())
        );
        assert_eq!(parse_query_param("/devices/0/play?x=1", "test"), None);
    }

    #[test]
    fn parses_volume_path() {
        assert_eq!(
            parse_volume_path("/devices/0/volume/toggle_mute?x=1"),
            Some("toggle_mute".into())
        );
        assert_eq!(parse_volume_path("/devices/0/volume/up"), Some("up".into()));
        assert_eq!(parse_volume_path("/devices/0/volume"), None);
    }

    #[test]
    fn openapi_lists_esp32_parity_routes() {
        let spec = openapi_json();
        assert!(spec.contains("/openapi.json"));
        assert!(spec.contains("/devices/0/bm"));
        assert!(spec.contains("/devices/0/peq/reset"));
        assert!(spec.contains("/devices/0/peq/default-frequencies"));
        assert!(spec.contains("/devices/{id}/play"));
        assert!(spec.contains("\"tail-marker\""));
        assert!(spec.contains("/play.startedAtUs"));
    }

    #[test]
    fn previous_source_stays_pascal_case() {
        assert_eq!(format_previous_source(Source::Toslink), "Toslink");
        assert_eq!(format_previous_source(Source::Usb), "Usb");
    }

    #[test]
    fn rejects_oversized_playback_requests() {
        let plan = PlaybackPlan::build(PlayRequest {
            sweep_duration_sec: 20.0,
            ..PlayRequest::default()
        })
        .expect("oversize request should still be a valid shared playback plan");

        let err = validate_esp32_playback_size(&plan)
            .expect_err("ESP32 should reject oversize playback buffers");
        assert!(err.contains("ESP32 buffer limit"));
    }
}
