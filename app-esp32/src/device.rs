//! Lightweight MiniDSP device client for ESP32.
//!
//! Wraps the USB HID bus and protocol crate into a device-level API.
//! All methods are blocking and thread-safe (via Arc<UsbHidBus>).

use crate::usb_hid::{UsbHidBus, UsbHidError};
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use log::{debug, info, warn};
use minidsp_protocol::commands::{self, HardwareId, Response};
use minidsp_protocol::default_preset::{default_preset_image_for_dsp, DefaultPresetImage};
use minidsp_protocol::device::probe;
use minidsp_protocol::device::DeviceProfile;
use minidsp_protocol::dialect::{Dialect, ValueFormat};
use minidsp_protocol::eeprom;
use minidsp_protocol::source::Source;
use minidsp_protocol::status::*;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy)]
struct PeqShadow {
    coeffs: [f32; 5],
    bypass: bool,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct CachedPeqShadow {
    pub(crate) coeffs: [f32; 5],
    pub(crate) bypass: bool,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct PresetShadowState {
    pub(crate) peq: HashMap<u16, CachedPeqShadow>,
    pub(crate) selector: HashMap<u16, u16>,
    pub(crate) scalar: HashMap<u16, u32>,
    #[serde(default, skip_serializing, skip_deserializing)]
    pub(crate) fir_blocks: HashMap<u16, Vec<f32>>,
}

/// Snapshot of all PEQ bands captured when entering measurement mode.
///
/// The entries are `(addr, coefficients, bypass)` tuples for every PEQ band
/// on inputs and outputs — the same shape as the desktop daemon's
/// `minidsp::client::SavedPeqState` so ezBEQ / Home Assistant clients get
/// identical JSON from either target. See
/// `minidsp/src/client.rs::SavedPeqState` for the upstream definition.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SavedPeqState {
    pub bands: Vec<(u16, [f32; 5], bool)>,
}

/// Device state — holds the USB bus, device profile, and identification info.
pub struct DeviceState {
    bus: Arc<UsbHidBus>,
    profile: DeviceProfile,
    hw_id: HardwareId,
    dsp_version: u8,
    serial: u32,
    operation_lock: Mutex<()>,
    peq_shadow: Mutex<HashMap<u16, PeqShadow>>,
    selector_shadow: Mutex<HashMap<u16, u16>>,
    scalar_shadow: Mutex<HashMap<u16, u32>>,
    fir_shadow: Mutex<HashMap<u16, Vec<f32>>>,
    preset_shadows: Mutex<HashMap<u8, PresetShadowState>>,
    active_preset: Mutex<Option<u8>>,
    trusted_snapshot_presets: Mutex<HashSet<u8>>,
    /// Measurement-mode snapshot — `Some` while PEQ bypass is in effect,
    /// `None` once the client has explicitly restored state.
    saved_peq: Mutex<Option<SavedPeqState>>,
}

fn build_peq_shadow(profile: &DeviceProfile) -> HashMap<u16, PeqShadow> {
    let mut shadow = HashMap::new();
    if let Some(table) = profile.dsp_table {
        for input in table.inputs {
            for band in input.peq {
                shadow.insert(
                    band.addr,
                    PeqShadow {
                        coeffs: minidsp_protocol::biquad::BYPASS,
                        bypass: false,
                    },
                );
            }
        }
        for output in table.outputs {
            for band in output.peq {
                shadow.insert(
                    band.addr,
                    PeqShadow {
                        coeffs: minidsp_protocol::biquad::BYPASS,
                        bypass: false,
                    },
                );
            }
            if let Some(crossover) = output.crossover {
                for base in [crossover.lpf_addr, crossover.hpf_addr] {
                    for i in 0..4u16 {
                        shadow.insert(
                            base + i * 5,
                            PeqShadow {
                                coeffs: minidsp_protocol::biquad::BYPASS,
                                bypass: false,
                            },
                        );
                    }
                }
            }
        }
    }
    shadow
}

/// Timeout for slow commands that write to flash (preset copy, set_config).
const SLOW_COMMAND_TIMEOUT: Duration = Duration::from_secs(35);
/// FIR load end can legitimately take longer than normal flash-style commands.
const FIR_LOAD_END_TIMEOUT: Duration = Duration::from_secs(60);
const GAIN_VERIFY_TOLERANCE_DB: f32 = 0.2;
const FLASH_BLOCK_PRESET_VALUES: u8 = 0x01;
const FLASH_BLOCK_PRESET_BYPASS: u8 = 0x02;
const RECOVERY_MODIFICATION_TOKEN: [u8; 4] = [0xFE, 0xEF, 0x8F, 0xCF];

fn raw_bridge_timeout_for_payload(payload: &[u8]) -> Duration {
    match payload.first().copied() {
        Some(
            commands::CMD_COPY_PRESET
            | commands::CMD_RESET
            | commands::CMD_ENTER_BOOTLOADER
            | commands::CMD_WRITE_FLASH_BLOCK
            | commands::CMD_FINISH_FLASH_BLOCK,
        ) => SLOW_COMMAND_TIMEOUT,
        _ => Duration::from_secs(5),
    }
}

fn build_preset_values_block(params: &[u8]) -> Vec<u8> {
    let total_len = params.len() + 7;
    let size = (total_len as u32).to_be_bytes();
    let mut block = vec![
        (size[1] & 0xF0) | 0x04,
        size[2],
        size[3],
        0x13,
        0x0F,
        0x00,
        0x00,
    ];
    block.extend_from_slice(params);
    block
}

fn build_preset_bypass_block(bypass: &[u8]) -> Vec<u8> {
    let total_len = bypass.len() + 3;
    let size = (total_len as u32).to_be_bytes();
    let mut block = vec![0x05, size[2], size[3]];
    block.extend_from_slice(bypass);
    block
}

fn default_bypass_map(image: &DefaultPresetImage) -> HashMap<u16, bool> {
    let mut map = HashMap::new();
    for chunk in image.bypass.chunks_exact(3) {
        map.insert(
            u16::from_be_bytes([chunk[1], chunk[2]]),
            chunk[0] & 0x80 != 0,
        );
    }
    map
}

fn default_param_word(image: &DefaultPresetImage, addr: u16) -> Option<u32> {
    let offset = addr as usize * 4;
    let bytes: [u8; 4] = image.params.get(offset..offset + 4)?.try_into().ok()?;
    Some(u32::from_le_bytes(bytes))
}

fn default_param_coeffs(image: &DefaultPresetImage, addr: u16) -> Option<[f32; 5]> {
    let mut coeffs = [0.0; 5];
    for (idx, coeff) in coeffs.iter_mut().enumerate() {
        *coeff = f32::from_bits(default_param_word(image, addr + idx as u16)?);
    }
    Some(coeffs)
}

pub(crate) fn default_preset_shadow(
    profile: &DeviceProfile,
    image: &DefaultPresetImage,
) -> PresetShadowState {
    let mut snapshot = PresetShadowState::default();
    let bypass = default_bypass_map(image);
    let Some(table) = profile.dsp_table else {
        return snapshot;
    };

    for input in table.inputs {
        if let Some(mute) = input.mute {
            if let Some(value) = default_param_word(image, mute) {
                snapshot.selector.insert(mute, value as u16);
            }
        }
        if let Some(gain) = input.gain {
            if let Some(value) = default_param_word(image, gain) {
                snapshot.scalar.insert(gain, value);
            }
        }
        if let Some(polarity) = input.polarity {
            if let Some(value) = default_param_word(image, polarity) {
                snapshot.selector.insert(polarity, value as u16);
            }
        }
        for band in input.peq {
            if let Some(coeffs) = default_param_coeffs(image, band.addr) {
                snapshot.peq.insert(
                    band.addr,
                    CachedPeqShadow {
                        coeffs,
                        bypass: bypass.get(&band.addr).copied().unwrap_or(false),
                    },
                );
            }
        }
        if let Some(fir) = input.fir {
            if let Some(value) = default_param_word(image, fir.taps_count) {
                snapshot.selector.insert(fir.taps_count, value as u16);
            }
        }
    }

    for output in table.outputs {
        if let Some(value) = default_param_word(image, output.mute) {
            snapshot.selector.insert(output.mute, value as u16);
        }
        if let Some(value) = default_param_word(image, output.gain) {
            snapshot.scalar.insert(output.gain, value);
        }
        if let Some(delay) = output.delay {
            if let Some(value) = default_param_word(image, delay) {
                snapshot.scalar.insert(delay, value);
            }
        }
        if let Some(polarity) = output.polarity {
            if let Some(value) = default_param_word(image, polarity) {
                snapshot.selector.insert(polarity, value as u16);
            }
        }
        for band in output.peq {
            if let Some(coeffs) = default_param_coeffs(image, band.addr) {
                snapshot.peq.insert(
                    band.addr,
                    CachedPeqShadow {
                        coeffs,
                        bypass: bypass.get(&band.addr).copied().unwrap_or(false),
                    },
                );
            }
        }
        if let Some(fir) = output.fir {
            if let Some(value) = default_param_word(image, fir.bypass) {
                snapshot.selector.insert(fir.bypass, value as u16);
            }
            if let Some(value) = default_param_word(image, fir.taps_count) {
                snapshot.selector.insert(fir.taps_count, value as u16);
            }
            if let Some(block) = default_fir_block(image, &snapshot, fir) {
                snapshot
                    .fir_blocks
                    .insert(block.data_start, block.coefficients);
            }
        }
        if let Some(crossover) = output.crossover {
            for base in [crossover.lpf_addr, crossover.hpf_addr] {
                for idx in 0..4u16 {
                    let addr = base + idx * 5;
                    if let Some(coeffs) = default_param_coeffs(image, addr) {
                        snapshot.peq.insert(
                            addr,
                            CachedPeqShadow {
                                coeffs,
                                bypass: bypass.get(&addr).copied().unwrap_or(false),
                            },
                        );
                    }
                }
            }
        }
        if let Some(comp) = output.compressor {
            if let Some(value) = default_param_word(image, comp.bypass) {
                snapshot.selector.insert(comp.bypass, value as u16);
            }
            for addr in [
                comp.threshold,
                comp.gain,
                comp.ratio,
                comp.knee,
                comp.attack,
                comp.release,
            ] {
                if let Some(value) = default_param_word(image, addr) {
                    snapshot.scalar.insert(addr, value);
                }
            }
        }
    }

    let routing_span = table.routing.rows as u16 * table.routing.cols as u16;
    for offset in 0..routing_span {
        let status_addr = table.routing.status_base + offset;
        if let Some(value) = default_param_word(image, status_addr) {
            snapshot.selector.insert(status_addr, value as u16);
        }
        let gain_addr = table.routing.gain_base + offset;
        if let Some(value) = default_param_word(image, gain_addr) {
            snapshot.scalar.insert(gain_addr, value);
        }
        if let Some(polarity_base) = table.routing.polarity_base {
            let polarity_addr = polarity_base + offset;
            if let Some(value) = default_param_word(image, polarity_addr) {
                snapshot.selector.insert(polarity_addr, value as u16);
            }
        }
    }

    snapshot
}

fn default_fir_block(
    image: &DefaultPresetImage,
    shadow: &PresetShadowState,
    fir: minidsp_protocol::device::dsp::FirFilter,
) -> Option<crate::snapshot::SnapshotFirBlock> {
    let taps = shadow.selector.get(&fir.taps_count).copied().unwrap_or(0) as usize;
    if taps == 0 {
        return None;
    }

    let mut coefficients = Vec::with_capacity(taps);
    for offset in 0..taps {
        coefficients.push(f32::from_bits(default_param_word(
            image,
            fir.data_start + offset as u16,
        )?));
    }
    Some(crate::snapshot::SnapshotFirBlock {
        data_start: fir.data_start,
        coefficients,
    })
}

fn random_token_bytes() -> Result<[u8; 4], UsbHidError> {
    let mut token = [0u8; 4];
    unsafe {
        esp_idf_sys::esp_fill_random(token.as_mut_ptr().cast(), token.len());
    }
    if token == [0u8; 4] {
        return Err(UsbHidError::Protocol(
            "failed to generate nonzero modification token".into(),
        ));
    }
    Ok(token)
}

impl DeviceState {
    const BM_NUM_CHANNELS: u8 = 8;

    fn expect_write_ok_with_timeout(
        &self,
        payload: &[u8],
        timeout: Duration,
        context: &str,
    ) -> Result<(), UsbHidError> {
        let resp = self.bus.command_with_timeout(payload, timeout)?;
        match commands::parse_response_for_request(payload, &resp, self.dialect()) {
            Response::Ack | Response::PresetComplete | Response::ResetComplete => Ok(()),
            Response::Error(data) => Err(UsbHidError::Protocol(format!(
                "{context}: device error {data:02x?}"
            ))),
            other => Err(UsbHidError::Protocol(format!(
                "{context}: unexpected response {other:?}"
            ))),
        }
    }

    fn expect_write_ok(&self, payload: &[u8], context: &str) -> Result<(), UsbHidError> {
        let resp = self.bus.command(payload)?;
        match commands::parse_response_for_request(payload, &resp, self.dialect()) {
            Response::Ack | Response::PresetComplete | Response::ResetComplete => Ok(()),
            Response::Error(data) => Err(UsbHidError::Protocol(format!(
                "{context}: device error {data:02x?}"
            ))),
            other => Err(UsbHidError::Protocol(format!(
                "{context}: unexpected response {other:?}"
            ))),
        }
    }

    fn expect_write_ok_slow(&self, payload: &[u8], context: &str) -> Result<(), UsbHidError> {
        self.expect_write_ok_with_timeout(payload, SLOW_COMMAND_TIMEOUT, context)
    }

    fn send_reboot_command(&self, payload: &[u8], context: &str) -> Result<(), UsbHidError> {
        const SEND_TIMEOUT: Duration = Duration::from_secs(2);

        match self.bus.command_with_timeout(payload, SEND_TIMEOUT) {
            Ok(resp) => {
                match commands::parse_response_for_request(payload, &resp, self.dialect()) {
                    Response::Ack | Response::ResetComplete => Ok(()),
                    Response::Error(data) => Err(UsbHidError::Protocol(format!(
                        "{context}: device error {data:02x?}"
                    ))),
                    other => Err(UsbHidError::Protocol(format!(
                        "{context}: unexpected response {other:?}"
                    ))),
                }
            }
            Err(UsbHidError::Timeout)
            | Err(UsbHidError::Disconnected)
            | Err(UsbHidError::Transfer(_)) => {
                debug!(
                    "{context}: device dropped transport during reboot, waiting for physical USB disconnect"
                );
                Ok(())
            }
            Err(err) => Err(err),
        }
    }

    pub fn probe_with_timeout(bus: Arc<UsbHidBus>, timeout: Duration) -> Result<Self, UsbHidError> {
        // Bare ReadHardwareId is the proven-good probe used by the last known
        // stable ESP32 build. Sending extra probe bytes here can leave the HID
        // endpoint in recovery before we ever identify the device.
        let resp = bus.command_with_timeout(&commands::read_hardware_id(), timeout)?;
        let parsed = commands::parse_response_for_request(
            &commands::read_hardware_id(),
            &resp,
            &Dialect::NEW,
        );
        let hw_id = match parsed {
            Response::HardwareId(id) => id,
            other => {
                return Err(UsbHidError::Protocol(format!(
                    "expected HardwareId, got {other:?}"
                )))
            }
        };
        debug!(
            "hw_id: fw={}.{}, hw={}",
            hw_id.fw_major, hw_id.fw_minor, hw_id.hw_id
        );

        // Read DSP version from EEPROM 0xFFA1
        let (addr_hi, addr_lo) = eeprom::DSP_VERSION_ADDR;
        let req = commands::read_flash(addr_hi, addr_lo, 1);
        let resp = bus.command_with_timeout(&req, timeout)?;
        debug!(
            "dsp_version raw resp ({} bytes): {:02x?}",
            resp.len(),
            &resp[..resp.len().min(10)]
        );
        let parsed = commands::parse_response_for_request(&req, &resp, &Dialect::NEW);
        let dsp_version = match parsed {
            Response::FlashData(ref data) => {
                debug!(
                    "dsp_version FlashData ({} bytes): {:02x?}",
                    data.len(),
                    data
                );
                eeprom::parse_dsp_version(data).unwrap_or(0)
            }
            ref other => {
                debug!("dsp_version response: {other:?}");
                0
            }
        };
        debug!("dsp_version: {dsp_version}");

        // Read serial from EEPROM 0xFFFE
        let (addr_hi, addr_lo) = eeprom::SERIAL_ADDR;
        let req = commands::read_flash(addr_hi, addr_lo, 2);
        let resp = bus.command_with_timeout(&req, timeout)?;
        let parsed = commands::parse_response_for_request(&req, &resp, &Dialect::NEW);
        let serial = match parsed {
            Response::FlashData(data) => eeprom::parse_serial(&data).unwrap_or(0),
            _ => 0,
        };
        debug!("serial: {serial}");

        // Identify device
        let profile = probe::identify(hw_id.hw_id, dsp_version);
        let peq_shadow = build_peq_shadow(&profile);

        let state = DeviceState {
            bus,
            profile,
            hw_id,
            dsp_version,
            serial,
            operation_lock: Mutex::new(()),
            peq_shadow: Mutex::new(peq_shadow),
            selector_shadow: Mutex::new(HashMap::new()),
            scalar_shadow: Mutex::new(HashMap::new()),
            fir_shadow: Mutex::new(HashMap::new()),
            preset_shadows: Mutex::new(HashMap::new()),
            active_preset: Mutex::new(None),
            trusted_snapshot_presets: Mutex::new(HashSet::new()),
            saved_peq: Mutex::new(None),
        };

        // Seed the active preset cache early so preset-scoped shadow state
        // does not get wiped the first time status is polled after boot.
        let _ = state.get_master_status();

        Ok(state)
    }

    pub fn try_begin_operation(&self) -> Option<std::sync::MutexGuard<'_, ()>> {
        match self.operation_lock.try_lock() {
            Ok(guard) => Some(guard),
            Err(std::sync::TryLockError::Poisoned(err)) => Some(err.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => None,
        }
    }

    // -- Accessors --

    pub fn hw_id(&self) -> &HardwareId {
        &self.hw_id
    }
    pub fn dsp_version(&self) -> u8 {
        self.dsp_version
    }
    pub fn serial(&self) -> u32 {
        self.serial
    }
    pub fn profile(&self) -> &DeviceProfile {
        &self.profile
    }
    pub fn product_name(&self) -> &str {
        probe::product_name(self.hw_id.hw_id, self.dsp_version)
    }

    pub fn current_source(&self) -> Result<Option<Source>, UsbHidError> {
        let status = self.get_master_status()?;
        Ok(self.profile.sources.source_for_index(status.source))
    }
    pub(crate) fn dialect(&self) -> &Dialect {
        &self.profile.dialect
    }

    fn capture_active_shadow(&self) -> PresetShadowState {
        let peq = self
            .peq_shadow
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(&addr, entry)| {
                (
                    addr,
                    CachedPeqShadow {
                        coeffs: entry.coeffs,
                        bypass: entry.bypass,
                    },
                )
            })
            .collect();
        let selector = self
            .selector_shadow
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let scalar = self
            .scalar_shadow
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let fir_blocks = self
            .fir_shadow
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        PresetShadowState {
            peq,
            selector,
            scalar,
            fir_blocks,
        }
    }

    fn apply_active_shadow_snapshot(&self, snapshot: Option<PresetShadowState>) {
        {
            let mut peq_shadow = self.peq_shadow.lock().unwrap_or_else(|e| e.into_inner());
            *peq_shadow = build_peq_shadow(&self.profile);
            if let Some(snapshot) = &snapshot {
                for (&addr, entry) in &snapshot.peq {
                    peq_shadow.insert(
                        addr,
                        PeqShadow {
                            coeffs: entry.coeffs,
                            bypass: entry.bypass,
                        },
                    );
                }
            }
        }
        {
            let mut selector_shadow = self
                .selector_shadow
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            selector_shadow.clear();
            if let Some(snapshot) = &snapshot {
                selector_shadow.extend(
                    snapshot
                        .selector
                        .iter()
                        .map(|(&addr, &value)| (addr, value)),
                );
            }
        }
        {
            let mut scalar_shadow = self.scalar_shadow.lock().unwrap_or_else(|e| e.into_inner());
            scalar_shadow.clear();
            if let Some(snapshot) = &snapshot {
                scalar_shadow.extend(snapshot.scalar.iter().map(|(&addr, &value)| (addr, value)));
            }
        }
        {
            let mut fir_shadow = self.fir_shadow.lock().unwrap_or_else(|e| e.into_inner());
            fir_shadow.clear();
            if let Some(snapshot) = &snapshot {
                fir_shadow.extend(
                    snapshot
                        .fir_blocks
                        .iter()
                        .map(|(&data_start, coeffs)| (data_start, coeffs.clone())),
                );
            }
        }
    }

    fn update_active_preset(&self, preset: u8) {
        let previous = {
            let mut active_preset = self.active_preset.lock().unwrap_or_else(|e| e.into_inner());
            if *active_preset == Some(preset) {
                return;
            }
            let previous = *active_preset;
            *active_preset = Some(preset);
            previous
        };

        if let Some(previous) = previous {
            let snapshot = self.capture_active_shadow();
            let mut preset_shadows = self.preset_shadows.lock().unwrap_or_else(|e| e.into_inner());
            let trusted = self
                .trusted_snapshot_presets
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&previous);
            let snapshot = if trusted {
                preset_shadows
                    .get(&previous)
                    .cloned()
                    .map(|existing| merge_trusted_snapshot(existing, snapshot.clone()))
                    .unwrap_or(snapshot)
            } else {
                snapshot
            };
            preset_shadows.insert(previous, snapshot);
        }

        let snapshot = self
            .preset_shadows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&preset)
            .cloned();
        self.apply_active_shadow_snapshot(snapshot);
    }

    pub(crate) fn store_preset_shadow_snapshot(&self, preset: u8, snapshot: PresetShadowState) {
        self.preset_shadows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(preset, snapshot.clone());
        if *self.active_preset.lock().unwrap_or_else(|e| e.into_inner()) == Some(preset) {
            self.apply_active_shadow_snapshot(Some(snapshot));
        }
    }

    pub(crate) fn mark_snapshot_trusted(&self, preset: u8) {
        self.trusted_snapshot_presets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(preset);
    }

    pub(crate) fn trusted_snapshot_slots(&self) -> HashSet<u8> {
        self.trusted_snapshot_presets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub(crate) fn export_shadow_snapshots(&self) -> HashMap<u8, PresetShadowState> {
        let active_preset = *self.active_preset.lock().unwrap_or_else(|e| e.into_inner());
        let mut presets = self
            .preset_shadows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(active_preset) = active_preset {
            presets
                .entry(active_preset)
                .or_insert_with(|| self.capture_active_shadow());
        }
        presets
    }

    fn prefers_scalar_shadow_for_addr(&self, addr: u16) -> bool {
        if !self.profile.has_unreliable_selector_readback {
            return false;
        }
        let Some(table) = self.profile.dsp_table else {
            return false;
        };
        if table
            .inputs
            .iter()
            .filter_map(|input| input.gain)
            .any(|gain| gain == addr)
        {
            return true;
        }

        if table.outputs.iter().any(|output| {
            output.gain == addr
                || output.delay == Some(addr)
                || output.compressor.is_some_and(|comp| {
                    [
                        comp.threshold,
                        comp.gain,
                        comp.ratio,
                        comp.knee,
                        comp.attack,
                        comp.release,
                    ]
                    .contains(&addr)
                })
        }) {
            return true;
        }

        let routing_span = table.routing.rows as u16 * table.routing.cols as u16;
        addr >= table.routing.gain_base && addr < table.routing.gain_base + routing_span
    }

    // -- Master status --

    pub fn get_master_status(&self) -> Result<eeprom::MasterStatusRaw, UsbHidError> {
        const MASTER_STATUS_RETRIES: usize = 6;
        const MASTER_STATUS_RETRY_DELAY: Duration = Duration::from_millis(100);

        let mut last_err = None;
        for attempt in 0..MASTER_STATUS_RETRIES {
            match self.read_master_status_once() {
                Ok(status) => return Ok(status),
                Err(UsbHidError::Protocol(err)) => {
                    last_err = Some(err);
                    if attempt + 1 < MASTER_STATUS_RETRIES {
                        std::thread::sleep(MASTER_STATUS_RETRY_DELAY);
                    }
                }
                Err(err) => return Err(err),
            }
        }

        Err(UsbHidError::Protocol(
            last_err.unwrap_or_else(|| "master status retry exhausted".into()),
        ))
    }

    fn read_master_status_once(&self) -> Result<eeprom::MasterStatusRaw, UsbHidError> {
        let (hi, lo) = eeprom::MASTER_STATUS_ADDR;
        let req = commands::read_flash(hi, lo, 4);
        let resp = self.bus.command(&req)?;
        match commands::parse_response_for_request(&req, &resp, self.dialect()) {
            Response::FlashData(data) if data.len() >= 4 => {
                let master = eeprom::MasterStatusRaw::from_bytes(&data)
                    .ok_or_else(|| UsbHidError::Protocol("bad master status".into()))?;
                self.update_active_preset(master.preset);
                Ok(master)
            }
            other => Err(UsbHidError::Protocol(format!("master status: {other:?}"))),
        }
    }

    pub fn get_status_summary(&self) -> Result<StatusSummary, UsbHidError> {
        let ms = self.get_master_status()?;
        let source_name = self
            .profile
            .sources
            .source_for_index(ms.source)
            .map(|s| s.pascal_case().to_string())
            .unwrap_or_else(|| format!("Source{}", ms.source));

        let dirac = if self.profile.has_dirac {
            self.read_dirac_state().ok()
        } else {
            None
        };

        // Read level meters if DSP table is available
        let (input_levels, output_levels) = if let Some(table) = self.profile.dsp_table {
            let il = match self.read_meters(table.meters.input) {
                Ok(values) => values,
                Err(e) => {
                    warn!("input meter read failed: {e}");
                    vec![]
                }
            };
            let ol = match self.read_meters(table.meters.output) {
                Ok(values) => values,
                Err(e) => {
                    warn!("output meter read failed: {e}");
                    vec![]
                }
            };
            (il, ol)
        } else {
            (vec![], vec![])
        };

        Ok(StatusSummary {
            master: MasterStatusView {
                preset: ms.preset,
                source: source_name,
                volume: ms.volume_db(),
                mute: ms.is_muted(),
                dirac,
            },
            input_levels,
            output_levels,
        })
    }

    pub fn read_device_config(&self) -> Result<DeviceConfig, UsbHidError> {
        let table = self
            .profile
            .dsp_table
            .ok_or_else(|| UsbHidError::Protocol("config read unsupported".into()))?;

        let master_raw = self.get_master_status()?;
        let source_name = self
            .profile
            .sources
            .source_for_index(master_raw.source)
            .map(|s| s.pascal_case().to_string())
            .unwrap_or_else(|| format!("Unknown({})", master_raw.source));
        let dirac = if self.profile.has_dirac {
            self.read_dirac_state().ok()
        } else {
            None
        };

        let mut inputs = Vec::new();
        for (ch_idx, input) in table.inputs.iter().enumerate() {
            let mut peq = Vec::new();
            for (band_idx, band) in input.peq.iter().enumerate() {
                match self.read_peq_coeffs(band.addr) {
                    Ok(coeffs) => {
                        peq.push(PeqBandConfig {
                            index: band_idx as u8,
                            coeff: BiquadCoeffs::from_array(&coeffs),
                            bypass: self.read_peq_bypass(band.addr).unwrap_or(false),
                        });
                    }
                    _ => {}
                }
            }

            let gain = input
                .gain
                .and_then(|addr| self.read_config_gain_db(addr).ok());
            let mute = input.mute.and_then(|addr| self.read_config_mute(addr).ok());
            let mut routing_entries = Vec::new();
            let matrix = table.routing;
            if ch_idx < matrix.rows as usize {
                for output in 0..matrix.cols {
                    let flat_idx = ch_idx as u16 * matrix.cols as u16 + output as u16;
                    let status_addr = matrix.status_base + flat_idx;
                    let gain_addr = matrix.gain_base + flat_idx;
                    let enabled = match self.profile.dialect.value_format {
                        ValueFormat::Float32 => {
                            self.read_routing_enabled(status_addr).unwrap_or(false)
                        }
                        ValueFormat::FixedPoint => {
                            self.read_param_word(status_addr).unwrap_or(0) != 0
                        }
                    };
                    let gain_db = self.read_config_gain_db(gain_addr).unwrap_or(0.0);
                    let polarity_inverted = matrix
                        .polarity_base
                        .and_then(|base| self.read_channel_invert(base + flat_idx).ok());
                    routing_entries.push(RoutingConfig {
                        output,
                        enabled,
                        gain_db,
                        polarity_inverted,
                    });
                }
            }

            inputs.push(InputChannelConfig {
                index: ch_idx as u8,
                peq,
                gain,
                mute,
                routing: routing_entries,
            });
        }

        let mut outputs = Vec::new();
        for (ch_idx, output) in table.outputs.iter().enumerate() {
            let mut peq = Vec::new();
            for (band_idx, band) in output.peq.iter().enumerate() {
                match self.read_peq_coeffs(band.addr) {
                    Ok(coeffs) => {
                        peq.push(PeqBandConfig {
                            index: band_idx as u8,
                            coeff: BiquadCoeffs::from_array(&coeffs),
                            bypass: self.read_peq_bypass(band.addr).unwrap_or(false),
                        });
                    }
                    _ => {}
                }
            }

            let mut crossover = Vec::new();
            if let Some(xover) = output.crossover {
                for (group_idx, base_addr) in [(0u8, xover.lpf_addr), (1u8, xover.hpf_addr)] {
                    let mut filters = Vec::new();
                    for band_idx in 0..4u8 {
                        let addr = base_addr + band_idx as u16 * 5;
                        if let Ok(coeffs) = self.read_peq_coeffs(addr) {
                            filters.push(IndexedBiquadConfig {
                                index: band_idx,
                                coeff: BiquadCoeffs::from_array(&coeffs),
                            });
                        }
                    }
                    crossover.push(CrossoverGroupConfig {
                        index: group_idx,
                        filters,
                        bypass: self.read_peq_bypass(base_addr).unwrap_or(false),
                    });
                }
            }

            let fir = if let Some(fir) = output.fir {
                Some(FirConfig {
                    bypass: self.read_config_bypass(fir.bypass).unwrap_or(false),
                    active_taps: self.read_fir_tap_count(fir.index).unwrap_or(0),
                })
            } else {
                None
            };

            let compressor = if let Some(comp) = &output.compressor {
                Some(CompressorConfig {
                    bypass: self.read_config_bypass(comp.bypass).unwrap_or(false),
                    threshold: self.read_config_gain_db(comp.threshold).unwrap_or(0.0),
                    gain: self.read_config_gain_db(comp.gain).unwrap_or(0.0),
                    ratio: self.read_config_gain_db(comp.ratio).unwrap_or(0.0),
                    knee: self.read_config_gain_db(comp.knee).unwrap_or(0.0),
                    attack: self.read_config_gain_db(comp.attack).unwrap_or(0.0),
                    release: self.read_config_gain_db(comp.release).unwrap_or(0.0),
                })
            } else {
                None
            };

            let delay = if let Some(delay_addr) = output.delay {
                self.read_config_delay(delay_addr)
                    .map(|delay| delay.as_secs_f32() * 1000.0)
                    .unwrap_or(0.0)
            } else {
                0.0
            };

            outputs.push(OutputChannelConfig {
                index: ch_idx as u8,
                gain: self.read_config_gain_db(output.gain).unwrap_or(0.0),
                mute: self.read_config_mute(output.mute).unwrap_or(false),
                delay,
                peq,
                crossover,
                fir,
                compressor,
            });
        }

        Ok(DeviceConfig {
            master_status: MasterStatusView {
                preset: master_raw.preset,
                source: source_name,
                volume: master_raw.volume_db(),
                mute: master_raw.is_muted(),
                dirac,
            },
            inputs,
            outputs,
        })
    }

    /// Read float values from DSP memory starting at `addr`.
    fn read_floats(&self, addr: u16, count: u8) -> Result<Vec<f32>, UsbHidError> {
        let req = commands::read_floats(addr, count, self.dialect());
        let resp = self.bus.command(&req)?;
        match commands::parse_response_for_request(&req, &resp, self.dialect()) {
            Response::Floats(values) => Ok(values),
            other => Err(UsbHidError::Protocol(format!("read_floats: {other:?}"))),
        }
    }

    pub fn read_dsp_floats(&self, addr: u16, count: u8) -> Result<Vec<f32>, UsbHidError> {
        self.read_floats(addr, count)
    }

    fn read_param_words(&self, addr: u16, count: u8) -> Result<Vec<u32>, UsbHidError> {
        let req = commands::read_floats(addr, count, self.dialect());
        let resp = self.bus.command(&req)?;
        commands::parse_param_words_response_for_request(&req, &resp)
            .ok_or_else(|| UsbHidError::Protocol("typed read_floats parse failed".into()))
    }

    fn read_param_words_be(&self, addr: u16, count: u8) -> Result<Vec<u32>, UsbHidError> {
        let req = commands::read_floats(addr, count, self.dialect());
        let resp = self.bus.command(&req)?;
        commands::parse_param_words_be_response_for_request(&req, &resp).ok_or_else(|| {
            UsbHidError::Protocol("big-endian typed read_floats parse failed".into())
        })
    }

    pub fn read_dsp_words(&self, addr: u16, count: u8) -> Result<Vec<u32>, UsbHidError> {
        self.read_param_words(addr, count)
    }

    fn read_param_word(&self, addr: u16) -> Result<u32, UsbHidError> {
        self.read_param_words(addr, 1)?
            .into_iter()
            .next()
            .ok_or_else(|| UsbHidError::Protocol("empty typed param response".into()))
    }

    fn read_param_scalar_u32(&self, addr: u16) -> Result<u32, UsbHidError> {
        match self.profile.dialect.value_format {
            ValueFormat::Float32 => self.read_param_word(addr),
            ValueFormat::FixedPoint => self
                .read_param_words_be(addr, 1)?
                .into_iter()
                .next()
                .ok_or_else(|| UsbHidError::Protocol("empty integer param response".into())),
        }
    }

    fn read_peq_coeffs(&self, addr: u16) -> Result<[f32; 5], UsbHidError> {
        // On devices with unreliable selector readback (DSP100), always prefer
        // shadow state — live reads can return collapsed/bogus values.
        if self.profile.has_unreliable_selector_readback {
            return Ok(self
                .peq_shadow
                .lock()
                .map_err(|_| UsbHidError::Protocol("peq shadow poisoned".into()))?
                .get(&addr)
                .map(|s| s.coeffs)
                .unwrap_or(minidsp_protocol::biquad::BYPASS));
        }

        let values = self.read_floats(addr, 5).unwrap_or_default();
        if values.len() >= 5 {
            let coeffs = [values[0], values[1], values[2], values[3], values[4]];
            if coeffs != [0.0; 5] {
                return Ok(coeffs);
            }
        }
        // Fallback to shadow for any device if live read returned all zeros
        Ok(self
            .peq_shadow
            .lock()
            .map_err(|_| UsbHidError::Protocol("peq shadow poisoned".into()))?
            .get(&addr)
            .map(|s| s.coeffs)
            .unwrap_or(minidsp_protocol::biquad::BYPASS))
    }

    fn read_peq_bypass(&self, addr: u16) -> Option<bool> {
        self.peq_shadow.lock().ok()?.get(&addr).map(|s| s.bypass)
    }

    fn read_config_gain_db(&self, addr: u16) -> Result<f32, UsbHidError> {
        match self.profile.dialect.value_format {
            ValueFormat::Float32 => {
                if self.prefers_scalar_shadow_for_addr(addr) {
                    if let Some(raw) = self
                        .scalar_shadow
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .get(&addr)
                        .copied()
                    {
                        return Ok(f32::from_bits(raw));
                    }
                }
                let raw = self.read_param_word(addr)?;
                if self.prefers_scalar_shadow_for_addr(addr) {
                    if let Some(value) = sanitize_shadow_preferred_scalar(raw) {
                        return Ok(value);
                    }
                }
                Ok(f32::from_bits(raw))
            }
            ValueFormat::FixedPoint => {
                let linear = self
                    .read_floats(addr, 1)?
                    .into_iter()
                    .next()
                    .ok_or_else(|| UsbHidError::Protocol("empty gain response".into()))?;
                Ok(20.0 * linear.max(1e-10).log10())
            }
        }
    }

    fn read_config_mute(&self, addr: u16) -> Result<bool, UsbHidError> {
        match self.profile.dialect.value_format {
            ValueFormat::Float32 => Ok(self.read_selector_word(addr)? == 1),
            ValueFormat::FixedPoint => {
                let value = self
                    .read_floats(addr, 1)?
                    .into_iter()
                    .next()
                    .ok_or_else(|| UsbHidError::Protocol("empty mute response".into()))?;
                Ok(value == 0.0)
            }
        }
    }

    fn read_config_bypass(&self, addr: u16) -> Result<bool, UsbHidError> {
        match self.profile.dialect.value_format {
            ValueFormat::Float32 => Ok(matches!(self.read_selector_word(addr)?, 1 | 3)),
            ValueFormat::FixedPoint => self.read_config_mute(addr),
        }
    }

    fn read_config_delay(&self, addr: u16) -> Result<std::time::Duration, UsbHidError> {
        let samples = match self.profile.dialect.value_format {
            ValueFormat::Float32 => {
                if self.prefers_scalar_shadow_for_addr(addr) {
                    if let Some(raw) = self
                        .scalar_shadow
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .get(&addr)
                        .copied()
                    {
                        raw as f32
                    } else {
                        let raw = self.read_param_word(addr)?;
                        if raw <= 2 {
                            0.0
                        } else {
                            raw as f32
                        }
                    }
                } else {
                    self.read_param_word(addr)? as f32
                }
            }
            ValueFormat::FixedPoint => self
                .read_param_words_be(addr, 1)?
                .into_iter()
                .next()
                .ok_or_else(|| UsbHidError::Protocol("empty delay response".into()))?
                as f32,
        };
        Ok(std::time::Duration::from_secs_f32(
            samples / self.profile.sample_rate as f32,
        ))
    }

    fn read_config_invert(&self, addr: u16) -> Result<bool, UsbHidError> {
        match self.profile.dialect.value_format {
            ValueFormat::Float32 => Ok(self.read_param_word(addr)? != 0),
            ValueFormat::FixedPoint => {
                let value = self
                    .read_floats(addr, 1)?
                    .into_iter()
                    .next()
                    .ok_or_else(|| UsbHidError::Protocol("empty invert response".into()))?;
                Ok(value < 0.0)
            }
        }
    }

    pub fn read_channel_invert(&self, addr: u16) -> Result<bool, UsbHidError> {
        self.read_config_invert(addr)
    }

    fn read_selector_word(&self, addr: u16) -> Result<u16, UsbHidError> {
        let raw = self.read_param_word(addr)? as u16;
        if raw == 1 {
            if let Some(&shadow) = self
                .selector_shadow
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&addr)
            {
                if shadow != 1 {
                    return Ok(shadow);
                }
            }
        }
        Ok(raw)
    }

    fn selector_shadow_value(&self, addr: u16) -> Option<u16> {
        self.selector_shadow
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&addr)
            .copied()
    }

    fn read_routing_enabled(&self, addr: u16) -> Result<bool, UsbHidError> {
        if let Some(shadow) = self.selector_shadow_value(addr) {
            return Ok(shadow == 2);
        }
        Ok(self.read_param_word(addr)? != 0)
    }

    pub fn read_fir_tap_count(&self, fir_index: u8) -> Result<u16, UsbHidError> {
        let req = commands::get_num_fir_taps(fir_index);
        let resp = self.bus.command(&req)?;
        match commands::parse_response_for_request(&req, &resp, self.dialect()) {
            Response::FirLoadInfo { max_coeffs } => Ok(max_coeffs),
            Response::Error(data) => Err(UsbHidError::Protocol(format!(
                "get_num_fir_taps({fir_index}): device error {data:02x?}"
            ))),
            other => Err(UsbHidError::Protocol(format!(
                "get_num_fir_taps({fir_index}): unexpected response {other:?}"
            ))),
        }
    }

    /// Read meter values from an array of DSP addresses and convert to dB.
    /// Batches contiguous addresses into single CMD_READ_FLOATS calls (max 14 per call).
    fn read_meters(&self, addrs: &[u16]) -> Result<Vec<f32>, UsbHidError> {
        if addrs.is_empty() {
            return Ok(vec![]);
        }

        let mut all_values = Vec::with_capacity(addrs.len());

        // Check if addresses are contiguous for a single bulk read
        let is_contiguous = addrs.len() <= 14 && addrs.windows(2).all(|w| w[1] == w[0] + 1);

        if is_contiguous {
            let values = self.read_floats(addrs[0], addrs.len() as u8)?;
            all_values.extend(values);
        } else {
            // Find contiguous runs and read each in bulk
            let mut i = 0;
            while i < addrs.len() {
                let start = i;
                while i + 1 < addrs.len() && addrs[i + 1] == addrs[i] + 1 && (i - start + 1) < 14 {
                    i += 1;
                }
                let count = (i - start + 1).min(14);
                let values = self.read_floats(addrs[start], count as u8)?;
                all_values.extend(values);
                i += 1;
            }
        }

        // SHARC-family meter registers already report dB-like float values.
        // Re-applying log10() here turns valid negative readings like -120 dB
        // into misleading positive numbers (~41.58). Return the raw meter
        // values and only clamp pathological silence / NaN cases.
        let db_values: Vec<f32> = all_values
            .iter()
            .map(|&v| {
                if !v.is_finite() {
                    -140.0
                } else if v <= -140.0 || v.abs() < 1e-10 {
                    -140.0
                } else {
                    v
                }
            })
            .collect();
        Ok(db_values)
    }

    // -- Control commands --

    pub fn set_volume(&self, value: u8) -> Result<(), UsbHidError> {
        self.expect_write_ok(&commands::set_volume(value), "set volume")
    }

    pub fn set_volume_db(&self, db: f32) -> Result<(), UsbHidError> {
        let value = ((-db) * 2.0).round().max(0.0).min(255.0) as u8;
        self.set_volume(value)
    }

    pub fn set_mute(&self, muted: bool) -> Result<(), UsbHidError> {
        self.expect_write_ok(&commands::set_mute(muted), "set mute")
    }

    pub fn set_source(&self, source: u8) -> Result<(), UsbHidError> {
        if self.profile.sources.source_for_index(source).is_none() {
            return Err(UsbHidError::Protocol(format!(
                "source {source} not available on this device"
            )));
        }

        if let Ok(before) = self.read_master_status_once() {
            if before.source == source {
                return Ok(());
            }
        }

        self.expect_write_ok(&commands::set_source(source), "set source")
    }

    pub fn set_preset(&self, preset: u8) -> Result<(), UsbHidError> {
        // Route through the verify path so HTTP + CLI callers share the same
        // dropped-PresetComplete fallback as the TCP bridge. 2x4 HD hardware
        // occasionally skips the completion frame after a preset switch.
        self.set_config_with_verify(preset, commands::PRESET_CHANGE_NORMAL)
    }

    pub fn set_dirac_bypass(&self, bypass: bool) -> Result<(), UsbHidError> {
        self.expect_write_ok(&commands::dirac_bypass(bypass), "set dirac bypass")
    }

    pub fn set_display_brightness(&self, level: u8) -> Result<(), UsbHidError> {
        self.expect_write_ok(
            &commands::set_display_brightness(level),
            "set display brightness",
        )
    }

    pub fn set_display_idle_time(&self, secs: u8) -> Result<(), UsbHidError> {
        self.expect_write_ok(
            &commands::set_display_idle_time(secs),
            "set display idle time",
        )
    }

    pub fn set_dre(&self, enabled: bool) -> Result<(), UsbHidError> {
        self.expect_write_ok(&commands::set_dre(enabled), "set dre")
    }

    pub fn copy_preset(&self) -> Result<(), UsbHidError> {
        self.expect_write_ok_slow(&commands::copy_preset(true), "copy preset")?;
        // Mirror Device Console's verifyProgram check: read 0xFFA3 and bail if
        // the device reports a failed flash persist (status > 0x0D).
        let status = self.read_verify_program_status()?;
        if !eeprom::is_verify_program_ok(status) {
            return Err(UsbHidError::Protocol(format!(
                "verifyProgram failed after CopyPreset: status=0x{:02X} > 0x{:02X}",
                status,
                eeprom::VERIFY_PROGRAM_OK_MAX
            )));
        }
        Ok(())
    }

    fn default_preset_image(&self) -> Result<&'static DefaultPresetImage, UsbHidError> {
        default_preset_image_for_dsp(self.dsp_version)
            .ok_or_else(|| UsbHidError::Protocol("default preset reset not supported".into()))
    }

    fn write_flash_image_block(
        &self,
        block_id: u8,
        block: &[u8],
        context: &str,
    ) -> Result<(), UsbHidError> {
        for (chunk_idx, chunk) in block.chunks(0x20).enumerate() {
            self.expect_write_ok_slow(
                &commands::write_flash_block(block_id, chunk, chunk_idx == 0),
                context,
            )?;
        }
        self.expect_write_ok_slow(&commands::finish_flash_block(block_id), context)?;
        Ok(())
    }

    fn write_preset_defaults_locked(&self) -> Result<(), UsbHidError> {
        let image = self.default_preset_image()?;
        self.write_flash_image_block(
            FLASH_BLOCK_PRESET_VALUES,
            &build_preset_values_block(image.params),
            "write default preset values",
        )?;
        self.write_flash_image_block(
            FLASH_BLOCK_PRESET_BYPASS,
            &build_preset_bypass_block(image.bypass),
            "write default preset bypass",
        )?;
        let status = self.read_verify_program_status()?;
        if !eeprom::is_verify_program_ok(status) {
            return Err(UsbHidError::Protocol(format!(
                "verifyProgram failed after default preset write: status=0x{:02X} > 0x{:02X}",
                status,
                eeprom::VERIFY_PROGRAM_OK_MAX
            )));
        }
        Ok(())
    }

    fn save_preset_token_locked(&self, preset: u8, token: [u8; 4]) -> Result<(), UsbHidError> {
        let (hi, lo) = eeprom::token_addr_for_slot(preset);
        self.expect_write_ok(
            &commands::write_flash(hi, lo, &token),
            "save modification token",
        )
    }

    pub fn supports_default_preset_reset(&self) -> bool {
        self.default_preset_image().is_ok()
    }

    pub fn reset_active_preset_to_defaults(&self) -> Result<u8, UsbHidError> {
        let _guard = self
            .try_begin_operation()
            .ok_or_else(|| UsbHidError::Protocol("device busy".into()))?;
        let preset = self.get_master_status()?.preset;
        self.write_preset_defaults_locked()?;
        let token = random_token_bytes()?;
        self.save_preset_token_locked(preset, token)?;
        self.set_config_with_verify(preset, commands::PRESET_CHANGE_RELOAD_SAME_SLOT)?;
        let snapshot = default_preset_shadow(self.profile(), self.default_preset_image()?);
        self.store_preset_shadow_snapshot(preset, snapshot);
        self.mark_snapshot_trusted(preset);
        Ok(preset)
    }

    pub fn reset_preset_to_defaults(&self, preset: u8) -> Result<(), UsbHidError> {
        let _guard = self
            .try_begin_operation()
            .ok_or_else(|| UsbHidError::Protocol("device busy".into()))?;
        self.set_config_with_verify(preset, commands::PRESET_CHANGE_WITHOUT_RESET)?;
        self.write_preset_defaults_locked()?;
        let token = random_token_bytes()?;
        self.save_preset_token_locked(preset, token)?;
        self.set_config_with_verify(preset, commands::PRESET_CHANGE_RELOAD_SAME_SLOT)?;
        let snapshot = default_preset_shadow(self.profile(), self.default_preset_image()?);
        self.store_preset_shadow_snapshot(preset, snapshot);
        self.mark_snapshot_trusted(preset);
        Ok(())
    }

    pub fn initialize_all_presets_to_defaults(&self, reboot: bool) -> Result<(), UsbHidError> {
        let _guard = self
            .try_begin_operation()
            .ok_or_else(|| UsbHidError::Protocol("device busy".into()))?;
        self.set_config_with_verify(0, commands::PRESET_CHANGE_WITHOUT_RESET)?;
        self.write_preset_defaults_locked()?;
        self.save_preset_token_locked(0, RECOVERY_MODIFICATION_TOKEN)?;
        self.set_config_with_verify(0, commands::PRESET_CHANGE_RELOAD_SAME_SLOT)?;
        self.copy_preset()?;

        let mut all_tokens = Vec::with_capacity(self.profile.presets as usize * 4);
        for _ in 0..self.profile.presets {
            all_tokens.extend_from_slice(&RECOVERY_MODIFICATION_TOKEN);
        }
        self.expect_write_ok(
            &commands::write_flash(eeprom::TOKENS_ADDR.0, eeprom::TOKENS_ADDR.1, &all_tokens),
            "write recovery modification tokens",
        )?;

        let snapshot = default_preset_shadow(self.profile(), self.default_preset_image()?);
        for preset in 0..self.profile.presets {
            self.store_preset_shadow_snapshot(preset, snapshot.clone());
            self.mark_snapshot_trusted(preset);
        }

        if reboot {
            self.reboot_device()?;
        }
        Ok(())
    }

    pub fn reboot_device(&self) -> Result<(), UsbHidError> {
        self.send_reboot_command(&commands::enter_bootloader(), "enter bootloader reboot")
    }

    pub fn supports_bass_management(&self) -> bool {
        self.profile
            .dsp_table
            .and_then(|table| table.bass_management)
            .is_some()
    }

    fn require_bm_table(
        &self,
    ) -> Result<&'static minidsp_protocol::device::dsp::BassManagement, UsbHidError> {
        self.profile
            .dsp_table
            .and_then(|table| table.bass_management)
            .ok_or_else(|| UsbHidError::Protocol("device does not support bass management".into()))
    }

    fn check_bm_channel(ch: u8) -> Result<(), UsbHidError> {
        if ch >= Self::BM_NUM_CHANNELS {
            return Err(UsbHidError::Protocol(format!(
                "bm channel {ch} out of range (0..{})",
                Self::BM_NUM_CHANNELS
            )));
        }
        Ok(())
    }

    pub fn read_bm_channel(&self, ch: u8) -> Result<BmChannelConfig, UsbHidError> {
        let bm = self.require_bm_table()?;
        Self::check_bm_channel(ch)?;
        let status_addr = bm.gain_status_base + ch as u16;
        let gain_addr = bm.gain_base + ch as u16;
        let muted = self.read_param_word(status_addr)? as u16 == 1;
        let gain_db = self.read_config_gain_db(gain_addr)?;
        Ok(BmChannelConfig {
            index: ch,
            muted,
            gain_db,
        })
    }

    pub fn set_bm_channel_gain(&self, ch: u8, gain_db: f32) -> Result<(), UsbHidError> {
        let bm = self.require_bm_table()?;
        Self::check_bm_channel(ch)?;
        let addr = bm.gain_base + ch as u16;
        self.set_channel_gain(addr, gain_db)
    }

    pub fn set_bm_channel_mute(&self, ch: u8, muted: bool) -> Result<(), UsbHidError> {
        let bm = self.require_bm_table()?;
        Self::check_bm_channel(ch)?;
        let addr = bm.gain_status_base + ch as u16;
        self.set_channel_mute(addr, muted)
    }

    pub fn read_bm_mixer_cell(&self, row: u8, col: u8) -> Result<BmMixerCellConfig, UsbHidError> {
        let bm = self.require_bm_table()?;
        let mixer = bm.bm_routing;
        let status_addr = mixer.status_addr(row, col).ok_or_else(|| {
            UsbHidError::Protocol(format!(
                "bm mixer cell ({row},{col}) out of range ({}x{})",
                mixer.rows, mixer.cols
            ))
        })?;
        let gain_addr = mixer.gain_addr(row, col).expect("validated by status_addr");
        let polarity_addr = mixer
            .polarity_addr(row, col)
            .expect("validated by status_addr");
        Ok(BmMixerCellConfig {
            row,
            col,
            enabled: self.read_param_word(status_addr)? as u16 == 2,
            gain_db: self.read_config_gain_db(gain_addr)?,
            polarity_inverted: self.read_param_word(polarity_addr)? as u16 != 0,
        })
    }

    pub fn set_bm_mixer_cell(
        &self,
        row: u8,
        col: u8,
        patch: BmMixerCellPatch,
    ) -> Result<(), UsbHidError> {
        let bm = self.require_bm_table()?;
        let mixer = bm.bm_routing;
        let status_addr = mixer.status_addr(row, col).ok_or_else(|| {
            UsbHidError::Protocol(format!(
                "bm mixer cell ({row},{col}) out of range ({}x{})",
                mixer.rows, mixer.cols
            ))
        })?;
        let gain_addr = mixer.gain_addr(row, col).expect("validated by status_addr");
        let polarity_addr = mixer
            .polarity_addr(row, col)
            .expect("validated by status_addr");
        if let Some(gain_db) = patch.gain_db {
            self.set_channel_gain(gain_addr, gain_db)?;
        }
        if let Some(enabled) = patch.enabled {
            self.write_dsp_selector_u32(status_addr, if enabled { 2 } else { 1 })?;
        }
        if let Some(polarity_inverted) = patch.polarity_inverted {
            self.set_invert(polarity_addr, polarity_inverted)?;
        }
        Ok(())
    }

    pub fn set_bm_channel(&self, ch: u8, patch: BmChannelPatch) -> Result<(), UsbHidError> {
        if let Some(gain_db) = patch.gain_db {
            self.set_bm_channel_gain(ch, gain_db)?;
        }
        if let Some(muted) = patch.muted {
            self.set_bm_channel_mute(ch, muted)?;
        }
        Ok(())
    }

    pub fn read_bass_management_state(&self) -> Result<BmStateConfig, UsbHidError> {
        let bm = self.require_bm_table()?;
        let mixer = bm.bm_routing;
        let mut channels = Vec::with_capacity(Self::BM_NUM_CHANNELS as usize);
        for ch in 0..Self::BM_NUM_CHANNELS {
            channels.push(self.read_bm_channel(ch)?);
        }
        let mut mixer_cells = Vec::with_capacity(mixer.cell_count() as usize);
        for row in 0..mixer.rows {
            for col in 0..mixer.cols {
                mixer_cells.push(self.read_bm_mixer_cell(row, col)?);
            }
        }
        Ok(BmStateConfig {
            channels,
            mixer: mixer_cells,
            rows: mixer.rows,
            cols: mixer.cols,
        })
    }

    pub fn read_verify_program_status(&self) -> Result<u8, UsbHidError> {
        let (hi, lo) = eeprom::VERIFY_PROGRAM_ADDR;
        let req = commands::read_flash(hi, lo, 1);
        let resp = self.bus.command_with_timeout(&req, SLOW_COMMAND_TIMEOUT)?;
        match commands::parse_response_for_request(&req, &resp, self.dialect()) {
            Response::FlashData(data) if !data.is_empty() => Ok(data[0]),
            Response::FlashData(_) => Err(UsbHidError::Protocol(
                "verify-program read returned empty payload".into(),
            )),
            other => Err(UsbHidError::Protocol(format!(
                "verify-program ReadFlash unexpected response: {other:?}"
            ))),
        }
    }

    // -- DSP writes --

    #[allow(dead_code)]
    pub fn write_biquad(&self, addr: u16, coeffs: &[f32; 5]) -> Result<(), UsbHidError> {
        info!("write_biquad start addr=0x{addr:04x}");
        self.expect_write_ok(
            &commands::write_biquad(addr, coeffs, self.dialect()),
            "write biquad",
        )?;
        info!("write_biquad done addr=0x{addr:04x}");
        if let Ok(mut shadow) = self.peq_shadow.lock() {
            let entry = shadow.entry(addr).or_insert(PeqShadow {
                coeffs: minidsp_protocol::biquad::BYPASS,
                bypass: false,
            });
            entry.coeffs = *coeffs;
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub fn bypass_filter(&self, addr: u16, bypass: bool) -> Result<(), UsbHidError> {
        info!("bypass_filter start addr=0x{addr:04x} bypass={bypass}");
        self.expect_write_ok(
            &commands::bypass_filter(addr, bypass, self.dialect()),
            "bypass filter",
        )?;
        info!("bypass_filter done addr=0x{addr:04x} bypass={bypass}");
        if let Ok(mut shadow) = self.peq_shadow.lock() {
            let entry = shadow.entry(addr).or_insert(PeqShadow {
                coeffs: minidsp_protocol::biquad::BYPASS,
                bypass: false,
            });
            entry.bypass = bypass;
        }
        Ok(())
    }

    // -- Measurement mode --

    /// Snapshot every PEQ band on the device and then bypass all of them so
    /// a REW sweep sees an unfiltered signal path. The snapshot is stashed
    /// in `saved_peq` so a later [`restore_measurement_mode`] can roll back.
    pub fn enter_measurement_mode(&self) -> Result<(), UsbHidError> {
        let table = self
            .profile
            .dsp_table
            .ok_or_else(|| UsbHidError::Protocol("measurement mode unsupported".into()))?;

        let mut bands: Vec<(u16, [f32; 5], bool)> = Vec::new();

        for input in table.inputs {
            for band in input.peq {
                let coeffs = self.read_peq_coeffs(band.addr)?;
                let bypass = self.read_peq_bypass(band.addr).unwrap_or(false);
                bands.push((band.addr, coeffs, bypass));
            }
        }
        for output in table.outputs {
            for band in output.peq {
                let coeffs = self.read_peq_coeffs(band.addr)?;
                let bypass = self.read_peq_bypass(band.addr).unwrap_or(false);
                bands.push((band.addr, coeffs, bypass));
            }
        }

        // Snapshot before mutating so a failure mid-loop still has the state
        // captured for an eventual restore.
        *self.saved_peq.lock().unwrap_or_else(|e| e.into_inner()) = Some(SavedPeqState {
            bands: bands.clone(),
        });

        for &(addr, _, _) in &bands {
            self.bypass_filter(addr, true)?;
        }

        info!(
            "measurement mode: saved and bypassed {} PEQ bands",
            bands.len()
        );
        Ok(())
    }

    /// Restore PEQ state captured by [`enter_measurement_mode`]. Errors if
    /// no snapshot is currently stored.
    pub fn restore_measurement_mode(&self) -> Result<(), UsbHidError> {
        let saved = self
            .saved_peq
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(|| UsbHidError::Protocol("no saved measurement mode state".into()))?;
        self.restore_measurement_mode_with(&saved)?;
        *self.saved_peq.lock().unwrap_or_else(|e| e.into_inner()) = None;
        Ok(())
    }

    /// Restore PEQ state from an externally-provided snapshot. Uses a safe
    /// three-phase write ordering that mirrors the desktop daemon's
    /// `build_measurement_restore_batches`: bypass every affected band
    /// first (so mid-stream coefficient writes don't cause audible
    /// discontinuities), then write coefficients, then restore the saved
    /// per-band bypass bits.
    pub fn restore_measurement_mode_with(&self, saved: &SavedPeqState) -> Result<(), UsbHidError> {
        for &(addr, _, _) in &saved.bands {
            self.bypass_filter(addr, true)?;
        }
        for &(addr, coeffs, _) in &saved.bands {
            self.write_biquad(addr, &coeffs)?;
        }
        for &(addr, _, bypass) in &saved.bands {
            self.bypass_filter(addr, bypass)?;
        }

        info!("measurement mode: restored {} PEQ bands", saved.bands.len());
        Ok(())
    }

    /// Read the currently-saved measurement-mode snapshot, if any.
    #[allow(dead_code)]
    pub fn saved_measurement_state(&self) -> Option<SavedPeqState> {
        self.saved_peq
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Overwrite the saved measurement-mode snapshot. `None` clears it.
    #[allow(dead_code)]
    pub fn set_saved_measurement_state(&self, state: Option<SavedPeqState>) {
        *self.saved_peq.lock().unwrap_or_else(|e| e.into_inner()) = state;
    }

    // -- PEQ shadow NVS persistence --
    //
    // The in-memory `peq_shadow` is re-read from the device on every boot,
    // which is fine for selector-reliable hardware. On the 2x4 HD / DSP100
    // class of devices the live readback is unreliable (see
    // `has_unreliable_selector_readback`), so losing the shadow on reboot
    // effectively loses the user's last ezBEQ load. Persisting the shadow
    // to NVS closes that gap without adding any always-on flash wear — we
    // only write on explicit commit boundaries (full-config POST,
    // measurement-mode transitions).

    /// Load the PEQ shadow from NVS, merging entries onto whatever
    /// `build_peq_shadow` seeded. Missing keys or decode failures are
    /// non-fatal — we just log and keep the in-memory default.
    pub fn load_peq_shadow_from_nvs(&self, nvs: &EspDefaultNvsPartition) {
        let ns = match EspNvs::<NvsDefault>::new(nvs.clone(), PEQ_NVS_NAMESPACE, false) {
            Ok(ns) => ns,
            Err(e) => {
                warn!("peq shadow NVS open (ro) failed: {e}");
                return;
            }
        };
        let key = peq_nvs_key(self.serial);
        let mut buf = vec![0u8; PEQ_SHADOW_MAX_BYTES];
        match ns.get_blob(&key, &mut buf) {
            Ok(Some(blob)) => match decode_peq_shadow(blob) {
                Some(decoded) => {
                    let mut shadow = self.peq_shadow.lock().unwrap_or_else(|e| e.into_inner());
                    let mut merged = 0usize;
                    for (addr, entry) in decoded {
                        if let Some(slot) = shadow.get_mut(&addr) {
                            *slot = entry;
                            merged += 1;
                        } else {
                            // Unknown address — insert anyway so subsequent
                            // reads see the persisted value.
                            shadow.insert(addr, entry);
                            merged += 1;
                        }
                    }
                    info!("peq shadow: restored {merged} bands from NVS (key {key})");
                }
                None => {
                    warn!("peq shadow NVS blob under key {key} failed to decode — ignoring");
                }
            },
            Ok(None) => {
                debug!("peq shadow: no NVS blob under {key}");
            }
            Err(e) => {
                warn!("peq shadow NVS read failed: {e}");
            }
        }
    }

    /// Save the current PEQ shadow to NVS. Intended to be called at commit
    /// boundaries — on every `/config` POST tail, after measurement-mode
    /// enter/restore, etc. — not on every individual biquad write.
    pub fn save_peq_shadow_to_nvs(&self, nvs: &EspDefaultNvsPartition) {
        let ns = match EspNvs::<NvsDefault>::new(nvs.clone(), PEQ_NVS_NAMESPACE, true) {
            Ok(ns) => ns,
            Err(e) => {
                warn!("peq shadow NVS open (rw) failed: {e}");
                return;
            }
        };
        let key = peq_nvs_key(self.serial);
        let shadow = self.peq_shadow.lock().unwrap_or_else(|e| e.into_inner());
        let blob = encode_peq_shadow(&shadow);
        drop(shadow);
        if blob.len() > PEQ_SHADOW_MAX_BYTES {
            warn!(
                "peq shadow NVS encode too large ({} > {PEQ_SHADOW_MAX_BYTES}) — skipping persist",
                blob.len()
            );
            return;
        }
        match ns.set_blob(&key, &blob) {
            Ok(()) => debug!(
                "peq shadow: persisted {} bytes to NVS key {key}",
                blob.len()
            ),
            Err(e) => warn!("peq shadow NVS write failed: {e}"),
        }
    }

    // -- Bridge command verification (SetConfig / Reset) --

    /// Switch presets with master-status verification.
    ///
    /// Mirrors the desktop daemon's `set_config_with_mode` so the TCP
    /// bridge path returns a successful completion frame even on 2x4 HD
    /// hardware that sometimes drops `RESP_PRESET_COMPLETE` after the
    /// switch. The command send is allowed to time out — we then confirm
    /// the switch actually happened by polling master status.
    pub fn set_config_with_verify(&self, preset: u8, reset_mode: u8) -> Result<(), UsbHidError> {
        // The 2x4 HD can take several seconds to emit `RESP_PRESET_COMPLETE`.
        // Timing out too early forces the USB timeout-recovery path on every
        // normal preset switch, which is both noisy and unstable under stress.
        const SEND_TIMEOUT: Duration = Duration::from_secs(5);
        const VERIFY_TIMEOUT: Duration = Duration::from_secs(5);
        const VERIFY_POLL_DELAY: Duration = Duration::from_millis(100);
        const POST_SET_CONFIG_SETTLE_DELAY: Duration = Duration::from_secs(1);

        if preset >= self.profile.presets {
            return Err(UsbHidError::Protocol(format!(
                "preset {} out of range (max {})",
                preset,
                self.profile.presets - 1
            )));
        }

        // Fast path: already on the requested preset. A same-slot reload still
        // needs to be sent because the reset flow uses it to activate a
        // freshly-written preset image.
        if let Ok(before) = self.read_master_status_once() {
            if before.preset == preset && reset_mode != commands::PRESET_CHANGE_RELOAD_SAME_SLOT {
                return Ok(());
            }
        }

        let payload = commands::set_config(preset, reset_mode);
        match self.bus.command_with_timeout(&payload, SEND_TIMEOUT) {
            Ok(_) => {}
            Err(UsbHidError::Timeout) => {
                debug!("set_config send timed out — verifying via master status");
            }
            Err(err) => return Err(err),
        }

        let deadline = Instant::now() + VERIFY_TIMEOUT;
        loop {
            std::thread::sleep(VERIFY_POLL_DELAY);
            if let Ok(status) = self.read_master_status_once() {
                if status.preset == preset {
                    std::thread::sleep(POST_SET_CONFIG_SETTLE_DELAY);
                    return Ok(());
                }
            }
            if Instant::now() >= deadline {
                return Err(UsbHidError::Protocol(format!(
                    "set_config did not persist (expected preset {preset})"
                )));
            }
        }
    }

    #[allow(dead_code)]
    pub fn write_dsp(&self, addr: u16, value: f32, save: bool) -> Result<(), UsbHidError> {
        self.expect_write_ok(
            &commands::write_dsp(addr, value, save, self.dialect()),
            "write dsp",
        )?;
        if self.profile.dialect.value_format == ValueFormat::Float32 {
            self.scalar_shadow
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(addr, value.to_bits());
        }
        Ok(())
    }

    pub fn write_dsp_verified(
        &self,
        addr: u16,
        value: f32,
        save: bool,
        context: &str,
    ) -> Result<(), UsbHidError> {
        if self.profile.dialect.value_format != ValueFormat::Float32 {
            return self.write_dsp(addr, value, save);
        }
        let before = self.read_param_word(addr)?;
        let expected = value.to_bits();
        self.write_dsp(addr, value, save)?;
        let after = self.read_param_word(addr)?;
        if after != expected {
            if self.prefers_scalar_shadow_for_addr(addr) {
                info!(
                    "accepting shadow-backed scalar write at addr {addr} despite non-authoritative live readback (before=0x{before:08X}, expected=0x{expected:08X}, after=0x{after:08X})"
                );
                return Ok(());
            }
            return Err(UsbHidError::Protocol(format!(
                "{context} at addr {addr} did not persist (before=0x{before:08X}, expected=0x{expected:08X}, after=0x{after:08X})"
            )));
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub fn write_dsp_u32(&self, addr: u16, value: u32, save: bool) -> Result<(), UsbHidError> {
        self.expect_write_ok(&commands::write_dsp_u32(addr, value, save), "write dsp u32")
    }

    pub(crate) fn write_dsp_int_verified(
        &self,
        addr: u16,
        value: u16,
        context: &str,
    ) -> Result<(), UsbHidError> {
        self.expect_write_ok(&commands::write_dsp_int(addr, value), context)?;
        self.selector_shadow
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(addr, value);
        Ok(())
    }

    #[allow(dead_code)]
    pub fn write_dsp_delay(&self, addr: u16, samples: u32, save: bool) -> Result<(), UsbHidError> {
        self.expect_write_ok(
            &commands::write_dsp_delay(addr, samples, save, self.dialect()),
            "write dsp delay",
        )?;
        if self.profile.dialect.value_format == ValueFormat::Float32 {
            self.scalar_shadow
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(addr, samples.min(u16::MAX as u32));
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub fn write_dsp_selector_u32(&self, addr: u16, value: u32) -> Result<(), UsbHidError> {
        self.expect_write_ok(
            &commands::write_dsp_u32_le_with_ctrl(addr, value, 0x80),
            "write dsp selector",
        )?;
        self.selector_shadow
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(addr, value as u16);
        Ok(())
    }

    pub fn set_status_bypass(&self, addr: u16, bypass: bool) -> Result<(), UsbHidError> {
        match self.profile.dialect.value_format {
            ValueFormat::Float32 => self.write_dsp_selector_u32(addr, if bypass { 3 } else { 2 }),
            ValueFormat::FixedPoint => self.write_dsp(addr, if bypass { 1.0 } else { 0.0 }, true),
        }
    }

    /// Set channel polarity inversion using dialect-aware encoding.
    #[allow(dead_code)]
    pub fn set_invert(&self, addr: u16, inverted: bool) -> Result<(), UsbHidError> {
        match self.profile.dialect.value_format {
            ValueFormat::Float32 => self.write_dsp_u32(addr, if inverted { 1 } else { 0 }, false),
            ValueFormat::FixedPoint => {
                let val = minidsp_protocol::dialect::polarity_value(inverted, self.dialect());
                self.write_dsp(addr, val, true)
            }
        }
    }

    #[allow(dead_code)]
    pub fn set_channel_gain(&self, addr: u16, db: f32) -> Result<(), UsbHidError> {
        let before = self.read_config_gain_db(addr)?;
        match self.profile.dialect.value_format {
            ValueFormat::Float32 => self.write_dsp(addr, db, false),
            ValueFormat::FixedPoint => self.write_dsp(addr, 10.0f32.powf(db / 20.0), true),
        }?;
        let after = self.read_config_gain_db(addr)?;
        if (after - db).abs() > GAIN_VERIFY_TOLERANCE_DB {
            if self.prefers_scalar_shadow_for_addr(addr) {
                info!(
                    "accepting shadow-backed gain write at addr {addr} despite non-authoritative live readback (before={before:.6}, expected={db:.6}, after={after:.6})"
                );
                return Ok(());
            }
            return Err(UsbHidError::Protocol(format!(
                "set channel gain at addr {addr} did not persist (before={before:.6}, expected={db:.6}, after={after:.6})"
            )));
        }
        Ok(())
    }

    pub fn set_channel_gain_persisted(&self, addr: u16, db: f32) -> Result<(), UsbHidError> {
        match self.profile.dialect.value_format {
            ValueFormat::Float32 => self.write_dsp_verified(addr, db, true, "set channel gain"),
            ValueFormat::FixedPoint => self.set_channel_gain(addr, db),
        }
    }

    #[allow(dead_code)]
    pub fn set_routing(
        &self,
        input: u8,
        output: u8,
        enabled: bool,
        gain_db: f32,
    ) -> Result<(), UsbHidError> {
        let table = self
            .profile
            .dsp_table
            .ok_or_else(|| UsbHidError::Protocol("routing unsupported".into()))?;
        let routing = table.routing;
        if input >= routing.rows || output >= routing.cols {
            return Err(UsbHidError::Protocol(format!(
                "routing index ({input}, {output}) out of range (max {}x{})",
                routing.rows, routing.cols
            )));
        }

        let flat_idx = input as u16 * routing.cols as u16 + output as u16;
        let status_addr = routing.status_base + flat_idx;
        let gain_addr = routing.gain_base + flat_idx;

        self.set_channel_gain(gain_addr, gain_db)?;
        match self.profile.dialect.value_format {
            ValueFormat::Float32 => {
                // Mixer status: 2=enabled, 1=disabled (matches minidsp-rs WriteInt).
                // Uses 0x13 with LE u32 value — audio-verified on 2x4 HD.
                let val = if enabled { 2u32 } else { 1u32 };
                self.write_dsp_selector_u32(status_addr, val)
            }
            ValueFormat::FixedPoint => {
                self.write_dsp(status_addr, if enabled { 1.0 } else { 0.0 }, true)
            }
        }
    }

    pub fn set_routing_persisted(
        &self,
        input: u8,
        output: u8,
        enabled: bool,
        gain_db: f32,
    ) -> Result<(), UsbHidError> {
        let table = self
            .profile
            .dsp_table
            .ok_or_else(|| UsbHidError::Protocol("routing unsupported".into()))?;
        let routing = table.routing;
        if input >= routing.rows || output >= routing.cols {
            return Err(UsbHidError::Protocol(format!(
                "routing index ({input}, {output}) out of range (max {}x{})",
                routing.rows, routing.cols
            )));
        }

        let flat_idx = input as u16 * routing.cols as u16 + output as u16;
        let status_addr = routing.status_base + flat_idx;
        let gain_addr = routing.gain_base + flat_idx;

        self.set_channel_gain_persisted(gain_addr, gain_db)?;
        match self.profile.dialect.value_format {
            ValueFormat::Float32 => {
                let val = if enabled { 2u32 } else { 1u32 };
                self.write_dsp_selector_u32(status_addr, val)
            }
            ValueFormat::FixedPoint => {
                self.write_dsp(status_addr, if enabled { 1.0 } else { 0.0 }, true)
            }
        }
    }

    pub fn set_routing_polarity(
        &self,
        input: u8,
        output: u8,
        inverted: bool,
    ) -> Result<(), UsbHidError> {
        let table = self
            .profile
            .dsp_table
            .ok_or_else(|| UsbHidError::Protocol("routing unsupported".into()))?;
        let routing = table.routing;
        if input >= routing.rows || output >= routing.cols {
            return Err(UsbHidError::Protocol(format!(
                "routing index ({input}, {output}) out of range (max {}x{})",
                routing.rows, routing.cols
            )));
        }
        let polarity_base = routing
            .polarity_base
            .ok_or_else(|| UsbHidError::Protocol("routing polarity unsupported".into()))?;
        let flat_idx = input as u16 * routing.cols as u16 + output as u16;
        let polarity_addr = polarity_base + flat_idx;
        self.set_invert(polarity_addr, inverted)
    }

    #[allow(dead_code)]
    pub fn set_channel_mute(&self, addr: u16, muted: bool) -> Result<(), UsbHidError> {
        match self.profile.dialect.value_format {
            ValueFormat::Float32 => {
                // Mute status: 1=muted, 2=unmuted (matches minidsp-rs WriteInt).
                let val = if muted { 1u32 } else { 2u32 };
                self.write_dsp_selector_u32(addr, val)
            }
            ValueFormat::FixedPoint => {
                let val = if muted { 0.0 } else { 1.0 };
                self.write_dsp(addr, val, true)
            }
        }
    }

    #[allow(dead_code)]
    pub fn write_dsp_linked_u32(
        &self,
        addr: u16,
        value: u32,
        linked_addrs: &[u16],
    ) -> Result<(), UsbHidError> {
        self.expect_write_ok(
            &commands::write_dsp_linked_u32(addr, value, linked_addrs),
            "write dsp linked u32",
        )
    }

    #[allow(dead_code)]
    pub fn set_channel_delay_samples(&self, addr: u16, samples: u32) -> Result<(), UsbHidError> {
        let before = self.read_param_scalar_u32(addr)?;
        let expected = match self.profile.dialect.value_format {
            ValueFormat::Float32 => samples.min(u16::MAX as u32),
            ValueFormat::FixedPoint => samples,
        };
        match self.profile.dialect.value_format {
            ValueFormat::Float32 => self.write_dsp_delay(addr, samples, false),
            ValueFormat::FixedPoint => self.write_dsp_delay(addr, samples, true),
        }?;
        let after = self.read_param_scalar_u32(addr)?;
        if after != expected {
            if self.prefers_scalar_shadow_for_addr(addr) {
                info!(
                    "accepting shadow-backed delay write at addr {addr} despite non-authoritative live readback (before={before}, expected={expected}, after={after})"
                );
                return Ok(());
            }
            return Err(UsbHidError::Protocol(format!(
                "set channel delay at addr {addr} did not persist (before={before}, expected={expected}, after={after})"
            )));
        }
        Ok(())
    }

    pub fn set_channel_delay_samples_persisted(
        &self,
        addr: u16,
        samples: u32,
    ) -> Result<(), UsbHidError> {
        match self.profile.dialect.value_format {
            ValueFormat::Float32 => {
                let before = self.read_param_scalar_u32(addr)?;
                let expected = samples.min(u16::MAX as u32);
                self.write_dsp_delay(addr, samples, true)?;
                let after = self.read_param_scalar_u32(addr)?;
                if after != expected {
                    if self.prefers_scalar_shadow_for_addr(addr) {
                        info!(
                            "accepting shadow-backed delay write at addr {addr} despite non-authoritative live readback (before={before}, expected={expected}, after={after})"
                        );
                        return Ok(());
                    }
                    return Err(UsbHidError::Protocol(format!(
                        "set channel delay at addr {addr} did not persist (before={before}, expected={expected}, after={after})"
                    )));
                }
                Ok(())
            }
            ValueFormat::FixedPoint => self.set_channel_delay_samples(addr, samples),
        }
    }

    pub fn load_fir_coefficients(
        &self,
        fir: minidsp_protocol::device::dsp::FirFilter,
        coeffs: &[f32],
    ) -> Result<(), UsbHidError> {
        if coeffs.len() > fir.max_taps as usize {
            return Err(UsbHidError::Protocol(format!(
                "FIR has {} taps but device maximum is {}",
                coeffs.len(),
                fir.max_taps
            )));
        }

        let master = self.get_master_status()?;
        self.expect_write_ok(
            &commands::write_dsp_int(fir.taps_count, coeffs.len() as u16),
            "set fir tap count",
        )?;

        let start = commands::fir_load_start(fir.index);
        let start_resp = self.bus.command(&start)?;
        let max_coeffs =
            match commands::parse_response_for_request(&start, &start_resp, self.dialect()) {
                Response::FirLoadInfo { max_coeffs } => max_coeffs,
                Response::Error(data) => {
                    return Err(UsbHidError::Protocol(format!(
                        "fir load start: device error {data:02x?}"
                    )));
                }
                other => {
                    return Err(UsbHidError::Protocol(format!(
                        "fir load start: unexpected response {other:?}"
                    )));
                }
            };
        if coeffs.len() > max_coeffs as usize {
            return Err(UsbHidError::Protocol(format!(
                "FIR has {} taps but device reported runtime maximum {}",
                coeffs.len(),
                max_coeffs
            )));
        }

        for chunk in coeffs.chunks(14) {
            self.expect_write_ok(&commands::fir_load_data(fir.index, chunk), "fir load data")?;
        }
        self.expect_write_ok_with_timeout(
            &commands::fir_load_end(),
            FIR_LOAD_END_TIMEOUT,
            "fir load end",
        )?;
        self.fir_shadow
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(fir.data_start, coeffs.to_vec());
        self.selector_shadow
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(fir.taps_count, coeffs.len() as u16);
        self.set_mute(master.is_muted())?;
        Ok(())
    }

    pub fn reset_all_peq_to_defaults(&self) -> Result<usize, UsbHidError> {
        let table = self
            .profile
            .dsp_table
            .ok_or_else(|| UsbHidError::Protocol("config read unsupported".into()))?;
        let mut touched = 0usize;
        for input in table.inputs {
            for band in input.peq {
                self.write_biquad(band.addr, &minidsp_protocol::biquad::BYPASS)?;
                self.bypass_filter(band.addr, false)?;
                touched += 1;
            }
        }
        for output in table.outputs {
            for band in output.peq {
                self.write_biquad(band.addr, &minidsp_protocol::biquad::BYPASS)?;
                self.bypass_filter(band.addr, false)?;
                touched += 1;
            }
        }
        Ok(touched)
    }

    pub fn default_peq_frequencies(&self) -> &'static [u32; 10] {
        &minidsp_protocol::device::dsp::DEFAULT_PEQ_FREQS_HZ
    }

    // -- Flash reads --

    pub fn read_flash_data(
        &self,
        addr_hi: u8,
        addr_lo: u8,
        count: u8,
    ) -> Result<Vec<u8>, UsbHidError> {
        let req = commands::read_flash(addr_hi, addr_lo, count);
        let resp = self.bus.command(&req)?;
        match commands::parse_response_for_request(&req, &resp, self.dialect()) {
            Response::FlashData(data) => Ok(data),
            other => Err(UsbHidError::Protocol(format!("flash read: {other:?}"))),
        }
    }

    // -- Display (EEPROM 0xFFE8) --

    pub fn read_display(&self) -> Result<(u8, u8), UsbHidError> {
        let data = self.read_flash_data(0xFF, 0xE8, 2)?;
        Ok((
            data.first().copied().unwrap_or(0), // idle_time
            data.get(1).copied().unwrap_or(0),  // brightness
        ))
    }

    // -- DRE (EEPROM 0xFFED) --

    pub fn read_dre(&self) -> Result<bool, UsbHidError> {
        let data = self.read_flash_data(0xFF, 0xED, 1)?;
        Ok(data.first().copied().unwrap_or(0) != 0)
    }

    // -- Dirac (EEPROM 0xFFE0) --

    pub fn read_dirac_state(&self) -> Result<bool, UsbHidError> {
        let data = self.read_flash_data(0xFF, 0xE0, 1)?;
        Ok(data.first().copied().unwrap_or(1) == 0) // 0 = active, 1 = bypassed
    }

    // -- Tokens (EEPROM 0xFFC8) --

    pub fn read_tokens(&self) -> Result<Vec<u8>, UsbHidError> {
        let (hi, lo) = eeprom::TOKENS_ADDR;
        self.read_flash_data(hi, lo, 16)
    }

    pub(crate) fn read_modification_tokens(&self) -> Result<HashMap<u8, [u8; 4]>, UsbHidError> {
        let bytes = self.read_tokens()?;
        let mut tokens = HashMap::new();
        for (preset, chunk) in bytes.chunks_exact(4).enumerate() {
            tokens.insert(preset as u8, [chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        Ok(tokens)
    }

    pub(crate) fn write_preset_token_exact(
        &self,
        preset: u8,
        token: [u8; 4],
    ) -> Result<(), UsbHidError> {
        if preset >= self.profile.presets {
            return Err(UsbHidError::Protocol(format!(
                "preset {} out of range (max {})",
                preset,
                self.profile.presets.saturating_sub(1)
            )));
        }
        self.save_preset_token_locked(preset, token)
    }

    // -- Raw command --

    /// Execute a raw command and mirror successful write side-effects into
    /// the in-memory shadow state so `/config` reflects TCP bridge /
    /// WebSocket activity.
    pub fn raw_command(&self, payload: &[u8]) -> Result<Vec<u8>, UsbHidError> {
        if payload.first().copied() == Some(commands::CMD_SET_CONFIG) && payload.len() >= 2 {
            let preset = payload[1];
            let reset_mode = payload
                .get(2)
                .copied()
                .unwrap_or(commands::PRESET_CHANGE_NORMAL);
            self.set_config_with_verify(preset, reset_mode)?;
            return Ok(vec![
                if reset_mode == commands::PRESET_CHANGE_WITHOUT_RESET {
                    commands::RESP_OK
                } else {
                    commands::RESP_PRESET_COMPLETE
                },
            ]);
        }

        if payload.first().copied() == Some(commands::CMD_ENTER_BOOTLOADER) {
            self.reboot_device()?;
            return Ok(vec![commands::RESP_RESET_COMPLETE]);
        }

        let mut resp = self
            .bus
            .command_with_timeout(payload, raw_bridge_timeout_for_payload(payload))?;
        // Trim HID echo garbage: for write commands, real DSP100/2x4 HD hardware
        // returns an unframed 0x01-prefixed echo of the full 64-byte HID buffer.
        // decode_hid_response strips the 0x01 prefix but returns all 63 trailing
        // bytes including garbage. Trim to the original request length so TCP
        // bridge clients (Device Console, minidsp-rs) only see the echoed command.
        if let Some(opcode) = payload.first().copied() {
            if commands::is_write_command(opcode)
                && resp.first().copied() == payload.first().copied()
                && resp.len() > payload.len()
            {
                resp.truncate(payload.len());
            }
        }
        let parsed = commands::parse_response_for_request(payload, &resp, self.dialect());
        if matches!(
            parsed,
            Response::Ack | Response::PresetComplete | Response::ResetComplete
        ) {
            self.mirror_raw_to_shadow(payload);
        }
        Ok(resp)
    }

    /// Parse a raw write payload and update in-memory shadow state for
    /// successful bridge / WebSocket writes.
    fn mirror_raw_to_shadow(&self, payload: &[u8]) {
        if let Some((addr, coeffs)) = parse_raw_write_biquad(payload, self.dialect()) {
            if let Ok(mut shadow) = self.peq_shadow.lock() {
                if let Some(entry) = shadow.get_mut(&addr) {
                    entry.coeffs = coeffs;
                }
            }
            return;
        }

        if let Some((addr, bypass)) = parse_raw_bypass_filter(payload) {
            if let Ok(mut shadow) = self.peq_shadow.lock() {
                if let Some(entry) = shadow.get_mut(&addr) {
                    entry.bypass = bypass;
                }
            }
            return;
        }

        if let Some((addr, value)) = parse_raw_selector_write(payload, self.dialect())
            .or_else(|| parse_raw_linked_selector_write(payload, self.dialect()))
        {
            self.selector_shadow
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(addr, value);
            return;
        }

        if self.profile.dialect.value_format == ValueFormat::Float32
            && payload.first().copied() == Some(commands::CMD_WRITE_DSP)
            && payload.len() >= 8
        {
            let addr = u16::from_be_bytes([payload[2], payload[3]]);
            let raw = u32::from_le_bytes([payload[4], payload[5], payload[6], payload[7]]);
            self.scalar_shadow
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(addr, raw);
        }
    }

    // -- Device info --

    pub fn device_info(&self) -> DeviceInfo {
        DeviceInfo {
            url: format!("usb://0x{:04x}:0x{:04x}", self.bus.vid(), self.bus.pid()),
            version: DeviceVersion {
                hw_id: self.hw_id.hw_id,
                fw_major: self.hw_id.fw_major,
                fw_minor: self.hw_id.fw_minor,
                dsp_version: self.dsp_version,
                serial: self.serial,
            },
            product_name: self.product_name().to_string(),
        }
    }
}

fn merge_trusted_snapshot(
    mut existing: PresetShadowState,
    captured: PresetShadowState,
) -> PresetShadowState {
    for (addr, entry) in captured.peq {
        if existing.peq.contains_key(&addr)
            || entry.coeffs != minidsp_protocol::biquad::BYPASS
            || entry.bypass
        {
            existing.peq.insert(addr, entry);
        }
    }
    existing.selector.extend(captured.selector);
    existing.scalar.extend(captured.scalar);
    existing.fir_blocks.extend(captured.fir_blocks);
    existing
}

#[cfg(test)]
fn encode_tcp_bridge_response(request: &[u8], raw_resp: &[u8], dialect: &Dialect) -> Vec<u8> {
    minidsp_protocol::packet::encode_device_console_tcp_response(request, raw_resp, dialect)
}

#[cfg(test)]
mod tcp_bridge_tests {
    use super::{encode_tcp_bridge_response, raw_bridge_timeout_for_payload, SLOW_COMMAND_TIMEOUT};
    use minidsp_protocol::commands;
    use minidsp_protocol::dialect::Dialect;

    #[test]
    fn tcp_bridge_write_flash_block_echo_becomes_ok_frame() {
        let request = [0x06u8, 0x01, 0x04, 0x88, 0x97, 0x13, 0x0f, 0x00, 0x00];
        let raw_resp = request;
        assert_eq!(
            encode_tcp_bridge_response(&request, &raw_resp, &Dialect::NEW),
            vec![1]
        );
    }

    #[test]
    fn tcp_bridge_finish_flash_block_echo_becomes_block_event() {
        let request = [0x07u8, 0x02];
        let raw_resp = request;
        assert_eq!(
            encode_tcp_bridge_response(&request, &raw_resp, &Dialect::NEW),
            vec![2, 0x02]
        );
    }

    #[test]
    fn tcp_bridge_finish_flash_block_event_stays_block_event() {
        let request = [0x07u8, 0x02];
        let raw_resp = [commands::RESP_EVENT, 0x02];
        assert_eq!(
            encode_tcp_bridge_response(&request, &raw_resp, &Dialect::NEW),
            vec![2, 0x02]
        );
    }

    #[test]
    fn tcp_bridge_finish_flash_block_bare_event_uses_request_block_id() {
        let request = [0x07u8, 0x02];
        let raw_resp = [commands::RESP_EVENT];
        assert_eq!(
            encode_tcp_bridge_response(&request, &raw_resp, &Dialect::NEW),
            vec![2, 0x02]
        );
    }

    #[test]
    fn tcp_bridge_preset_complete_stays_event_frame() {
        let request = commands::set_config(0, commands::PRESET_CHANGE_RELOAD_SAME_SLOT);
        let raw_resp = [commands::RESP_PRESET_COMPLETE];
        assert_eq!(
            encode_tcp_bridge_response(&request, &raw_resp, &Dialect::NEW),
            vec![2, commands::RESP_PRESET_COMPLETE]
        );
    }

    #[test]
    fn tcp_bridge_read_flash_keeps_command_response_shape() {
        let request = commands::read_flash(0xff, 0xa3, 1);
        let raw_resp = [commands::CMD_READ_FLASH, 0xff, 0xa3, 0x03];
        assert_eq!(
            encode_tcp_bridge_response(&request, &raw_resp, &Dialect::NEW),
            vec![5, commands::CMD_READ_FLASH, 0xff, 0xa3, 0x03]
        );
    }

    #[test]
    fn tcp_bridge_error_becomes_single_byte_error_frame() {
        let request = commands::write_flash(0xff, 0xc8, &[0x12, 0x34, 0x56, 0x78]);
        let raw_resp = [commands::RESP_ERROR, 0x71];
        assert_eq!(
            encode_tcp_bridge_response(&request, &raw_resp, &Dialect::NEW),
            vec![0]
        );
    }

    #[test]
    fn tcp_bridge_set_config_without_reset_becomes_ok_frame() {
        let request = commands::set_config(0, commands::PRESET_CHANGE_WITHOUT_RESET);
        let raw_resp = [commands::RESP_OK];
        assert_eq!(
            encode_tcp_bridge_response(&request, &raw_resp, &Dialect::NEW),
            vec![1]
        );
    }

    #[test]
    fn raw_bridge_flash_commands_use_slow_timeout() {
        assert_eq!(
            raw_bridge_timeout_for_payload(&commands::write_flash_block(0x01, &[0x04, 0x88], true)),
            SLOW_COMMAND_TIMEOUT
        );
        assert_eq!(
            raw_bridge_timeout_for_payload(&commands::finish_flash_block(0x02)),
            SLOW_COMMAND_TIMEOUT
        );
        assert_eq!(
            raw_bridge_timeout_for_payload(&commands::copy_preset(true)),
            SLOW_COMMAND_TIMEOUT
        );
        assert_eq!(
            raw_bridge_timeout_for_payload(&commands::enter_bootloader()),
            SLOW_COMMAND_TIMEOUT
        );
    }
}

// -- PEQ shadow NVS encoding helpers --

/// NVS namespace used for PEQ shadow blobs. Max 15 chars.
const PEQ_NVS_NAMESPACE: &str = "peqshad";

/// Soft cap on the encoded PEQ shadow blob. NVS auto-chunks blobs bigger
/// than one page (~4000 bytes) so we can comfortably go above the
/// single-chunk boundary; 8 KiB fits the biggest supported device
/// (Flex HT with every PEQ band populated = ~160 bands × 23 bytes + 3
/// header ≈ 3.7 KiB) with healthy headroom. Increase if we add devices
/// with more bands.
const PEQ_SHADOW_MAX_BYTES: usize = 8192;

/// Wire format version — future-proofs the blob for field additions.
const PEQ_SHADOW_FORMAT_V1: u8 = 0x01;

fn peq_nvs_key(serial: u32) -> String {
    format!("s{serial:08x}")
}

fn sanitize_shadow_preferred_scalar(raw: u32) -> Option<f32> {
    if raw <= 2 {
        return Some(0.0);
    }

    let value = f32::from_bits(raw);
    if !value.is_finite() || value.abs() > 10_000.0 {
        return Some(0.0);
    }

    None
}

fn parse_raw_write_biquad(payload: &[u8], dialect: &Dialect) -> Option<(u16, [f32; 5])> {
    if payload.first().copied()? != commands::CMD_WRITE_BIQUAD || payload.len() < 26 {
        return None;
    }
    if payload[1] != 0x80 {
        return None;
    }
    let addr = u16::from_be_bytes([payload[2], payload[3]]);
    let mut coeffs = [0.0f32; 5];
    let mut offset = 6;
    for coeff in &mut coeffs {
        let bytes: [u8; 4] = payload.get(offset..offset + 4)?.try_into().ok()?;
        *coeff = minidsp_protocol::dialect::decode_value(&bytes, dialect);
        offset += 4;
    }
    Some((addr, coeffs))
}

fn parse_raw_bypass_filter(payload: &[u8]) -> Option<(u16, bool)> {
    if payload.first().copied()? != commands::CMD_BYPASS_FILTER || payload.len() < 6 {
        return None;
    }
    let addr = u16::from_be_bytes([payload[2], payload[3]]);
    Some((addr, payload[1] & 0x80 != 0))
}

fn parse_raw_selector_write(payload: &[u8], dialect: &Dialect) -> Option<(u16, u16)> {
    if dialect.value_format != ValueFormat::Float32 {
        return None;
    }
    if payload.first().copied()? != commands::CMD_WRITE_DSP || payload.len() < 8 {
        return None;
    }
    if payload[1] != 0x80 || payload[6] != 0 || payload[7] != 0 {
        return None;
    }
    let addr = u16::from_be_bytes([payload[2], payload[3]]);
    let value = u16::from_le_bytes([payload[4], payload[5]]);
    Some((addr, value))
}

fn parse_raw_linked_selector_write(payload: &[u8], dialect: &Dialect) -> Option<(u16, u16)> {
    if dialect.value_format != ValueFormat::Float32 {
        return None;
    }
    if payload.first().copied()? != commands::CMD_WRITE_BIQUAD || payload.len() < 10 {
        return None;
    }
    let ctrl = payload[1];
    if ctrl < 0x80 {
        return None;
    }
    let linked_count = (ctrl - 0x80) as usize;
    let linked_bytes = if linked_count == 0 {
        2
    } else {
        linked_count * 2
    };
    let value_offset = 4 + linked_bytes;
    let value_bytes: [u8; 4] = payload
        .get(value_offset..value_offset + 4)?
        .try_into()
        .ok()?;
    let addr = u16::from_be_bytes([payload[2], payload[3]]);
    Some((addr, u32::from_le_bytes(value_bytes) as u16))
}

/// Binary-encode the PEQ shadow for NVS persistence. Layout:
///
/// ```text
/// [0]       : format version (0x01)
/// [1..3]    : little-endian u16 band count
/// [3..]     : band count × 23 bytes
///   [0..2]    : LE u16 address
///   [2..22]   : 5 × LE f32 biquad coefficients
///   [22]      : bypass flag (0 = off, 1 = on)
/// ```
///
/// Entries are sorted by address so two back-to-back encodes of the same
/// logical state produce the same blob — avoids unnecessary NVS writes.
fn encode_peq_shadow(shadow: &HashMap<u16, PeqShadow>) -> Vec<u8> {
    let mut sorted: Vec<_> = shadow.iter().collect();
    sorted.sort_by_key(|(addr, _)| **addr);

    let mut buf = Vec::with_capacity(3 + sorted.len() * 23);
    buf.push(PEQ_SHADOW_FORMAT_V1);
    buf.extend_from_slice(&(sorted.len() as u16).to_le_bytes());
    for (&addr, entry) in sorted {
        buf.extend_from_slice(&addr.to_le_bytes());
        for coeff in entry.coeffs {
            buf.extend_from_slice(&coeff.to_le_bytes());
        }
        buf.push(if entry.bypass { 1 } else { 0 });
    }
    buf
}

/// Decode a PEQ shadow blob produced by [`encode_peq_shadow`]. Returns
/// `None` on any structural error — the caller should log and fall back to
/// the default shadow.
fn decode_peq_shadow(data: &[u8]) -> Option<HashMap<u16, PeqShadow>> {
    if data.len() < 3 || data[0] != PEQ_SHADOW_FORMAT_V1 {
        return None;
    }
    let count = u16::from_le_bytes([data[1], data[2]]) as usize;
    let expected_len = 3 + count * 23;
    if data.len() < expected_len {
        return None;
    }

    let mut shadow = HashMap::with_capacity(count);
    let mut offset = 3;
    for _ in 0..count {
        let addr = u16::from_le_bytes([data[offset], data[offset + 1]]);
        offset += 2;

        let mut coeffs = [0.0f32; 5];
        for coeff in &mut coeffs {
            let bytes = [
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
            ];
            *coeff = f32::from_le_bytes(bytes);
            offset += 4;
        }

        let bypass = data[offset] != 0;
        offset += 1;

        shadow.insert(addr, PeqShadow { coeffs, bypass });
    }
    Some(shadow)
}

#[cfg(test)]
mod peq_shadow_tests {
    use super::*;

    #[test]
    fn roundtrip_is_lossless() {
        let mut original = HashMap::new();
        original.insert(
            0x1000,
            PeqShadow {
                coeffs: [1.0, 0.5, -0.25, 0.125, -0.0625],
                bypass: false,
            },
        );
        original.insert(
            0x1001,
            PeqShadow {
                coeffs: [0.0; 5],
                bypass: true,
            },
        );

        let blob = encode_peq_shadow(&original);
        let decoded = decode_peq_shadow(&blob).expect("decode");
        assert_eq!(decoded.len(), 2);
        let entry = decoded[&0x1000];
        assert_eq!(entry.coeffs, [1.0, 0.5, -0.25, 0.125, -0.0625]);
        assert!(!entry.bypass);
        let bypassed = decoded[&0x1001];
        assert!(bypassed.bypass);
    }

    #[test]
    fn decode_rejects_wrong_version() {
        let blob = vec![0xFF, 0, 0];
        assert!(decode_peq_shadow(&blob).is_none());
    }

    #[test]
    fn decode_rejects_truncated() {
        let blob = vec![PEQ_SHADOW_FORMAT_V1, 2, 0];
        assert!(decode_peq_shadow(&blob).is_none());
    }

    #[test]
    fn sanitize_shadow_preferred_scalar_clamps_placeholder_and_absurd_values() {
        assert_eq!(sanitize_shadow_preferred_scalar(0), Some(0.0));
        assert_eq!(sanitize_shadow_preferred_scalar(2), Some(0.0));
        assert_eq!(sanitize_shadow_preferred_scalar(0x4A03_9901), Some(0.0));
        assert_eq!(sanitize_shadow_preferred_scalar(9.5f32.to_bits()), None);
    }

    #[test]
    fn parse_raw_selector_write_extracts_selector_value() {
        let payload = commands::write_dsp_u32_le_with_ctrl(0x1234, 2, 0x80);
        let parsed = parse_raw_selector_write(&payload, &Dialect::NEW);
        assert_eq!(parsed, Some((0x1234, 2)));
    }

    #[test]
    fn parse_raw_linked_selector_write_extracts_primary_value() {
        let payload = commands::write_dsp_linked_u32(0x1234, 3, &[0x2000, 0x2001]);
        let parsed = parse_raw_linked_selector_write(&payload, &Dialect::NEW);
        assert_eq!(parsed, Some((0x1234, 3)));
    }
}
