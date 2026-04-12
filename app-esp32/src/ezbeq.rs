//! ezBEQ integration: filter load/clear, slot state, REST API handlers.
//!
//! Reads biquad coefficients from the binary catalogue index on LittleFS,
//! writes them to MiniDSP input PEQ bands via USB HID.
//!
//! Entry lookup uses digest-based binary search (BEQ\x02 format).
//! The browser fetches the full catalogue from our GitHub Pages mirror;
//! the ESP32 only stores catalogue.bin for biquad coefficient lookup.

use crate::device::DeviceState;
use crate::http_server::SharedDeviceState;
use crate::storage;
use log::info;
use std::io::{Read, Seek, SeekFrom};
use std::sync::{Arc, Mutex};

// Binary index constants (must match build-catalogue.py v2)
const BIN_MAGIC: &[u8; 4] = b"BEQ\x02";
const DIGEST_PREFIX_SIZE: usize = 8;
const MAX_FILTERS: usize = 10;
const COEFFS_PER_FILTER: usize = 5;
/// Per-entry record: 8 bytes digest_prefix + 1 byte filter_count + 4 bytes mv_adjust + 10*5*4 bytes coefficients
const RECORD_SIZE: usize = DIGEST_PREFIX_SIZE + 1 + 4 + MAX_FILTERS * COEFFS_PER_FILTER * 4; // 213
/// Header: 4 bytes magic + 4 bytes entry_count + 4 bytes sample_rate
const HEADER_SIZE: usize = 12;

/// Number of ezBEQ slots (matches MiniDSP presets 1-4).
const NUM_SLOTS: usize = 4;

/// Bypass biquad: identity filter [1, 0, 0, 0, 0].
const BYPASS_COEFFS: [f32; 5] = [1.0, 0.0, 0.0, 0.0, 0.0];

/// Per-slot state tracking.
#[derive(Clone, Default, serde::Serialize)]
pub struct SlotState {
    pub id: String,
    pub last: String,
    pub active: bool,
    pub author: Option<String>,
    pub gains: Vec<ChannelValue<f32>>,
    pub mutes: Vec<ChannelValue<bool>>,
    #[serde(rename = "canActivate")]
    pub can_activate: bool,
    pub inputs: u8,
    pub outputs: u8,
}

#[derive(Clone, serde::Serialize)]
pub struct ChannelValue<T: Clone> {
    pub id: String,
    pub value: T,
}

/// Shared ezBEQ state.
pub struct EzBeqState {
    pub slots: [SlotState; NUM_SLOTS],
    /// Detached WS sender for pushing state updates to the connected /ws client.
    #[cfg(esp_idf_httpd_ws_support)]
    pub ws_sender: Option<esp_idf_svc::http::server::ws::EspHttpWsDetachedSender>,
}

pub type SharedEzBeqState = Arc<Mutex<EzBeqState>>;

impl EzBeqState {
    pub fn new(inputs: u8, outputs: u8) -> Self {
        let slots = core::array::from_fn::<SlotState, NUM_SLOTS, _>(|i| SlotState {
            id: (i + 1).to_string(),
            last: "Empty".into(),
            active: i == 0,
            author: None,
            gains: (0..inputs)
                .map(|ch| ChannelValue {
                    id: (ch + 1).to_string(),
                    value: 0.0,
                })
                .collect(),
            mutes: (0..inputs)
                .map(|ch| ChannelValue {
                    id: (ch + 1).to_string(),
                    value: false,
                })
                .collect(),
            can_activate: true,
            inputs,
            outputs,
        });
        Self {
            slots,
            #[cfg(esp_idf_httpd_ws_support)]
            ws_sender: None,
        }
    }
}

/// Parse the first 8 bytes of a hex digest string into a byte array for binary search.
fn parse_digest_prefix(
    digest_hex: &str,
) -> Result<[u8; DIGEST_PREFIX_SIZE], Box<dyn std::error::Error>> {
    if digest_hex.len() < DIGEST_PREFIX_SIZE * 2 {
        return Err(format!(
            "digest too short: '{}' (need {} hex chars)",
            digest_hex,
            DIGEST_PREFIX_SIZE * 2
        )
        .into());
    }
    let mut prefix = [0u8; DIGEST_PREFIX_SIZE];
    for i in 0..DIGEST_PREFIX_SIZE {
        prefix[i] = u8::from_str_radix(&digest_hex[i * 2..i * 2 + 2], 16)
            .map_err(|_| format!("invalid hex in digest: '{}'", digest_hex))?;
    }
    Ok(prefix)
}

/// Read biquad coefficients for an entry by digest hash (binary search).
///
/// Returns (filter_count, mv_adjust, coefficients[filter_count][5]).
pub fn read_entry_biquads_by_digest(
    digest_hex: &str,
) -> Result<(u8, f32, Vec<[f32; 5]>), Box<dyn std::error::Error>> {
    let target = parse_digest_prefix(digest_hex)?;

    let path = storage::full_path("catalogue.bin");
    let mut file = std::fs::File::open(&path)?;

    // Read and verify header
    let mut header = [0u8; HEADER_SIZE];
    file.read_exact(&mut header)?;
    if &header[0..4] != BIN_MAGIC {
        return Err("bad catalogue.bin magic (expected BEQ\\x02)".into());
    }
    let entry_count = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
    if entry_count == 0 {
        return Err("catalogue is empty".into());
    }

    // Binary search on sorted digest prefixes
    let mut lo: usize = 0;
    let mut hi: usize = entry_count - 1;
    let mut prefix_buf = [0u8; DIGEST_PREFIX_SIZE];

    while lo <= hi {
        let mid = lo + (hi - lo) / 2;
        let offset = HEADER_SIZE + mid * RECORD_SIZE;
        file.seek(SeekFrom::Start(offset as u64))?;
        file.read_exact(&mut prefix_buf)?;

        match prefix_buf.cmp(&target) {
            std::cmp::Ordering::Equal => {
                // Found — read the rest of the record
                let mut record = [0u8; RECORD_SIZE - DIGEST_PREFIX_SIZE];
                file.read_exact(&mut record)?;
                return parse_biquad_record(&record);
            }
            std::cmp::Ordering::Less => {
                lo = mid + 1;
            }
            std::cmp::Ordering::Greater => {
                if mid == 0 {
                    break;
                }
                hi = mid - 1;
            }
        }
    }

    Err("entry not found in catalogue".into())
}

/// Parse the biquad portion of a record (after digest prefix).
fn parse_biquad_record(
    record: &[u8],
) -> Result<(u8, f32, Vec<[f32; 5]>), Box<dyn std::error::Error>> {
    if record.len() < 5 {
        return Err(format!("record too short: {} bytes", record.len()).into());
    }
    let filter_count = record[0];
    if filter_count as usize > MAX_FILTERS {
        return Err(format!(
            "filter_count {} exceeds MAX_FILTERS {}",
            filter_count, MAX_FILTERS
        )
        .into());
    }
    let mv_adjust = f32::from_le_bytes([record[1], record[2], record[3], record[4]]);
    if !mv_adjust.is_finite() || mv_adjust.abs() > 40.0 {
        return Err(
            format!("mv_adjust {mv_adjust} out of range (must be finite, |mv| <= 40)").into(),
        );
    }

    let needed = 5 + filter_count as usize * COEFFS_PER_FILTER * 4;
    if record.len() < needed {
        return Err(format!(
            "record too short for {filter_count} filters: have {}, need {needed}",
            record.len()
        )
        .into());
    }

    let mut coeffs = Vec::with_capacity(filter_count as usize);
    for i in 0..filter_count as usize {
        let base = 5 + i * COEFFS_PER_FILTER * 4;
        let mut c = [0f32; 5];
        for j in 0..5 {
            let offset = base + j * 4;
            c[j] = f32::from_le_bytes([
                record[offset],
                record[offset + 1],
                record[offset + 2],
                record[offset + 3],
            ]);
        }
        coeffs.push(c);
    }

    Ok((filter_count, mv_adjust, coeffs))
}

/// Load an ezBEQ filter into the device's input PEQ bands.
///
/// Writes biquad coefficients to all input channels' PEQ bands.
/// Unused bands are set to bypass (identity filter).
pub fn load_filter(
    device: &DeviceState,
    digest: &str,
    title: &str,
    _author: &str,
) -> Result<f32, String> {
    let (filter_count, mv_adjust, coeffs) =
        read_entry_biquads_by_digest(digest).map_err(|e| e.to_string())?;
    let table = device
        .profile()
        .dsp_table
        .ok_or("device has no DSP table")?;

    info!("loading BEQ: {title} ({filter_count} filters, mv={mv_adjust:+.1}dB)");

    // Write filters to input PEQ bands on ALL input channels
    for (ch_idx, input) in table.inputs.iter().enumerate() {
        for (band_idx, band) in input.peq.iter().enumerate() {
            if band_idx < filter_count as usize {
                device
                    .write_biquad(band.addr, &coeffs[band_idx])
                    .map_err(|e| e.to_string())?;
                device
                    .bypass_filter(band.addr, false)
                    .map_err(|e| e.to_string())?;
            } else {
                device
                    .write_biquad(band.addr, &BYPASS_COEFFS)
                    .map_err(|e| e.to_string())?;
                device
                    .bypass_filter(band.addr, true)
                    .map_err(|e| e.to_string())?;
            }
        }

        // Apply MV adjust to input channel gain (dB → linear)
        if let Some(gain_addr) = input.gain {
            let gain_linear = 10.0f32.powf(mv_adjust / 20.0);
            device
                .write_dsp(gain_addr, gain_linear, true)
                .map_err(|e| e.to_string())?;
        }

        log::debug!(
            "  ch{ch_idx}: wrote {filter_count}/{} PEQ bands, gain={mv_adjust:+.1}dB",
            input.peq.len()
        );
    }

    Ok(mv_adjust)
}

/// Clear all input PEQ bands (set to bypass).
pub fn clear_filter(device: &DeviceState) -> Result<(), String> {
    let table = device
        .profile()
        .dsp_table
        .ok_or("device has no DSP table")?;

    for input in table.inputs {
        for band in input.peq {
            device
                .write_biquad(band.addr, &BYPASS_COEFFS)
                .map_err(|e| e.to_string())?;
            device
                .bypass_filter(band.addr, true)
                .map_err(|e| e.to_string())?;
        }
        // Reset input gain to 0dB (linear 1.0)
        if let Some(gain_addr) = input.gain {
            device
                .write_dsp(gain_addr, 1.0, true)
                .map_err(|e| e.to_string())?;
        }
    }

    Ok(())
}

/// Push a DeviceState update to the connected /ws client (if any).
#[cfg(esp_idf_httpd_ws_support)]
pub fn broadcast_state(device: &DeviceState, beq_state: &mut EzBeqState) {
    if beq_state.ws_sender.is_none() {
        return;
    }
    let json = device_state_json(device, beq_state);
    if let Some(ref mut sender) = beq_state.ws_sender {
        if sender
            .send(esp_idf_svc::ws::FrameType::Text(false), json.as_bytes())
            .is_err()
        {
            beq_state.ws_sender = None;
        }
    }
}

/// Build the DeviceState JSON for the ezBEQ WebSocket protocol.
pub fn device_state_json(device: &DeviceState, beq_state: &EzBeqState) -> String {
    let slots_json: Vec<serde_json::Value> = beq_state
        .slots
        .iter()
        .map(|s| serde_json::to_value(s).unwrap_or_default())
        .collect();

    // Single USB round-trip instead of two
    let ms = device.get_master_status().ok();
    let volume = ms.as_ref().map(|m| m.volume_db()).unwrap_or(-10.0);
    let mute = ms.as_ref().map(|m| m.is_muted()).unwrap_or(false);

    serde_json::to_string(&serde_json::json!({
        "message": "DeviceState",
        "data": {
            "type": "minidsp",
            "name": "master",
            "masterVolume": volume,
            "mute": mute,
            "slots": slots_json,
        }
    }))
    .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// HTTP route registration
// ---------------------------------------------------------------------------

/// Register ezBEQ REST API routes on the HTTP server.
pub fn register_routes(
    server: &mut esp_idf_svc::http::server::EspHttpServer<'static>,
    device_state: SharedDeviceState,
    beq_state: SharedEzBeqState,
    nvs: esp_idf_svc::nvs::EspDefaultNvsPartition,
) -> Result<(), esp_idf_svc::sys::EspError> {
    use crate::http_server::{get_device, send_error, send_json};
    use esp_idf_svc::http::Method;

    // GET /api/1/version
    server.fn_handler("/api/1/version", Method::Get, |req| {
        let version = env!("CARGO_PKG_VERSION");
        let body = format!(r#"{{"version":"{version}"}}"#);
        send_json(req, 200, &body)
    })?;

    // Catalogue filter endpoints — return empty arrays (catalogue is loaded by the browser directly)
    for path in &[
        "/api/1/authors",
        "/api/1/years",
        "/api/1/audiotypes",
        "/api/1/contenttypes",
        "/api/1/languages",
    ] {
        server.fn_handler(path, Method::Get, |req| send_json(req, 200, "[]"))?;
    }

    // GET /api/2/devices — device list with ezBEQ state
    {
        let st = device_state.clone();
        let beq = beq_state.clone();
        server.fn_handler("/api/2/devices", Method::Get, move |req| {
            let dev = match get_device(&st) {
                Some(d) => d,
                None => return send_json(req, 200, "{}"),
            };
            let _op = dev.begin_operation();
            let beq_guard = beq.lock().unwrap_or_else(|e| e.into_inner());
            let json = device_state_json_rest(&dev, &beq_guard);
            send_json(req, 200, &json)
        })?;
    }

    // PUT /api/1/devices/master/filter/{slot} — load a BEQ filter by digest
    for slot_idx in 0..NUM_SLOTS {
        let st = device_state.clone();
        let beq = beq_state.clone();
        let nvs = nvs.clone();
        let path = format!("/api/1/devices/master/filter/{}", slot_idx + 1);
        server.fn_handler(&path, Method::Put, move |mut req| {
            let dev = match get_device(&st) {
                Some(d) => d,
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
            };

            // Parse request body: {"entryId": "<digest>", "title": "...", "author": "..."}
            let mut buf = [0u8; 1024];
            let len = req.read(&mut buf).unwrap_or(0);
            let json: serde_json::Value = match serde_json::from_slice(&buf[..len]) {
                Ok(v) => v,
                Err(_) => return send_error(req, 400, "invalid JSON"),
            };

            let digest = match json.get("entryId").and_then(|v| v.as_str()) {
                Some(id) => id,
                None => return send_error(req, 400, "missing entryId"),
            };

            // Title and author are enriched by the JS shim (truncate to prevent heap bloat)
            let raw_title = json
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown");
            let raw_author = json
                .get("author")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown");
            let title = if raw_title.len() > 256 {
                &raw_title[..256]
            } else {
                raw_title
            };
            let author = if raw_author.len() > 256 {
                &raw_author[..256]
            } else {
                raw_author
            };
            let _op = dev.begin_operation();

            match load_filter(&dev, digest, title, author) {
                Ok(mv_adjust) => {
                    let mut beq_guard = beq.lock().unwrap_or_else(|e| e.into_inner());
                    beq_guard.slots[slot_idx].last = title.to_string();
                    beq_guard.slots[slot_idx].author = Some(author.to_string());
                    // Mark this slot as active, all others inactive
                    for (i, slot) in beq_guard.slots.iter_mut().enumerate() {
                        slot.active = i == slot_idx;
                    }
                    for gain in &mut beq_guard.slots[slot_idx].gains {
                        gain.value = mv_adjust;
                    }
                    #[cfg(esp_idf_httpd_ws_support)]
                    broadcast_state(&dev, &mut beq_guard);

                    let body = serde_json::to_string(&serde_json::json!({
                        "status": "ok",
                        "slot": slot_idx + 1,
                        "mvAdjust": mv_adjust,
                    }))
                    .unwrap_or_default();
                    send_json(req, 200, &body)
                }
                Err(e) => {
                    if e.contains("not found") {
                        let started = crate::updater::trigger_background_refresh(
                            nvs.clone(),
                            "catalogue entry missing during filter load",
                        );
                        let body = serde_json::json!({
                            "error": e,
                            "refreshStarted": started,
                        });
                        send_json(req, 404, &serde_json::to_string(&body).unwrap_or_default())
                    } else {
                        send_error(req, 500, &e)
                    }
                }
            }
        })?;
    }

    // DELETE /api/1/devices/master/filter/{slot} — clear a BEQ filter
    for slot_idx in 0..NUM_SLOTS {
        let st = device_state.clone();
        let beq = beq_state.clone();
        let path = format!("/api/1/devices/master/filter/{}", slot_idx + 1);
        server.fn_handler(&path, Method::Delete, move |req| {
            let dev = match get_device(&st) {
                Some(d) => d,
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
            };
            let _op = dev.begin_operation();

            match clear_filter(&dev) {
                Ok(()) => {
                    let mut beq_guard = beq.lock().unwrap_or_else(|e| e.into_inner());
                    beq_guard.slots[slot_idx].last = "Empty".into();
                    beq_guard.slots[slot_idx].author = None;
                    for gain in &mut beq_guard.slots[slot_idx].gains {
                        gain.value = 0.0;
                    }
                    #[cfg(esp_idf_httpd_ws_support)]
                    broadcast_state(&dev, &mut beq_guard);

                    send_json(req, 200, r#"{"status":"ok"}"#)
                }
                Err(e) => send_error(req, 500, &e.to_string()),
            }
        })?;
    }

    // PATCH /api/3/devices/master — device control (volume, mute)
    {
        let st = device_state.clone();
        server.fn_handler("/api/3/devices/master", Method::Patch, move |mut req| {
            let dev = match get_device(&st) {
                Some(d) => d,
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
            };
            let mut buf = [0u8; 256];
            let len = req.read(&mut buf).unwrap_or(0);
            let json: serde_json::Value = match serde_json::from_slice(&buf[..len]) {
                Ok(v) => v,
                Err(_) => return send_error(req, 400, "invalid JSON"),
            };
            let _op = dev.begin_operation();

            if let Some(vol) = json.get("masterVolume").and_then(|v| v.as_f64()) {
                if let Err(e) = dev.set_volume_db(vol as f32) {
                    return send_error(req, 500, &format!("set_volume: {e}"));
                }
            }
            if let Some(mute) = json.get("mute").and_then(|v| v.as_bool()) {
                if let Err(e) = dev.set_mute(mute) {
                    return send_error(req, 500, &format!("set_mute: {e}"));
                }
            }

            send_json(req, 200, r#"{"status":"ok"}"#)
        })?;
    }

    // PUT /api/1/devices/master/commands — preset save/overwrite (used by React UI)
    {
        let st = device_state.clone();
        server.fn_handler(
            "/api/1/devices/master/commands",
            Method::Put,
            move |mut req| {
                let dev = match get_device(&st) {
                    Some(d) => d,
                    None => return send_json(req, 503, r#"{"error":"no device"}"#),
                };
                let mut buf = [0u8; 512];
                let len = req.read(&mut buf).unwrap_or(0);
                let json: serde_json::Value = match serde_json::from_slice(&buf[..len]) {
                    Ok(v) => v,
                    Err(_) => return send_error(req, 400, "invalid JSON"),
                };
                let _op = dev.begin_operation();

                // Handle "overwrite" command (copy current preset to flash)
                if json
                    .get("overwrite")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    if let Err(e) = dev.copy_preset() {
                        return send_error(req, 500, &format!("copy_preset: {e}"));
                    }
                }

                send_json(req, 200, r#"{"status":"ok"}"#)
            },
        )?;
    }

    // PUT /api/1/devices/master/config/{slot}/active — activate a slot
    for slot_idx in 0..NUM_SLOTS {
        let beq = beq_state.clone();
        let path = format!("/api/1/devices/master/config/{}/active", slot_idx + 1);
        server.fn_handler(&path, Method::Put, move |req| {
            let mut beq_guard = beq.lock().unwrap_or_else(|e| e.into_inner());
            for (i, slot) in beq_guard.slots.iter_mut().enumerate() {
                slot.active = i == slot_idx;
            }
            let body = format!(r#"{{"status":"ok","active":{}}}"#, slot_idx + 1);
            send_json(req, 200, &body)
        })?;
    }

    // GET /api/1/search — catalogue search (handled client-side by shim, return empty)
    server.fn_handler("/api/1/search", Method::Get, |req| {
        send_json(req, 200, "[]")
    })?;

    info!("ezBEQ API routes registered");
    Ok(())
}

/// Build REST response for GET /api/2/devices (different envelope than WS).
fn device_state_json_rest(device: &DeviceState, beq_state: &EzBeqState) -> String {
    let slots_json: Vec<serde_json::Value> = beq_state
        .slots
        .iter()
        .map(|s| serde_json::to_value(s).unwrap_or_default())
        .collect();

    let ms = device.get_master_status().ok();
    let volume = ms.as_ref().map(|m| m.volume_db()).unwrap_or(-10.0);
    let mute = ms.as_ref().map(|m| m.is_muted()).unwrap_or(false);

    serde_json::to_string(&serde_json::json!({
        "master": {
            "type": "minidsp",
            "name": "master",
            "masterVolume": volume,
            "mute": mute,
            "slots": slots_json,
        }
    }))
    .unwrap_or_default()
}
