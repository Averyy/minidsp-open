//! ezBEQ integration: filter load/clear, slot state, REST API handlers.
//!
//! Reads biquad coefficients from the binary catalogue index on LittleFS,
//! writes them to MiniDSP input PEQ bands via USB HID.
//!
//! Entry lookup uses digest-based binary search (BEQ v3; v2 read compatibility).
//! The browser fetches the full catalogue through the board's same-origin
//! `/database.json` endpoint. The board stores a compressed metadata listing
//! and a binary coefficient index, plus a sorted feed used for slot metadata.

use crate::device::DeviceState;
use crate::http_server::SharedDeviceState;
use crate::storage;
use log::info;
use minidsp_protocol::status::StatusSummary;
use std::io::{BufRead, Read, Seek, SeekFrom};
use std::sync::{Arc, Mutex};

/// Number of ezBEQ slots (matches MiniDSP presets 1-4).
const NUM_SLOTS: usize = 4;

/// Bypass biquad: identity filter [1, 0, 0, 0, 0].
const BYPASS_COEFFS: [f32; 5] = [1.0, 0.0, 0.0, 0.0, 0.0];

/// Per-slot state tracking.
#[derive(Clone, Default)]
pub struct SlotState {
    pub id: String,
    pub last: String,
    pub active: bool,
    pub author: Option<String>,
    pub gains: Vec<ChannelValue<f32>>,
    pub mutes: Vec<ChannelValue<bool>>,
    pub can_activate: bool,
    pub inputs: u8,
    pub outputs: u8,
    pub optimise: bool,
    pub coefficients: Option<String>,
    pub profile: Option<String>,
    pub output_gains: Vec<ChannelValue<f32>>,
    pub output_mutes: Vec<ChannelValue<bool>>,
}

/// Writes the ezBEQ slot shape straight to the output. A `serde_json::Value`
/// tree of the four slots took ~60 KB of small allocations, which drained
/// internal RAM on every state push.
impl serde::Serialize for SlotState {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        // An empty slot carries no coefficient provenance, whatever was last loaded.
        let loaded = self.last != "Empty";
        let coefficients = self.coefficients.as_ref().filter(|_| loaded);
        let profile = self.profile.as_ref().filter(|_| loaded);
        let mut slot = serializer.serialize_struct("SlotState", 14)?;
        slot.serialize_field("id", &self.id)?;
        slot.serialize_field("last", &self.last)?;
        slot.serialize_field("active", &self.active)?;
        slot.serialize_field("author", &self.author)?;
        slot.serialize_field("gains", &self.gains)?;
        slot.serialize_field("mutes", &self.mutes)?;
        slot.serialize_field("canActivate", &self.can_activate)?;
        slot.serialize_field("inputs", &self.inputs)?;
        slot.serialize_field("outputs", &self.outputs)?;
        slot.serialize_field("optimise", &self.optimise)?;
        match coefficients {
            Some(coefficients) => slot.serialize_field("coefficients", coefficients)?,
            None => slot.skip_field("coefficients")?,
        }
        match profile {
            Some(profile) => slot.serialize_field("profile", profile)?,
            None => slot.skip_field("profile")?,
        }
        slot.serialize_field("outputGains", &self.output_gains)?;
        slot.serialize_field("outputMutes", &self.output_mutes)?;
        slot.end()
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct ChannelValue<T: Clone> {
    pub id: String,
    pub value: T,
}

/// Shared ezBEQ state.
pub struct EzBeqState {
    pub slots: [SlotState; NUM_SLOTS],
    /// Compatibility notification trigger used by the shared HTTP control paths.
    /// Always aliases the first state sender; broadcasts use the full list below.
    #[cfg(esp_idf_httpd_ws_support)]
    pub ws_sender: Option<esp_idf_svc::http::server::ws::EspHttpWsDetachedSender>,
    #[cfg(esp_idf_httpd_ws_support)]
    pub ws_senders: Vec<(u64, esp_idf_svc::http::server::ws::EspHttpWsDetachedSender)>,
    #[cfg(esp_idf_httpd_ws_support)]
    pub pending_state: Option<String>,
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
            optimise: true,
            coefficients: None,
            profile: None,
            output_gains: (0..outputs)
                .map(|ch| ChannelValue {
                    id: (ch + 1).to_string(),
                    value: 0.0,
                })
                .collect(),
            output_mutes: (0..outputs)
                .map(|ch| ChannelValue {
                    id: (ch + 1).to_string(),
                    value: false,
                })
                .collect(),
        });
        Self {
            slots,
            #[cfg(esp_idf_httpd_ws_support)]
            ws_sender: None,
            #[cfg(esp_idf_httpd_ws_support)]
            ws_senders: Vec::new(),
            #[cfg(esp_idf_httpd_ws_support)]
            pending_state: None,
        }
    }
}

#[path = "service_input.rs"]
mod input;
use input::{parse_digest_prefix, truncate_utf8};

fn read_request_body<C: esp_idf_svc::http::server::Connection>(
    req: &mut esp_idf_svc::http::server::Request<C>,
    max_len: usize,
) -> Result<Vec<u8>, (u16, &'static str)> {
    let declared_len = req
        .header("Content-Length")
        .map(|value| value.parse::<usize>())
        .transpose()
        .map_err(|_| (400, "invalid Content-Length"))?;
    input::read_body(declared_len, max_len, |buf| req.read(buf))
}

type EntryBiquads = (u8, f32, Vec<[f32; 5]>, bool);

/// Read biquad coefficients for an entry by digest hash (binary search).
///
/// Returns (filter_count, mv_adjust, coefficients[filter_count][5]).
pub fn read_entry_biquads_by_digest(
    digest_hex: &str,
    expected_rate: u32,
) -> Result<EntryBiquads, Box<dyn std::error::Error>> {
    let target = parse_digest_prefix(digest_hex)?;

    let mut file = std::fs::File::open(storage::full_path("catalogue.bin"))?;
    let entry = command_text::read_entry(&mut file, &target, Some(expected_rate))?;
    Ok((
        entry.coefficients.len() as u8,
        entry.mv,
        entry.coefficients,
        entry.optimised,
    ))
}

/// Resolve display metadata on the board, using the same binary index as the
/// coefficients. The record points into the existing sorted feed, avoiding a
/// second copy of every title/author in flash and all browser request rewriting.
fn entry_metadata(digest: &str) -> Result<(String, String), String> {
    let target = parse_digest_prefix(digest)?;
    let mut file =
        std::fs::File::open(storage::full_path("catalogue.bin")).map_err(|e| e.to_string())?;
    let entry = command_text::read_entry(&mut file, &target, None)?;
    let Some(offset) = entry.metadata_offset else {
        // Compatibility with old BEQ v2 bundles until their next data update.
        return Ok((digest.to_string(), String::new()));
    };
    let mut feed = std::io::BufReader::new(
        std::fs::File::open(storage::full_path("entry-metadata.jsonl"))
            .map_err(|e| e.to_string())?,
    );
    feed.seek(SeekFrom::Start(offset as u64))
        .map_err(|e| e.to_string())?;
    let mut line = String::new();
    feed.take(4096)
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    let row: Vec<serde_json::Value> = serde_json::from_str(&line).map_err(|e| e.to_string())?;
    if row.first().and_then(|v| v.as_str()) != Some(digest) {
        return Err("catalogue metadata/index mismatch; update the bundle".into());
    }
    let title = row
        .get(1)
        .and_then(|v| v.as_str())
        .ok_or("missing entry title")?;
    let author = row.get(2).and_then(|v| v.as_str()).unwrap_or("");
    Ok((
        truncate_utf8(title, 256).to_string(),
        truncate_utf8(author, 256).to_string(),
    ))
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
) -> Result<(f32, bool), String> {
    let (filter_count, mv_adjust, coeffs, optimised) =
        read_entry_biquads_by_digest(digest, device.profile().sample_rate)
            .map_err(|e| e.to_string())?;
    let table = device
        .profile()
        .dsp_table
        .ok_or("device has no DSP table")?;

    if table.inputs.is_empty()
        || table
            .inputs
            .iter()
            .any(|ch| ch.peq.len() < filter_count as usize)
    {
        return Err("device has insufficient input PEQ bands for this catalogue entry".into());
    }
    if optimised
        && device.profile().dialect.value_format != minidsp_protocol::dialect::ValueFormat::Float32
    {
        return Err("optimised catalogue coefficients require a float32 DSP".into());
    }
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

        log::debug!(
            "  ch{ch_idx}: wrote {filter_count}/{} PEQ bands, mv={mv_adjust:+.1}dB",
            input.peq.len()
        );
    }

    Ok((mv_adjust, optimised))
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
        // Match ezBEQ clear: reset input gain and mute.
        if let Some(gain_addr) = input.gain {
            device
                .set_channel_gain(gain_addr, 0.0)
                .map_err(|e| e.to_string())?;
        }
        if let Some(mute_addr) = input.mute {
            device
                .set_channel_mute(mute_addr, false)
                .map_err(|e| e.to_string())?;
        }
    }

    Ok(())
}

fn parse_slot_index(slot_id: &str) -> Result<usize, String> {
    let slot_num = slot_id
        .parse::<usize>()
        .map_err(|_| format!("invalid slot id '{slot_id}'"))?;
    if !(1..=NUM_SLOTS).contains(&slot_num) {
        return Err(format!("slot id {slot_num} out of range (1..={NUM_SLOTS})"));
    }
    Ok(slot_num - 1)
}

fn set_input_gain_mute_state(
    device: &DeviceState,
    gains: &[ChannelValue<f32>],
    mutes: &[ChannelValue<bool>],
) -> Result<(), String> {
    let table = device
        .profile()
        .dsp_table
        .ok_or("device has no DSP table")?;

    for gain in gains {
        if !gain.value.is_finite() || !(-72.0..=12.0).contains(&gain.value) {
            return Err(format!(
                "gain for input {} out of range (must be finite, |gain| <= 120 dB)",
                gain.id
            ));
        }
        let ch = gain
            .id
            .parse::<usize>()
            .map_err(|_| format!("invalid gain channel id '{}'", gain.id))?;
        let input = table
            .inputs
            .get(ch.checked_sub(1).ok_or("channel ids start at 1")?)
            .ok_or_else(|| format!("gain channel {} out of range", gain.id))?;
        let addr = input
            .gain
            .ok_or_else(|| format!("input {} has no gain control", gain.id))?;
        device
            .set_channel_gain(addr, gain.value)
            .map_err(|e| format!("set input {} gain: {e}", gain.id))?;
    }

    for mute in mutes {
        let ch = mute
            .id
            .parse::<usize>()
            .map_err(|_| format!("invalid mute channel id '{}'", mute.id))?;
        let input = table
            .inputs
            .get(ch.checked_sub(1).ok_or("channel ids start at 1")?)
            .ok_or_else(|| format!("mute channel {} out of range", mute.id))?;
        let addr = input
            .mute
            .ok_or_else(|| format!("input {} has no mute control", mute.id))?;
        device
            .set_channel_mute(addr, mute.value)
            .map_err(|e| format!("set input {} mute: {e}", mute.id))?;
    }

    Ok(())
}

fn set_provenance(slot: &mut SlotState, optimised: bool) {
    slot.coefficients = Some(if optimised { "optimised" } else { "standard" }.into());
    slot.profile = optimised.then(|| "float32-96k".into());
}

fn set_output_gain_mute_state(
    device: &DeviceState,
    gains: &[ChannelValue<f32>],
    mutes: &[ChannelValue<bool>],
) -> Result<(), String> {
    let table = device
        .profile()
        .dsp_table
        .ok_or("device has no DSP table")?;
    for gain in gains {
        let ch = gain
            .id
            .parse::<usize>()
            .map_err(|_| "invalid output channel")?;
        let output = table
            .outputs
            .get(ch.checked_sub(1).ok_or("channel ids start at 1")?)
            .ok_or("output channel out of range")?;
        device
            .set_channel_gain(output.gain, gain.value)
            .map_err(|e| e.to_string())?;
    }
    for mute in mutes {
        let ch = mute
            .id
            .parse::<usize>()
            .map_err(|_| "invalid output channel")?;
        let output = table
            .outputs
            .get(ch.checked_sub(1).ok_or("channel ids start at 1")?)
            .ok_or("output channel out of range")?;
        device
            .set_channel_mute(output.mute, mute.value)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Validate advertised PATCH fields before changing the active preset or DSP.
fn validate_patch(device: &DeviceState, json: &serde_json::Value) -> Result<(), String> {
    let fields = json.as_object().ok_or("device patch must be an object")?;
    for key in fields.keys() {
        if !["masterVolume", "mute", "slots"].contains(&key.as_str()) {
            return Err(format!("unsupported device patch field {key}"));
        }
    }
    if let Some(vol) = json.get("masterVolume") {
        let vol = vol.as_f64().ok_or("masterVolume must be a number")?;
        if !vol.is_finite() || !(-127.0..=0.0).contains(&vol) {
            return Err("masterVolume must be between -127 and 0 dB".into());
        }
    }
    if json.get("mute").is_some_and(|v| !v.is_boolean()) {
        return Err("mute must be a boolean".into());
    }
    let Some(slots) = json.get("slots") else {
        return Ok(());
    };
    let table = device
        .profile()
        .dsp_table
        .ok_or("device has no DSP table")?;
    for slot in slots.as_array().ok_or("slots must be an array")? {
        let fields = slot.as_object().ok_or("slot must be an object")?;
        parse_slot_index(
            slot.get("id")
                .and_then(|v| v.as_str())
                .ok_or("slot patch missing id")?,
        )?;
        for key in fields.keys() {
            if ![
                "id",
                "entry",
                "title",
                "author",
                "gains",
                "mutes",
                "outputGains",
                "outputMutes",
                "optimise",
                "active",
            ]
            .contains(&key.as_str())
            {
                return Err(format!("unsupported slot patch field {key}"));
            }
        }
        command_text::slot_steps(slot)?;
        if let Some(value) = slot.get("optimise") {
            if value.as_bool() != Some(true) {
                return Err("this bundle always selects published optimised coefficients; disabling optimisation is unsupported".into());
            }
        }
        if let Some(entry) = slot.get("entry") {
            let digest = entry.as_str().ok_or("slot entry must be a string")?;
            if !digest.is_empty() {
                entry_metadata(digest)?;
                let (count, _, _, optimised) =
                    read_entry_biquads_by_digest(digest, device.profile().sample_rate)
                        .map_err(|e| e.to_string())?;
                if table.inputs.is_empty()
                    || table.inputs.iter().any(|ch| ch.peq.len() < count as usize)
                {
                    return Err(
                        "device has insufficient input PEQ bands for this catalogue entry".into(),
                    );
                }
                if optimised
                    && device.profile().dialect.value_format
                        != minidsp_protocol::dialect::ValueFormat::Float32
                {
                    return Err("optimised catalogue coefficients require a float32 DSP".into());
                }
            }
        }
        for key in ["title", "author"] {
            if slot
                .get(key)
                .is_some_and(|v| !v.is_string() || v.as_str().unwrap().len() > 256)
            {
                return Err(format!("{key} must be a string of at most 256 bytes"));
            }
        }
        for (key, output, gain) in [
            ("gains", false, true),
            ("mutes", false, false),
            ("outputGains", true, true),
            ("outputMutes", true, false),
        ] {
            if let Some(values) = slot.get(key) {
                for value in values
                    .as_array()
                    .ok_or_else(|| format!("{key} must be an array"))?
                {
                    let ch = value
                        .get("id")
                        .and_then(|v| v.as_str())
                        .ok_or("channel id must be a string")?
                        .parse::<usize>()
                        .map_err(|_| "invalid channel id")?;
                    let idx = ch.checked_sub(1).ok_or("channel ids start at 1")?;
                    if output {
                        if table.outputs.get(idx).is_none() {
                            return Err("output channel out of range".into());
                        }
                    } else {
                        let input = table.inputs.get(idx).ok_or("input channel out of range")?;
                        if (gain && input.gain.is_none()) || (!gain && input.mute.is_none()) {
                            return Err("input has no requested control".into());
                        }
                    }
                    if gain {
                        let number = value
                            .get("value")
                            .and_then(|v| v.as_f64())
                            .ok_or("gain must be a number")?;
                        if !number.is_finite() || !(-72.0..=12.0).contains(&number) {
                            return Err("gain out of range".into());
                        }
                    } else if !value.get("value").is_some_and(|v| v.is_boolean()) {
                        return Err("mute must be a boolean".into());
                    }
                }
            }
        }
    }
    Ok(())
}

#[path = "ezbeq_contract.rs"]
mod command_text;

fn sync_slot_state(
    slot: &mut SlotState,
    title: Option<&str>,
    author: Option<&str>,
    gains: &[ChannelValue<f32>],
    mutes: &[ChannelValue<bool>],
) {
    if let Some(title) = title {
        slot.last = if title.is_empty() {
            "Empty".into()
        } else {
            title.to_string()
        };
    }
    if let Some(author) = author {
        slot.author = if author.is_empty() {
            None
        } else {
            Some(author.to_string())
        };
    }

    for gain in gains {
        if let Some(existing) = slot.gains.iter_mut().find(|ch| ch.id == gain.id) {
            existing.value = gain.value;
        }
    }
    for mute in mutes {
        if let Some(existing) = slot.mutes.iter_mut().find(|ch| ch.id == mute.id) {
            existing.value = mute.value;
        }
    }
}

/// Keep the legacy notification trigger in sync with the bounded state-client list.
#[cfg(esp_idf_httpd_ws_support)]
pub fn remove_ws_sender(state: &mut EzBeqState, token: u64) {
    state.ws_senders.retain(|(id, _)| *id != token);
    state.ws_sender = state.ws_senders.first().map(|(_, sender)| sender.clone());
}

/// Detached sends wait for httpd work: never call them on the single httpd task.
/// The shared WS worker broadcasts the latest update to at most four state clients.
#[cfg(esp_idf_httpd_ws_support)]
pub fn broadcast_state(
    beq_state: SharedEzBeqState,
    _sender: esp_idf_svc::http::server::ws::EspHttpWsDetachedSender,
    json: String,
) {
    // Coalesce updates into the latest complete state. The shared WS worker
    // performs detached sends off httpd, with a fixed thread/stack budget.
    beq_state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .pending_state = Some(json);
}

/// The ezBEQ device object, borrowed from the slot state and serialized directly.
#[derive(serde::Serialize)]
struct DeviceStateBody<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    name: &'static str,
    #[serde(rename = "masterVolume")]
    master_volume: f32,
    mute: bool,
    connected: bool,
    // Null is understood by 2.12.4: hide the sticky toggle and profile
    // comparisons, while still rendering each loaded slot's provenance chip.
    optimisation: (),
    slots: &'a [SlotState],
}

fn device_state_body<'a>(device: &DeviceState, beq_state: &'a EzBeqState) -> DeviceStateBody<'a> {
    // Single USB round-trip instead of two
    let ms = device.get_master_status().ok();
    DeviceStateBody {
        kind: "minidsp",
        name: "master",
        master_volume: ms.as_ref().map(|m| m.volume_db()).unwrap_or(-10.0),
        mute: ms.as_ref().map(|m| m.is_muted()).unwrap_or(false),
        connected: ms.is_some(),
        optimisation: (),
        slots: &beq_state.slots,
    }
}

/// Build the DeviceState JSON for the ezBEQ WebSocket protocol.
pub fn device_state_json(device: &DeviceState, beq_state: &EzBeqState) -> String {
    #[derive(serde::Serialize)]
    struct Envelope<'a> {
        message: &'static str,
        data: DeviceStateBody<'a>,
    }
    serde_json::to_string(&Envelope {
        message: "DeviceState",
        data: device_state_body(device, beq_state),
    })
    .unwrap_or_default()
}

fn device_state_json_plain(device: &DeviceState, beq_state: &EzBeqState) -> String {
    serde_json::to_string(&device_state_body(device, beq_state)).unwrap_or_default()
}

fn storage_json_or_default(path: &str, default: &str) -> String {
    storage::read_file(path)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_else(|| default.to_string())
}

/// Stream compact, build-time-sorted rows with bounded RAM, including arbitrary
/// since/limit queries. The browser receives the upstream object representation.
fn send_recent_changes<C: esp_idf_svc::http::server::Connection>(
    req: esp_idf_svc::http::server::Request<C>,
) -> Result<(), C::Error> {
    use esp_idf_svc::io::Write;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    // ESP32 may boot without a synchronised wall clock. The feed/listing are
    // built at the same instant; use that snapshot time until a real clock exists.
    let now = if now >= 1_600_000_000 {
        now
    } else {
        storage::read_file("catalogue-built-at.txt")
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .and_then(|value| value.trim().parse::<i64>().ok())
            .unwrap_or(now)
    };
    let mut since = now - 14 * 24 * 60 * 60;
    let mut limit = 50i64;
    if let Some((_, query)) = req.uri().split_once('?') {
        for pair in query.split('&') {
            let Some((key, value)) = pair.split_once('=') else {
                continue;
            };
            if key == "since" || key == "limit" {
                let number = match value.parse::<i64>() {
                    Ok(number) => number,
                    Err(_) => {
                        return crate::http_server::send_error(
                            req,
                            400,
                            "since and limit must be integers",
                        )
                    }
                };
                if key == "since" {
                    since = number;
                } else {
                    limit = number;
                }
            }
        }
    }
    let file = match std::fs::File::open(storage::full_path("recent-changes.jsonl")) {
        Ok(file) => file,
        Err(_) => return crate::http_server::send_json(req, 200, "[]"),
    };
    let mut resp = req.into_response(
        200,
        None,
        &[
            ("Content-Type", "application/json"),
            ("Access-Control-Allow-Origin", "*"),
            ("Cache-Control", "no-store"),
        ],
    )?;
    resp.write_all(b"[")?;
    let mut count = 0i64;
    for (idx, line) in std::io::BufReader::new(file).lines().enumerate() {
        let row = line
            .ok()
            .and_then(|line| serde_json::from_str::<Vec<serde_json::Value>>(&line).ok());
        let Some(row) = row.filter(|row| row.len() == 10) else {
            log::warn!("invalid recent-changes row");
            continue;
        };
        let created = row[7].as_i64().unwrap_or(0);
        let updated = row[8].as_i64().unwrap_or(0);
        if created.max(updated) < since {
            break;
        }
        let mut entry = serde_json::Map::new();
        for (key, value) in [
            "id",
            "formattedTitle",
            "year",
            "author",
            "content_type",
            "audioTypes",
            "language",
            "created_at",
            "updated_at",
            "freshness",
        ]
        .into_iter()
        .zip(row)
        {
            if !value.is_null() && value != "" && value != 0 && value != serde_json::json!([]) {
                entry.insert(key.into(), value);
            }
        }
        if let Some(content) = entry.get("content_type").cloned() {
            entry.insert("contentType".into(), content);
        }
        if count > 0 {
            resp.write_all(b",")?;
        }
        resp.write_all(serde_json::Value::Object(entry).to_string().as_bytes())?;
        count += 1;
        if limit > 0 && count >= limit {
            break;
        }
        if idx % 32 == 31 {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    resp.write_all(b"]")?;
    Ok(())
}

/// The stock UI reports HTTP status but discards error bodies. Its state socket
/// already understands Error messages, so surface the explanation there without
/// rewriting browser fetch() or changing upstream UI code.
fn send_beq_error<C: esp_idf_svc::http::server::Connection>(
    _beq: &SharedEzBeqState,
    req: esp_idf_svc::http::server::Request<C>,
    status: u16,
    message: &str,
) -> Result<(), C::Error> {
    #[cfg(esp_idf_httpd_ws_support)]
    if let Ok(mut state) = _beq.try_lock() {
        // Some hardware-error paths already hold the slot-state lock. Never
        // recursively lock it from the httpd task; HTTP still carries the error
        // if an explanatory WS notification cannot be queued in that case.
        state.pending_state = Some(
            serde_json::json!({"message": "Error", "data": message, "persistent": false})
                .to_string(),
        );
    }
    crate::http_server::send_error(req, status, message)
}

// ---------------------------------------------------------------------------
// HTTP route registration
// ---------------------------------------------------------------------------

/// Register ezBEQ REST API routes on the HTTP server.
pub fn register_routes(
    server: &mut esp_idf_svc::http::server::EspHttpServer<'static>,
    device_state: SharedDeviceState,
    audio_state: crate::usb_audio::SharedAudioState,
    beq_state: SharedEzBeqState,
    nvs: esp_idf_svc::nvs::EspDefaultNvsPartition,
) -> Result<(), esp_idf_svc::sys::EspError> {
    use crate::http_server::{get_device, reject_hid_during_playback, send_error, send_json};
    use esp_idf_svc::http::Method;

    // GET /api/1/version
    server.fn_handler("/api/1/version", Method::Get, |req| {
        let version = env!("CARGO_PKG_VERSION");
        let body = format!(r#"{{"version":"{version}"}}"#);
        send_json(req, 200, &body)
    })?;

    // Catalogue filter metadata endpoints — served from the flashed/updater-managed bundle.
    for (path, storage_path) in [
        ("/api/1/authors", "authors.json"),
        ("/api/1/filterauthors", "filterauthors.json"),
        ("/api/1/years", "years.json"),
        ("/api/1/audiotypes", "audiotypes.json"),
        ("/api/1/contenttypes", "contenttypes.json"),
        ("/api/1/languages", "languages.json"),
    ] {
        server.fn_handler(path, Method::Get, move |req| {
            let body = storage_json_or_default(storage_path, "[]");
            send_json(req, 200, &body)
        })?;
    }

    server.fn_handler("/api/1/whats-new", Method::Get, |req| {
        send_recent_changes(req)
    })?;
    // Parameter comparisons require authored filter parameters, which are not stored.
    server.fn_handler("/api/1/catalogue/filters", Method::Get, |req| {
        send_error(
            req,
            404,
            "filter parameter comparisons are unavailable on this device",
        )
    })?;

    // GET /api/2/devices — device list with ezBEQ state
    {
        let st = device_state.clone();
        let audio = audio_state.clone();
        let beq = beq_state.clone();
        server.fn_handler("/api/2/devices", Method::Get, move |req| {
            let req = match reject_hid_during_playback(req, &audio)? {
                Some(req) => req,
                None => return Ok(()),
            };
            let dev = match get_device(&st) {
                Some(d) => d,
                None => return send_json(req, 200, "{}"),
            };
            let _op = match dev.try_begin_operation() {
                Some(op) => op,
                None => return send_error(req, 409, crate::http_server::DEVICE_BUSY),
            };
            let beq_guard = beq.lock().unwrap_or_else(|e| e.into_inner());
            let json = device_state_json_rest(&dev, &beq_guard);
            send_json(req, 200, &json)
        })?;
    }

    // GET /api/1/devices/master/levels — direct level snapshot in upstream ezBEQ shape
    {
        let st = device_state.clone();
        let audio = audio_state.clone();
        server.fn_handler("/api/1/devices/master/levels", Method::Get, move |req| {
            let req = match reject_hid_during_playback(req, &audio)? {
                Some(req) => req,
                None => return Ok(()),
            };
            let dev = match get_device(&st) {
                Some(d) => d,
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
            };
            let _op = match dev.try_begin_operation() {
                Some(op) => op,
                None => return send_error(req, 409, crate::http_server::DEVICE_BUSY),
            };
            match dev.get_status_summary() {
                Ok(status) => send_json(req, 200, &levels_json("master", &status)),
                Err(e) => send_error(req, 500, &format!("read_levels: {e}")),
            }
        })?;
    }

    // PUT /api/1/devices/master/filter/{slot} — load a BEQ filter by digest
    for slot_idx in 0..NUM_SLOTS {
        let st = device_state.clone();
        let audio = audio_state.clone();
        let beq = beq_state.clone();
        let nvs = nvs.clone();
        let path = format!("/api/1/devices/master/filter/{}", slot_idx + 1);
        server.fn_handler(&path, Method::Put, move |mut req| {
            req = match reject_hid_during_playback(req, &audio)? {
                Some(req) => req,
                None => return Ok(()),
            };
            let dev = match get_device(&st) {
                Some(d) => d,
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
            };

            // Stock UI body: {"entryId": "<digest>"}; metadata resolves locally.
            let body = match read_request_body(&mut req, 1024) {
                Ok(body) => body,
                Err((status, message)) => return send_beq_error(&beq, req, status, message),
            };
            let json: serde_json::Value = match serde_json::from_slice(&body) {
                Ok(v) => v,
                Err(_) => return send_beq_error(&beq, req, 400, "invalid JSON"),
            };

            let digest = match json.get("entryId").and_then(|v| v.as_str()) {
                Some(id) => id,
                None => return send_beq_error(&beq, req, 400, "missing entryId"),
            };

            let (title, author) = match entry_metadata(digest) {
                Ok(metadata) => metadata,
                Err(e) if e.contains("not found") => {
                    let started = crate::updater::trigger_background_refresh(
                        nvs.clone(),
                        "catalogue entry missing during metadata lookup",
                    );
                    return send_json(
                        req,
                        404,
                        &serde_json::json!({
                            "error": e, "refreshStarted": started,
                        })
                        .to_string(),
                    );
                }
                Err(e) => return send_beq_error(&beq, req, 400, &e),
            };
            if let Err(e) = parse_digest_prefix(digest) {
                return send_beq_error(&beq, req, 400, &e);
            }
            let _op = match dev.try_begin_operation() {
                Some(op) => op,
                None => return send_beq_error(&beq, req, 409, crate::http_server::DEVICE_BUSY),
            };
            if let Err(e) = read_entry_biquads_by_digest(digest, dev.profile().sample_rate) {
                let message = e.to_string();
                if message.contains("not found") {
                    let started = crate::updater::trigger_background_refresh(
                        nvs.clone(),
                        "catalogue entry missing during filter load",
                    );
                    return send_json(
                        req,
                        404,
                        &serde_json::json!({
                            "error": message, "refreshStarted": started,
                        })
                        .to_string(),
                    );
                }
                return send_beq_error(&beq, req, 400, &message);
            }
            if let Err(e) = dev.set_preset(slot_idx as u8) {
                return send_beq_error(&beq, req, 500, &format!("set_preset: {e}"));
            }

            match load_filter(&dev, digest, &title, &author) {
                Ok((_, optimised)) => {
                    dev.save_peq_shadow_to_nvs(&nvs);
                    let mut beq_guard = beq.lock().unwrap_or_else(|e| e.into_inner());
                    set_provenance(&mut beq_guard.slots[slot_idx], optimised);
                    beq_guard.slots[slot_idx].last = title.to_string();
                    beq_guard.slots[slot_idx].author = Some(author.to_string());
                    // Mark this slot as active, all others inactive
                    for (i, slot) in beq_guard.slots.iter_mut().enumerate() {
                        slot.active = i == slot_idx;
                    }
                    #[cfg(esp_idf_httpd_ws_support)]
                    let ws_update = beq_guard
                        .ws_sender
                        .as_ref()
                        .cloned()
                        .map(|sender| (sender, device_state_json(&dev, &beq_guard)));
                    let body = device_state_json_plain(&dev, &beq_guard);
                    drop(beq_guard);
                    #[cfg(esp_idf_httpd_ws_support)]
                    if let Some((sender, json)) = ws_update {
                        broadcast_state(beq.clone(), sender, json);
                    }
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
                        send_beq_error(&beq, req, 500, &e)
                    }
                }
            }
        })?;
    }

    // DELETE /api/1/devices/master/filter/{slot} — clear a BEQ filter
    for slot_idx in 0..NUM_SLOTS {
        let st = device_state.clone();
        let audio = audio_state.clone();
        let beq = beq_state.clone();
        let nvs = nvs.clone();
        let path = format!("/api/1/devices/master/filter/{}", slot_idx + 1);
        server.fn_handler(&path, Method::Delete, move |req| {
            let req = match reject_hid_during_playback(req, &audio)? {
                Some(req) => req,
                None => return Ok(()),
            };
            let dev = match get_device(&st) {
                Some(d) => d,
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
            };
            let _op = match dev.try_begin_operation() {
                Some(op) => op,
                None => return send_beq_error(&beq, req, 409, crate::http_server::DEVICE_BUSY),
            };
            if let Err(e) = dev.set_preset(slot_idx as u8) {
                return send_beq_error(&beq, req, 500, &format!("set_preset: {e}"));
            }

            match clear_filter(&dev) {
                Ok(()) => {
                    dev.save_peq_shadow_to_nvs(&nvs);
                    let mut beq_guard = beq.lock().unwrap_or_else(|e| e.into_inner());
                    beq_guard.slots[slot_idx].last = "Empty".into();
                    beq_guard.slots[slot_idx].author = None;
                    beq_guard.slots[slot_idx].coefficients = None;
                    beq_guard.slots[slot_idx].profile = None;
                    for gain in &mut beq_guard.slots[slot_idx].gains {
                        gain.value = 0.0;
                    }
                    for mute in &mut beq_guard.slots[slot_idx].mutes {
                        mute.value = false;
                    }
                    for (i, slot) in beq_guard.slots.iter_mut().enumerate() {
                        slot.active = i == slot_idx;
                    }
                    #[cfg(esp_idf_httpd_ws_support)]
                    let ws_update = beq_guard
                        .ws_sender
                        .as_ref()
                        .cloned()
                        .map(|sender| (sender, device_state_json(&dev, &beq_guard)));
                    let body = device_state_json_plain(&dev, &beq_guard);
                    drop(beq_guard);
                    #[cfg(esp_idf_httpd_ws_support)]
                    if let Some((sender, json)) = ws_update {
                        broadcast_state(beq.clone(), sender, json);
                    }
                    send_json(req, 200, &body)
                }
                Err(e) => send_beq_error(&beq, req, 500, &e.to_string()),
            }
        })?;
    }

    // PATCH /api/3/devices/master — device control (volume, mute)
    {
        let st = device_state.clone();
        let audio = audio_state.clone();
        let beq = beq_state.clone();
        let nvs = nvs.clone();
        server.fn_handler("/api/3/devices/master", Method::Patch, move |mut req| {
            req = match reject_hid_during_playback(req, &audio)? {
                Some(req) => req,
                None => return Ok(()),
            };
            let dev = match get_device(&st) {
                Some(d) => d,
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
            };
            let body = match read_request_body(&mut req, 4096) {
                Ok(body) => body,
                Err((status, message)) => return send_beq_error(&beq, req, status, message),
            };
            let json: serde_json::Value = match serde_json::from_slice(&body) {
                Ok(v) => v,
                Err(_) => return send_beq_error(&beq, req, 400, "invalid JSON"),
            };
            let _op = match dev.try_begin_operation() {
                Some(op) => op,
                None => return send_beq_error(&beq, req, 409, crate::http_server::DEVICE_BUSY),
            };
            if let Err(e) = validate_patch(&dev, &json) {
                return send_beq_error(&beq, req, 400, &e);
            }
            let mut beq_guard = beq.lock().unwrap_or_else(|e| e.into_inner());

            if let Some(vol) = json.get("masterVolume").and_then(|v| v.as_f64()) {
                if let Err(e) = dev.set_volume_db(vol as f32) {
                    return send_beq_error(&beq, req, 500, &format!("set_volume: {e}"));
                }
            }
            if let Some(mute) = json.get("mute").and_then(|v| v.as_bool()) {
                if let Err(e) = dev.set_mute(mute) {
                    return send_beq_error(&beq, req, 500, &format!("set_mute: {e}"));
                }
            }

            if let Some(slots) = json.get("slots").and_then(|v| v.as_array()) {
                for slot in slots {
                    let slot_id = match slot.get("id").and_then(|v| v.as_str()) {
                        Some(id) => id,
                        None => return send_beq_error(&beq, req, 400, "slot patch missing id"),
                    };
                    let slot_idx = match parse_slot_index(slot_id) {
                        Ok(idx) => idx,
                        Err(e) => return send_beq_error(&beq, req, 400, &e),
                    };

                    let steps = command_text::slot_steps(slot).unwrap(); // validated above
                    if !steps.is_empty() {
                        if let Err(e) = dev.set_preset(slot_idx as u8) {
                            return send_beq_error(&beq, req, 500, &format!("set_preset: {e}"));
                        }
                    }
                    for step in &steps {
                        match step {
                            command_text::SlotStep::Inputs => {
                                let gains = match slot.get("gains") {
                                    Some(value) => {
                                        match serde_json::from_value::<Vec<ChannelValue<f32>>>(
                                            value.clone(),
                                        ) {
                                            Ok(gains) => gains,
                                            Err(_) => {
                                                return send_beq_error(
                                                    &beq,
                                                    req,
                                                    400,
                                                    "invalid slot gains",
                                                )
                                            }
                                        }
                                    }
                                    None => Vec::new(),
                                };
                                let mutes = match slot.get("mutes") {
                                    Some(value) => {
                                        match serde_json::from_value::<Vec<ChannelValue<bool>>>(
                                            value.clone(),
                                        ) {
                                            Ok(mutes) => mutes,
                                            Err(_) => {
                                                return send_beq_error(
                                                    &beq,
                                                    req,
                                                    400,
                                                    "invalid slot mutes",
                                                )
                                            }
                                        }
                                    }
                                    None => Vec::new(),
                                };

                                if let Err(e) = set_input_gain_mute_state(&dev, &gains, &mutes) {
                                    return send_beq_error(&beq, req, 500, &e);
                                }
                                sync_slot_state(
                                    &mut beq_guard.slots[slot_idx],
                                    None,
                                    None,
                                    &gains,
                                    &mutes,
                                );
                            }
                            command_text::SlotStep::Outputs => {
                                let output_gains: Vec<ChannelValue<f32>> = serde_json::from_value(
                                    slot.get("outputGains")
                                        .cloned()
                                        .unwrap_or_else(|| serde_json::json!([])),
                                )
                                .unwrap();
                                let output_mutes: Vec<ChannelValue<bool>> = serde_json::from_value(
                                    slot.get("outputMutes")
                                        .cloned()
                                        .unwrap_or_else(|| serde_json::json!([])),
                                )
                                .unwrap();
                                if let Err(e) =
                                    set_output_gain_mute_state(&dev, &output_gains, &output_mutes)
                                {
                                    return send_beq_error(&beq, req, 500, &e);
                                }
                                for gain in output_gains {
                                    if let Some(ch) = beq_guard.slots[slot_idx]
                                        .output_gains
                                        .iter_mut()
                                        .find(|ch| ch.id == gain.id)
                                    {
                                        ch.value = gain.value;
                                    }
                                }
                                for mute in output_mutes {
                                    if let Some(ch) = beq_guard.slots[slot_idx]
                                        .output_mutes
                                        .iter_mut()
                                        .find(|ch| ch.id == mute.id)
                                    {
                                        ch.value = mute.value;
                                    }
                                }
                            }
                            command_text::SlotStep::Entry => {
                                if let Some(entry) = slot.get("entry") {
                                    let digest = match entry.as_str() {
                                        Some(digest) => digest,
                                        None => {
                                            return send_beq_error(
                                                &beq,
                                                req,
                                                400,
                                                "slot entry must be a string",
                                            )
                                        }
                                    };
                                    if digest.is_empty() {
                                        if let Err(e) = clear_filter(&dev) {
                                            return send_beq_error(&beq, req, 500, &e);
                                        }
                                        beq_guard.slots[slot_idx].last = "Empty".into();
                                        beq_guard.slots[slot_idx].author = None;
                                        beq_guard.slots[slot_idx].coefficients = None;
                                        beq_guard.slots[slot_idx].profile = None;
                                        for gain in &mut beq_guard.slots[slot_idx].gains {
                                            gain.value = 0.0;
                                        }
                                        for mute in &mut beq_guard.slots[slot_idx].mutes {
                                            mute.value = false;
                                        }
                                    } else {
                                        let (title, author) = match entry_metadata(digest) {
                                            Ok(metadata) => metadata,
                                            Err(e) => return send_beq_error(&beq, req, 400, &e),
                                        };
                                        let optimised =
                                            match load_filter(&dev, digest, &title, &author) {
                                                Ok((_, optimised)) => optimised,
                                                Err(e) => {
                                                    return send_beq_error(&beq, req, 400, &e)
                                                }
                                            };
                                        set_provenance(&mut beq_guard.slots[slot_idx], optimised);
                                        beq_guard.slots[slot_idx].last = title.to_string();
                                        beq_guard.slots[slot_idx].author = if author.is_empty() {
                                            None
                                        } else {
                                            Some(author.to_string())
                                        };
                                    }
                                }
                            }
                            // Every mutation above already selects this preset.
                            command_text::SlotStep::Activate => {}
                        }
                    }
                    if !steps.is_empty() {
                        for (i, state_slot) in beq_guard.slots.iter_mut().enumerate() {
                            state_slot.active = i == slot_idx;
                        }
                    }
                }
            }

            dev.save_peq_shadow_to_nvs(&nvs);
            #[cfg(esp_idf_httpd_ws_support)]
            let ws_update = beq_guard
                .ws_sender
                .as_ref()
                .cloned()
                .map(|sender| (sender, device_state_json(&dev, &beq_guard)));
            let body = device_state_json_plain(&dev, &beq_guard);
            drop(beq_guard);
            #[cfg(esp_idf_httpd_ws_support)]
            if let Some((sender, json)) = ws_update {
                broadcast_state(beq.clone(), sender, json);
            }
            send_json(req, 200, &body)
        })?;
    }

    // PUT /commands — upstream coefficient uploads (one-based channels/slot).
    {
        let st = device_state.clone();
        let audio = audio_state.clone();
        let beq = beq_state.clone();
        let nvs = nvs.clone();
        server.fn_handler("/api/1/devices/master/commands", Method::Put, move |mut req| {
            req = match reject_hid_during_playback(req, &audio)? {
                Some(req) => req,
                None => return Ok(()),
            };
            let dev = match get_device(&st) {
                Some(d) => d,
                None => return send_beq_error(&beq, req, 503, "no device"),
            };
            let body = match read_request_body(&mut req, 16 * 1024) {
                Ok(body) => body,
                Err((status, message)) => return send_beq_error(&beq, req, status, message),
            };
            let json: serde_json::Value = match serde_json::from_slice(&body) {
                Ok(v) => v,
                Err(_) => return send_beq_error(&beq, req, 400, "invalid JSON"),
            };
            if json.get("commandType").and_then(|v| v.as_str()) != Some("bq") {
                return send_beq_error(&beq, req, 400, "only commandType 'bq' is supported; export miniDSP biquad coefficient text (filt/rs filter design is unavailable)");
            }
            let Some(text) = json.get("commands").and_then(|v| v.as_str()) else {
                return send_beq_error(&beq, req, 400, "commands must contain coefficient text");
            };
            let biquads = match command_text::parse_biquads(text) {
                Ok(biquads) => biquads,
                Err(e) => return send_beq_error(&beq, req, 400, &e),
            };
            let slot_idx = match json.get("slot").and_then(|v| v.as_str()).ok_or("missing slot")
                .and_then(|id| parse_slot_index(id).map_err(|_| "slot must be 1-4")) {
                Ok(idx) => idx,
                Err(e) => return send_beq_error(&beq, req, 400, e),
            };
            let Some(overwrite) = json.get("overwrite").and_then(|v| v.as_bool()) else {
                return send_beq_error(&beq, req, 400, "overwrite must be a boolean");
            };
            let Some(table) = dev.profile().dsp_table else {
                return send_beq_error(&beq, req, 400, "device has no DSP table");
            };
            let mut targets = Vec::new();
            let mut has_inputs = false;
            for (key, output) in [("inputs", false), ("outputs", true)] {
                let Some(channels) = json.get(key).and_then(|v| v.as_array()) else {
                    return send_beq_error(&beq, req, 400, "inputs and outputs must be channel arrays");
                };
                for ch in channels {
                    let Some(idx) = ch.as_u64().and_then(|ch| ch.checked_sub(1)).and_then(|ch| usize::try_from(ch).ok()) else {
                        return send_beq_error(&beq, req, 400, "channel ids must be positive integers");
                    };
                    let bands = if output { table.outputs.get(idx).map(|ch| ch.peq) }
                                else { table.inputs.get(idx).map(|ch| ch.peq) };
                    let Some(bands) = bands else {
                        return send_beq_error(&beq, req, 400, "channel out of range");
                    };
                    if bands.len() < biquads.len() {
                        return send_beq_error(&beq, req, 400, "too many biquads for selected channel");
                    }
                    targets.push(bands);
                    has_inputs |= !output;
                }
            }
            if targets.is_empty() {
                return send_beq_error(&beq, req, 400, "select at least one input or output channel");
            }
            let _op = match dev.try_begin_operation() {
                Some(op) => op,
                None => return send_beq_error(&beq, req, 409, crate::http_server::DEVICE_BUSY),
            };
            if let Err(e) = dev.set_preset(slot_idx as u8) {
                return send_beq_error(&beq, req, 500, &format!("set_preset: {e}"));
            }
            for bands in targets {
                for action in command_text::band_actions(&biquads, overwrite, bands.len()) {
                    let result = match action {
                        command_text::BandAction::Write { index, coefficients } => dev
                            .write_biquad(bands[index].addr, &coefficients)
                            .and_then(|_| dev.bypass_filter(bands[index].addr, false)),
                        command_text::BandAction::Bypass { index } =>
                            dev.bypass_filter(bands[index].addr, true),
                    };
                    if let Err(e) = result {
                        return send_beq_error(&beq, req, 500, &format!("write biquad: {e}"));
                    }
                }
            }
            dev.save_peq_shadow_to_nvs(&nvs);
            let mut guard = beq.lock().unwrap_or_else(|e| e.into_inner());
            for (idx, slot) in guard.slots.iter_mut().enumerate() { slot.active = idx == slot_idx; }
            if has_inputs {
                let slot = &mut guard.slots[slot_idx];
                slot.last = "CUSTOM".into();
                slot.author = None;
                slot.coefficients = Some("standard".into());
                slot.profile = None;
            }
            let body = device_state_json_plain(&dev, &guard);
            #[cfg(esp_idf_httpd_ws_support)]
            let update = guard.ws_sender.clone().map(|sender| (sender, device_state_json(&dev, &guard)));
            drop(guard);
            #[cfg(esp_idf_httpd_ws_support)]
            if let Some((sender, json)) = update { broadcast_state(beq.clone(), sender, json); }
            send_json(req, 200, &body)
        })?;
    }

    // PUT /api/1/devices/master/config/{slot}/active — activate a slot
    for slot_idx in 0..NUM_SLOTS {
        let st = device_state.clone();
        let audio = audio_state.clone();
        let beq = beq_state.clone();
        let path = format!("/api/1/devices/master/config/{}/active", slot_idx + 1);
        server.fn_handler(&path, Method::Put, move |req| {
            let req = match reject_hid_during_playback(req, &audio)? {
                Some(req) => req,
                None => return Ok(()),
            };
            let dev = match get_device(&st) {
                Some(d) => d,
                None => return send_json(req, 503, r#"{"error":"no device"}"#),
            };
            let _op = match dev.try_begin_operation() {
                Some(op) => op,
                None => return send_beq_error(&beq, req, 409, crate::http_server::DEVICE_BUSY),
            };
            if let Err(e) = dev.set_preset(slot_idx as u8) {
                return send_beq_error(&beq, req, 500, &format!("set_preset: {e}"));
            }
            let mut beq_guard = beq.lock().unwrap_or_else(|e| e.into_inner());
            for (i, slot) in beq_guard.slots.iter_mut().enumerate() {
                slot.active = i == slot_idx;
            }
            #[cfg(esp_idf_httpd_ws_support)]
            let ws_update = beq_guard
                .ws_sender
                .as_ref()
                .cloned()
                .map(|sender| (sender, device_state_json(&dev, &beq_guard)));
            let body = device_state_json_plain(&dev, &beq_guard);
            drop(beq_guard);
            #[cfg(esp_idf_httpd_ws_support)]
            if let Some((sender, json)) = ws_update {
                broadcast_state(beq.clone(), sender, json);
            }
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
    #[derive(serde::Serialize)]
    struct Devices<'a> {
        master: DeviceStateBody<'a>,
    }
    serde_json::to_string(&Devices {
        master: device_state_body(device, beq_state),
    })
    .unwrap_or_default()
}

pub(crate) fn levels_json(name: &str, status: &StatusSummary) -> String {
    let mut levels = serde_json::Map::new();
    for (i, &v) in status.input_levels.iter().enumerate() {
        levels.insert(format!("I{i}"), serde_json::Value::from(v));
    }
    for (i, &v) in status.output_levels.iter().enumerate() {
        levels.insert(format!("O{i}"), serde_json::Value::from(v));
    }

    let ts = (unsafe { esp_idf_sys::esp_timer_get_time() } as f64) / 1_000_000.0;
    serde_json::to_string(&serde_json::json!({
        "name": name,
        "ts": ts,
        "levels": levels,
    }))
    .unwrap_or_default()
}
