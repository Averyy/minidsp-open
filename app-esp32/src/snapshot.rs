use crate::device::{CachedPeqShadow, DeviceState, PresetShadowState};
use crate::usb_hid::UsbHidError;
use log::info;
use minidsp_protocol::commands;
use minidsp_protocol::default_preset::{default_preset_image_for_dsp, DefaultPresetImage};
use minidsp_protocol::device::dsp::{Compressor, FirFilter};
use minidsp_protocol::device::DeviceProfile;
use minidsp_protocol::status::DeviceConfig;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

const SNAPSHOT_FORMAT_VERSION: u32 = 1;
const RECOVERY_MODIFICATION_TOKEN: [u8; 4] = [0xFE, 0xEF, 0x8F, 0xCF];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceSnapshot {
    pub format_version: u32,
    pub captured_at: String,
    pub tool: SnapshotTool,
    pub device: SnapshotDevice,
    pub transport: SnapshotTransport,
    pub master: SnapshotMasterState,
    pub presets: Vec<PresetSnapshot>,
    pub completeness: SnapshotCompleteness,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

impl DeviceSnapshot {
    pub fn is_strict_complete(&self) -> bool {
        self.completeness.strict
    }

    pub fn preset_by_slot(&self, slot: u8) -> Option<&PresetSnapshot> {
        self.presets.iter().find(|preset| preset.slot == slot)
    }

    pub fn semantically_matches(&self, other: &Self) -> Result<(), Vec<String>> {
        let mut mismatches = Vec::new();

        if self.device.serial != other.device.serial {
            mismatches.push(format!(
                "serial mismatch: expected {}, got {}",
                self.device.serial, other.device.serial
            ));
        }
        if self.device.hw_id != other.device.hw_id {
            mismatches.push(format!(
                "hw_id mismatch: expected {}, got {}",
                self.device.hw_id, other.device.hw_id
            ));
        }
        if self.device.dsp_version != other.device.dsp_version {
            mismatches.push(format!(
                "dsp_version mismatch: expected {}, got {}",
                self.device.dsp_version, other.device.dsp_version
            ));
        }
        if self.master.active_preset != other.master.active_preset {
            mismatches.push(format!(
                "active preset mismatch: expected {}, got {}",
                self.master.active_preset, other.master.active_preset
            ));
        }
        if self.master.source != other.master.source {
            mismatches.push(format!(
                "source mismatch: expected {}, got {}",
                self.master.source, other.master.source
            ));
        }
        if !approx_equal(self.master.volume, other.master.volume, 0.001) {
            mismatches.push(format!(
                "volume mismatch: expected {:.6}, got {:.6}",
                self.master.volume, other.master.volume
            ));
        }
        if self.master.mute != other.master.mute {
            mismatches.push(format!(
                "mute mismatch: expected {}, got {}",
                self.master.mute, other.master.mute
            ));
        }
        if self.master.dirac != other.master.dirac {
            mismatches.push(format!(
                "dirac mismatch: expected {:?}, got {:?}",
                self.master.dirac, other.master.dirac
            ));
        }

        for expected in &self.presets {
            let Some(actual) = other.preset_by_slot(expected.slot) else {
                mismatches.push(format!("missing preset {}", expected.slot));
                continue;
            };
            if expected.token != actual.token {
                mismatches.push(format!(
                    "preset {} token mismatch: expected {:?}, got {:?}",
                    expected.slot, expected.token, actual.token
                ));
            }
            if expected.shadow != actual.shadow {
                mismatches.push(format!("preset {} shadow mismatch", expected.slot));
            }
            if expected.fir_blocks.len() != actual.fir_blocks.len() {
                mismatches.push(format!(
                    "preset {} fir block count mismatch: expected {}, got {}",
                    expected.slot,
                    expected.fir_blocks.len(),
                    actual.fir_blocks.len()
                ));
            }
            for expected_block in &expected.fir_blocks {
                let Some(actual_block) = actual
                    .fir_blocks
                    .iter()
                    .find(|block| block.data_start == expected_block.data_start)
                else {
                    mismatches.push(format!(
                        "preset {} missing FIR block at addr {}",
                        expected.slot, expected_block.data_start
                    ));
                    continue;
                };
                if expected_block.coefficients.len() != actual_block.coefficients.len() {
                    mismatches.push(format!(
                        "preset {} FIR block {} length mismatch: expected {}, got {}",
                        expected.slot,
                        expected_block.data_start,
                        expected_block.coefficients.len(),
                        actual_block.coefficients.len()
                    ));
                    continue;
                }
                if expected_block
                    .coefficients
                    .iter()
                    .zip(&actual_block.coefficients)
                    .any(|(left, right)| !approx_equal(*left, *right, 1e-6))
                {
                    mismatches.push(format!(
                        "preset {} FIR block {} coefficients mismatch",
                        expected.slot, expected_block.data_start
                    ));
                }
            }
        }

        if mismatches.is_empty() {
            Ok(())
        } else {
            Err(mismatches)
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotTool {
    pub name: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_commit: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotDevice {
    pub serial: u32,
    pub hw_id: u8,
    pub dsp_version: u8,
    pub fw_major: u8,
    pub fw_minor: u8,
    pub product_name: String,
    pub preset_count: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotTransport {
    pub kind: String,
    pub target: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotMasterState {
    pub active_preset: u8,
    pub source: String,
    pub volume: f32,
    pub mute: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dirac: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresetSnapshot {
    pub slot: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    pub complete: bool,
    pub provenance: Vec<SnapshotProvenance>,
    pub completeness: SnapshotCompleteness,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shadow: Option<PresetShadowState>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fir_blocks: Vec<SnapshotFirBlock>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotFirBlock {
    pub data_start: u16,
    pub coefficients: Vec<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SnapshotProvenance {
    ControllerShadow,
    LiveDeviceConfig,
    TokenMatchedOfficialXml,
    DefaultPresetImage,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotCompleteness {
    pub strict: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<String>,
}

#[derive(Debug, Clone, Default)]
struct PresetSource {
    shadow: Option<PresetShadowState>,
    fir_blocks: Vec<SnapshotFirBlock>,
    provenance: Vec<SnapshotProvenance>,
    issues: Vec<String>,
}

impl DeviceState {
    pub fn capture_device_snapshot(&self) -> Result<DeviceSnapshot, UsbHidError> {
        let _guard = self
            .try_begin_operation()
            .ok_or_else(|| UsbHidError::Protocol("device busy".into()))?;
        self.capture_device_snapshot_locked()
    }

    pub fn learn_device_snapshot(&self) -> Result<DeviceSnapshot, UsbHidError> {
        let _guard = self
            .try_begin_operation()
            .ok_or_else(|| UsbHidError::Protocol("device busy".into()))?;
        self.learn_device_snapshot_locked()
    }

    pub fn restore_device_snapshot(&self, snapshot: &DeviceSnapshot) -> Result<(), UsbHidError> {
        let _guard = self
            .try_begin_operation()
            .ok_or_else(|| UsbHidError::Protocol("device busy".into()))?;
        self.restore_device_snapshot_locked(snapshot)
    }

    fn capture_device_snapshot_locked(&self) -> Result<DeviceSnapshot, UsbHidError> {
        let active_config = self.read_device_config()?;
        let master = self.get_master_status()?;
        let profile = self.profile();
        let token_map = self.read_modification_tokens()?;
        let shadows = self.export_shadow_snapshots();
        let trusted = self.trusted_snapshot_slots();
        let default_image = default_preset_image_for_dsp(self.dsp_version());

        let mut presets = Vec::with_capacity(profile.presets as usize);
        let mut issues = Vec::new();
        let mut notes = vec![
            "ESP32 snapshot export is controller-shadow-backed and will refuse strict export for presets the board has not learned truthfully.".to_string(),
        ];

        if !trusted.is_empty() {
            notes.push(
                "Trusted preset snapshots came from prior snapshot import/restore, explicit snapshot learning, or device-default rebuilds on this board.".to_string(),
            );
        }
        info!("snapshot export trusted presets: {:?}", trusted);

        for slot in 0..profile.presets {
            let token = token_map.get(&slot).copied();
            let source = if trusted.contains(&slot) {
                match shadows.get(&slot) {
                    Some(shadow) => build_shadow_preset_source(profile, shadow),
                    None => PresetSource {
                        issues: vec![format!(
                            "preset {slot} is marked trusted but no controller shadow is cached"
                        )],
                        ..PresetSource::default()
                    },
                }
            } else if token == Some(RECOVERY_MODIFICATION_TOKEN) {
                build_default_preset_source(profile, default_image)
            } else {
                PresetSource {
                    issues: vec![
                        "no trusted controller snapshot for this preset on the ESP32 target"
                            .to_string(),
                    ],
                    ..PresetSource::default()
                }
            };

            let mut preset_issues = Vec::new();
            if token.is_none() {
                preset_issues.push("missing modification token".to_string());
            }
            preset_issues.extend(source.issues);
            let strict = preset_issues.is_empty();
            for issue in &preset_issues {
                issues.push(format!("preset {}: {issue}", slot));
            }

            presets.push(PresetSnapshot {
                slot,
                token: token.map(hex_token),
                complete: strict,
                provenance: source.provenance,
                completeness: SnapshotCompleteness {
                    strict,
                    issues: preset_issues,
                },
                shadow: source.shadow.map(snapshot_shadow_payload),
                fir_blocks: source.fir_blocks,
            });
        }

        Ok(DeviceSnapshot {
            format_version: SNAPSHOT_FORMAT_VERSION,
            captured_at: snapshot_timestamp(),
            tool: SnapshotTool {
                name: "minidsp-open".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                git_commit: option_env!("MINIDSP_OPEN_GIT_COMMIT").map(str::to_string),
            },
            device: SnapshotDevice {
                serial: self.serial(),
                hw_id: self.hw_id().hw_id,
                dsp_version: self.dsp_version(),
                fw_major: self.hw_id().fw_major,
                fw_minor: self.hw_id().fw_minor,
                product_name: self.product_name().to_string(),
                preset_count: profile.presets,
            },
            transport: SnapshotTransport {
                kind: "bridge".to_string(),
                target: "esp32://self".to_string(),
            },
            master: SnapshotMasterState {
                active_preset: master.preset,
                source: active_config.master_status.source,
                volume: active_config.master_status.volume,
                mute: active_config.master_status.mute,
                dirac: active_config.master_status.dirac,
            },
            presets,
            completeness: SnapshotCompleteness {
                strict: issues.is_empty(),
                issues,
            },
            notes,
        })
    }

    fn learn_device_snapshot_locked(&self) -> Result<DeviceSnapshot, UsbHidError> {
        let original_preset = self.get_master_status()?.preset;
        let profile = self.profile();
        let token_map = self.read_modification_tokens()?;

        let learn_result = (|| -> Result<(), UsbHidError> {
            for slot in 0..profile.presets {
                self.set_config_with_verify(slot, commands::PRESET_CHANGE_WITHOUT_RESET)?;
                let (shadow, source) = capture_live_preset_source(self)?;
                let trust_live = token_map.contains_key(&slot) && !profile.has_unreliable_selector_readback;
                info!(
                    "snapshot learn slot {} trust_live={} issues={:?}",
                    slot, trust_live, source.issues
                );
                self.store_preset_shadow_snapshot(slot, shadow);
                if trust_live {
                    self.mark_snapshot_trusted(slot);
                }
            }
            Ok(())
        })();

        info!(
            "snapshot learn trusted presets after sweep: {:?}",
            self.trusted_snapshot_slots()
        );

        let restore_result =
            self.set_config_with_verify(original_preset, commands::PRESET_CHANGE_WITHOUT_RESET);

        match (learn_result, restore_result) {
            (Err(err), _) => Err(err),
            (Ok(()), Err(err)) => Err(err),
            (Ok(()), Ok(())) => self.capture_device_snapshot_locked(),
        }
    }

    fn restore_device_snapshot_locked(&self, snapshot: &DeviceSnapshot) -> Result<(), UsbHidError> {
        validate_snapshot_target(self, snapshot)?;
        if !snapshot.is_strict_complete() {
            return Err(UsbHidError::Protocol(
                "refusing to import an incomplete snapshot".to_string(),
            ));
        }

        let mut presets = snapshot.presets.clone();
        presets.sort_by_key(|preset| preset.slot);
        for preset in &presets {
            self.restore_preset_snapshot(preset)?;
        }

        self.restore_master_state(&snapshot.master)?;

        let verified = self.capture_device_snapshot_locked()?;
        match snapshot.semantically_matches(&verified) {
            Ok(()) => Ok(()),
            Err(mismatches) => Err(UsbHidError::Protocol(format!(
                "post-restore verification failed: {}",
                mismatches.join("; ")
            ))),
        }
    }

    fn restore_preset_snapshot(&self, preset: &PresetSnapshot) -> Result<(), UsbHidError> {
        let mut shadow = preset.shadow.as_ref().cloned().ok_or_else(|| {
            UsbHidError::Protocol(format!(
                "preset {} has no restorable shadow payload",
                preset.slot
            ))
        })?;
        shadow.fir_blocks = preset
            .fir_blocks
            .iter()
            .map(|block| (block.data_start, block.coefficients.clone()))
            .collect();

        self.set_config_with_verify(preset.slot, commands::PRESET_CHANGE_WITHOUT_RESET)?;
        apply_shadow_to_active_preset(self, &shadow)?;

        if let Some(token) = preset.token.as_deref() {
            self.write_preset_token_exact(preset.slot, parse_hex_token(token)?)?;
        }
        self.store_preset_shadow_snapshot(preset.slot, shadow);
        self.mark_snapshot_trusted(preset.slot);
        Ok(())
    }

    fn restore_master_state(&self, master: &SnapshotMasterState) -> Result<(), UsbHidError> {
        self.set_config_with_verify(master.active_preset, commands::PRESET_CHANGE_WITHOUT_RESET)?;

        let source = minidsp_protocol::source::Source::from_str_pascal(&master.source)
            .or_else(|| minidsp_protocol::source::Source::from_str_lowercase(&master.source))
            .ok_or_else(|| UsbHidError::Protocol(format!("unknown source '{}'", master.source)))?;
        let source_idx = self
            .profile()
            .sources
            .index_for_source(source)
            .ok_or_else(|| {
                UsbHidError::Protocol(format!(
                    "source '{}' not available on this device",
                    master.source
                ))
            })?;
        self.set_source(source_idx)?;
        self.set_volume_db(master.volume)?;
        self.set_mute(master.mute)?;
        if let Some(dirac) = master.dirac {
            self.set_dirac_bypass(!dirac)?;
        }
        Ok(())
    }
}

fn build_shadow_preset_source(profile: &DeviceProfile, shadow: &PresetShadowState) -> PresetSource {
    let mut issues = Vec::new();
    let fir_blocks = collect_fir_blocks_from_shadow(profile, shadow, &mut issues);
    PresetSource {
        shadow: Some(shadow.clone()),
        fir_blocks,
        provenance: vec![SnapshotProvenance::ControllerShadow],
        issues,
    }
}

fn build_live_preset_source(profile: &DeviceProfile, shadow: &PresetShadowState) -> PresetSource {
    let mut issues = Vec::new();
    if profile.has_unreliable_selector_readback {
        issues.push(
            "active preset live readback is not authoritative on this device family without prior shadow truth".to_string(),
        );
    }
    let fir_blocks = collect_fir_blocks_from_shadow(profile, shadow, &mut issues);
    PresetSource {
        shadow: Some(snapshot_shadow_payload(shadow.clone())),
        fir_blocks,
        provenance: vec![SnapshotProvenance::LiveDeviceConfig],
        issues,
    }
}

fn snapshot_shadow_payload(shadow: PresetShadowState) -> PresetShadowState {
    let mut shadow = shadow;
    shadow.fir_blocks.clear();
    shadow
}

fn build_default_preset_source(
    profile: &DeviceProfile,
    image: Option<&'static DefaultPresetImage>,
) -> PresetSource {
    let Some(image) = image else {
        return PresetSource {
            issues: vec!["no bundled default preset image for this DSP family".to_string()],
            ..PresetSource::default()
        };
    };

    let shadow = crate::device::default_preset_shadow(profile, image);
    let mut issues = Vec::new();
    let fir_blocks = collect_fir_blocks_from_shadow(profile, &shadow, &mut issues);

    PresetSource {
        shadow: Some(shadow),
        fir_blocks,
        provenance: vec![SnapshotProvenance::DefaultPresetImage],
        issues,
    }
}

fn validate_snapshot_target(
    device: &DeviceState,
    snapshot: &DeviceSnapshot,
) -> Result<(), UsbHidError> {
    if snapshot.device.serial != device.serial() {
        return Err(UsbHidError::Protocol(format!(
            "snapshot serial {} does not match device serial {}",
            snapshot.device.serial,
            device.serial()
        )));
    }
    if snapshot.device.hw_id != device.hw_id().hw_id {
        return Err(UsbHidError::Protocol(format!(
            "snapshot hw_id {} does not match device hw_id {}",
            snapshot.device.hw_id,
            device.hw_id().hw_id
        )));
    }
    if snapshot.device.dsp_version != device.dsp_version() {
        return Err(UsbHidError::Protocol(format!(
            "snapshot dsp_version {} does not match device dsp_version {}",
            snapshot.device.dsp_version,
            device.dsp_version()
        )));
    }
    Ok(())
}

fn capture_live_preset_source(
    device: &DeviceState,
) -> Result<(PresetShadowState, PresetSource), UsbHidError> {
    let config = device.read_device_config()?;
    let mut shadow = shadow_from_device_config(device.profile(), &config);
    populate_live_fir_blocks(device, &mut shadow)?;
    let source = build_live_preset_source(device.profile(), &shadow);
    Ok((shadow, source))
}

fn populate_live_fir_blocks(
    device: &DeviceState,
    shadow: &mut PresetShadowState,
) -> Result<(), UsbHidError> {
    let Some(table) = device.profile().dsp_table else {
        return Ok(());
    };

    for input in table.inputs {
        if let Some(fir) = input.fir {
            if let Some(block) = read_live_fir_block(device, shadow, fir)? {
                shadow.fir_blocks.insert(block.data_start, block.coefficients);
            }
        }
    }
    for output in table.outputs {
        if let Some(fir) = output.fir {
            if let Some(block) = read_live_fir_block(device, shadow, fir)? {
                shadow.fir_blocks.insert(block.data_start, block.coefficients);
            }
        }
    }
    Ok(())
}

fn read_live_fir_block(
    device: &DeviceState,
    shadow: &PresetShadowState,
    fir: FirFilter,
) -> Result<Option<SnapshotFirBlock>, UsbHidError> {
    let taps = shadow.selector.get(&fir.taps_count).copied().unwrap_or(0) as usize;
    if taps == 0 {
        return Ok(None);
    }

    let mut coefficients = Vec::with_capacity(taps);
    let mut offset = 0usize;
    while offset < taps {
        let chunk = (taps - offset).min(14);
        let values = device.read_dsp_floats(fir.data_start + offset as u16, chunk as u8)?;
        if values.len() != chunk {
            return Err(UsbHidError::Protocol(format!(
                "short FIR read at data_start {}: expected {} floats, got {}",
                fir.data_start + offset as u16,
                chunk,
                values.len()
            )));
        }
        coefficients.extend(values);
        offset += chunk;
    }

    Ok(Some(SnapshotFirBlock {
        data_start: fir.data_start,
        coefficients,
    }))
}

fn apply_shadow_to_active_preset(
    device: &DeviceState,
    shadow: &PresetShadowState,
) -> Result<(), UsbHidError> {
    let profile = device.profile();
    let table = profile
        .dsp_table
        .ok_or_else(|| UsbHidError::Protocol("snapshot restore unsupported".into()))?;
    let mut handled_peq = HashSet::new();
    let mut handled_selector = HashSet::new();
    let mut handled_scalar = HashSet::new();

    for input in table.inputs {
        if let Some(mute_addr) = input.mute {
            if let Some(value) = shadow.selector.get(&mute_addr).copied() {
                device.set_channel_mute(mute_addr, value == 1)?;
                handled_selector.insert(mute_addr);
            }
        }
        if let Some(gain_addr) = input.gain {
            if let Some(raw) = shadow.scalar.get(&gain_addr).copied() {
                device.set_channel_gain_persisted(gain_addr, raw_scalar_to_f32(profile, raw)?)?;
                handled_scalar.insert(gain_addr);
            }
        }
        if let Some(polarity_addr) = input.polarity {
            if let Some(value) = shadow.selector.get(&polarity_addr).copied() {
                device.set_invert(polarity_addr, value != 0)?;
                handled_selector.insert(polarity_addr);
            }
        }
        for band in input.peq {
            if let Some(entry) = shadow.peq.get(&band.addr) {
                device.write_biquad(band.addr, &entry.coeffs)?;
                device.bypass_filter(band.addr, entry.bypass)?;
                handled_peq.insert(band.addr);
            }
        }
        if let Some(fir) = input.fir {
            apply_fir_state(device, shadow, fir, &mut handled_selector)?;
        }
    }

    for output in table.outputs {
        if let Some(value) = shadow.selector.get(&output.mute).copied() {
            device.set_channel_mute(output.mute, value == 1)?;
            handled_selector.insert(output.mute);
        }
        if let Some(raw) = shadow.scalar.get(&output.gain).copied() {
            device.set_channel_gain_persisted(output.gain, raw_scalar_to_f32(profile, raw)?)?;
            handled_scalar.insert(output.gain);
        }
        if let Some(delay_addr) = output.delay {
            if let Some(raw) = shadow.scalar.get(&delay_addr).copied() {
                device.set_channel_delay_samples_persisted(delay_addr, raw)?;
                handled_scalar.insert(delay_addr);
            }
        }
        if let Some(polarity_addr) = output.polarity {
            if let Some(value) = shadow.selector.get(&polarity_addr).copied() {
                device.set_invert(polarity_addr, value != 0)?;
                handled_selector.insert(polarity_addr);
            }
        }
        for band in output.peq {
            if let Some(entry) = shadow.peq.get(&band.addr) {
                device.write_biquad(band.addr, &entry.coeffs)?;
                device.bypass_filter(band.addr, entry.bypass)?;
                handled_peq.insert(band.addr);
            }
        }
        if let Some(crossover) = output.crossover {
            for base in [crossover.lpf_addr, crossover.hpf_addr] {
                for idx in 0..4u16 {
                    let addr = base + idx * 5;
                    if let Some(entry) = shadow.peq.get(&addr) {
                        device.write_biquad(addr, &entry.coeffs)?;
                        handled_peq.insert(addr);
                    }
                }
                if let Some(entry) = shadow.peq.get(&base) {
                    device.bypass_filter(base, entry.bypass)?;
                }
            }
        }
        if let Some(fir) = output.fir {
            apply_fir_state(device, shadow, fir, &mut handled_selector)?;
        }
        if let Some(comp) = output.compressor {
            apply_compressor_state(
                device,
                shadow,
                comp,
                &mut handled_selector,
                &mut handled_scalar,
            )?;
        }
    }

    apply_routing_state(device, shadow, &mut handled_selector, &mut handled_scalar)?;

    let unhandled_peq: Vec<u16> = shadow
        .peq
        .keys()
        .filter(|addr| !handled_peq.contains(addr))
        .copied()
        .collect();
    let unhandled_selector: Vec<u16> = shadow
        .selector
        .keys()
        .filter(|addr| !handled_selector.contains(addr))
        .copied()
        .collect();
    let unhandled_scalar: Vec<u16> = shadow
        .scalar
        .keys()
        .filter(|addr| !handled_scalar.contains(addr))
        .copied()
        .collect();
    if !unhandled_peq.is_empty() || !unhandled_selector.is_empty() || !unhandled_scalar.is_empty() {
        return Err(UsbHidError::Protocol(format!(
            "snapshot restore left unhandled addresses: peq={unhandled_peq:?}, selector={unhandled_selector:?}, scalar={unhandled_scalar:?}"
        )));
    }

    Ok(())
}

fn apply_compressor_state(
    device: &DeviceState,
    shadow: &PresetShadowState,
    comp: Compressor,
    handled_selector: &mut HashSet<u16>,
    handled_scalar: &mut HashSet<u16>,
) -> Result<(), UsbHidError> {
    if let Some(value) = shadow.selector.get(&comp.bypass).copied() {
        device.set_status_bypass(comp.bypass, matches!(value, 1 | 3))?;
        handled_selector.insert(comp.bypass);
    }
    for (addr, label) in [
        (comp.threshold, "compressor threshold"),
        (comp.gain, "compressor gain"),
        (comp.ratio, "compressor ratio"),
        (comp.knee, "compressor knee"),
        (comp.attack, "compressor attack"),
        (comp.release, "compressor release"),
    ] {
        if let Some(raw) = shadow.scalar.get(&addr).copied() {
            device.write_dsp_verified(addr, f32::from_bits(raw), true, label)?;
            handled_scalar.insert(addr);
        }
    }
    Ok(())
}

fn apply_routing_state(
    device: &DeviceState,
    shadow: &PresetShadowState,
    handled_selector: &mut HashSet<u16>,
    handled_scalar: &mut HashSet<u16>,
) -> Result<(), UsbHidError> {
    let table = device
        .profile()
        .dsp_table
        .ok_or_else(|| UsbHidError::Protocol("routing unsupported".into()))?;
    let routing = table.routing;
    let span = routing.rows as u16 * routing.cols as u16;
    for offset in 0..span {
        let input = (offset / routing.cols as u16) as u8;
        let output = (offset % routing.cols as u16) as u8;
        let status_addr = routing.status_base + offset;
        let gain_addr = routing.gain_base + offset;
        let gain_raw = shadow.scalar.get(&gain_addr).copied().unwrap_or(0);
        let gain_db = raw_scalar_to_f32(device.profile(), gain_raw)?;
        let enabled = match shadow.selector.get(&status_addr).copied() {
            Some(raw)
                if device.profile().dialect.value_format
                    == minidsp_protocol::dialect::ValueFormat::Float32 =>
            {
                raw == 2
            }
            Some(raw) => raw != 0,
            None => false,
        };
        device.set_routing_persisted(input, output, enabled, gain_db)?;
        handled_selector.insert(status_addr);
        handled_scalar.insert(gain_addr);

        if let Some(polarity_base) = routing.polarity_base {
            let polarity_addr = polarity_base + offset;
            if let Some(value) = shadow.selector.get(&polarity_addr).copied() {
                device.set_routing_polarity(input, output, value != 0)?;
                handled_selector.insert(polarity_addr);
            }
        }
    }
    Ok(())
}

fn apply_fir_state(
    device: &DeviceState,
    shadow: &PresetShadowState,
    fir: FirFilter,
    handled_selector: &mut HashSet<u16>,
) -> Result<(), UsbHidError> {
    let taps = shadow.selector.get(&fir.taps_count).copied().unwrap_or(0);
    let bypass_raw = shadow.selector.get(&fir.bypass).copied().unwrap_or(3);
    let bypass = matches!(bypass_raw, 1 | 3);

    if taps > 0 {
        let coefficients = shadow.fir_blocks.get(&fir.data_start).ok_or_else(|| {
            UsbHidError::Protocol(format!(
                "missing FIR coefficients for data_start {}",
                fir.data_start
            ))
        })?;
        if coefficients.len() < taps as usize {
            return Err(UsbHidError::Protocol(format!(
                "FIR coefficient block at {} is shorter than active tap count {}",
                fir.data_start, taps
            )));
        }
        device.load_fir_coefficients(fir, &coefficients[..taps as usize])?;
    } else {
        device.write_dsp_int_verified(fir.taps_count, 0, "set fir tap count")?;
    }
    device.set_status_bypass(fir.bypass, bypass)?;
    handled_selector.insert(fir.taps_count);
    handled_selector.insert(fir.bypass);
    Ok(())
}

fn collect_fir_blocks_from_shadow(
    profile: &DeviceProfile,
    shadow: &PresetShadowState,
    issues: &mut Vec<String>,
) -> Vec<SnapshotFirBlock> {
    let mut blocks = Vec::new();
    let Some(table) = profile.dsp_table else {
        return blocks;
    };

    for input in table.inputs {
        if let Some(fir) = input.fir {
            if let Some(block) = required_fir_block(shadow, fir, issues) {
                blocks.push(block);
            }
        }
    }
    for output in table.outputs {
        if let Some(fir) = output.fir {
            if let Some(block) = required_fir_block(shadow, fir, issues) {
                blocks.push(block);
            }
        }
    }
    blocks
}

fn required_fir_block(
    shadow: &PresetShadowState,
    fir: FirFilter,
    issues: &mut Vec<String>,
) -> Option<SnapshotFirBlock> {
    let taps = shadow.selector.get(&fir.taps_count).copied().unwrap_or(0) as usize;
    if taps == 0 {
        return None;
    }
    let Some(coefficients) = shadow.fir_blocks.get(&fir.data_start) else {
        issues.push(format!(
            "missing FIR coefficient block for data_start {} with {} active taps",
            fir.data_start, taps
        ));
        return None;
    };
    if coefficients.len() < taps {
        issues.push(format!(
            "FIR coefficient block at data_start {} is shorter than active tap count {}",
            fir.data_start, taps
        ));
        return None;
    }
    Some(SnapshotFirBlock {
        data_start: fir.data_start,
        coefficients: coefficients[..taps].to_vec(),
    })
}

fn raw_scalar_to_f32(profile: &DeviceProfile, raw: u32) -> Result<f32, UsbHidError> {
    use minidsp_protocol::dialect::ValueFormat;
    match profile.dialect.value_format {
        ValueFormat::Float32 => Ok(f32::from_bits(raw)),
        ValueFormat::FixedPoint => Err(UsbHidError::Protocol(
            "fixed-point snapshot restore is not supported".into(),
        )),
    }
}

fn snapshot_timestamp() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

fn shadow_from_device_config(profile: &DeviceProfile, config: &DeviceConfig) -> PresetShadowState {
    let mut shadow = PresetShadowState::default();
    let Some(table) = profile.dsp_table else {
        return shadow;
    };

    for input_cfg in &config.inputs {
        let Some(input) = table.inputs.get(input_cfg.index as usize) else {
            continue;
        };
        if let Some(addr) = input.gain {
            if let Some(gain) = input_cfg.gain {
                shadow.scalar.insert(addr, gain.to_bits());
            }
        }
        if let Some(addr) = input.mute {
            if let Some(mute) = input_cfg.mute {
                shadow.selector.insert(addr, if mute { 1 } else { 2 });
            }
        }
        if let Some(addr) = input.polarity {
            shadow.selector.insert(addr, 0);
        }
        for band in &input_cfg.peq {
            if let Some(def) = input.peq.get(band.index as usize) {
                shadow.peq.insert(
                    def.addr,
                    CachedPeqShadow {
                        coeffs: band.coeff.to_array(),
                        bypass: band.bypass,
                    },
                );
            }
        }
    }

    for output_cfg in &config.outputs {
        let Some(output) = table.outputs.get(output_cfg.index as usize) else {
            continue;
        };
        shadow
            .selector
            .insert(output.mute, if output_cfg.mute { 1 } else { 2 });
        shadow.scalar.insert(output.gain, output_cfg.gain.to_bits());
        if let Some(addr) = output.delay {
            shadow
                .scalar
                .insert(addr, profile.delay_ms_to_samples(output_cfg.delay) as u32);
        }
        if let Some(addr) = output.polarity {
            shadow.selector.insert(addr, 0);
        }
        for band in &output_cfg.peq {
            if let Some(def) = output.peq.get(band.index as usize) {
                shadow.peq.insert(
                    def.addr,
                    CachedPeqShadow {
                        coeffs: band.coeff.to_array(),
                        bypass: band.bypass,
                    },
                );
            }
        }
        for group in &output_cfg.crossover {
            if let Some(xover) = output.crossover {
                let base_addr = match group.index {
                    0 => xover.lpf_addr,
                    1 => xover.hpf_addr,
                    _ => continue,
                };
                for filter in &group.filters {
                    let addr = base_addr + filter.index as u16 * 5;
                    shadow.peq.insert(
                        addr,
                        CachedPeqShadow {
                            coeffs: filter.coeff.to_array(),
                            bypass: group.bypass,
                        },
                    );
                }
            }
        }
        if let Some(fir) = output.fir {
            if let Some(state) = &output_cfg.fir {
                shadow
                    .selector
                    .insert(fir.bypass, if state.bypass { 3 } else { 2 });
                shadow.selector.insert(fir.taps_count, state.active_taps);
            }
        }
        if let Some(comp) = output.compressor {
            if let Some(state) = &output_cfg.compressor {
                shadow
                    .selector
                    .insert(comp.bypass, if state.bypass { 3 } else { 2 });
                shadow
                    .scalar
                    .insert(comp.threshold, state.threshold.to_bits());
                shadow.scalar.insert(comp.gain, state.gain.to_bits());
                shadow.scalar.insert(comp.ratio, state.ratio.to_bits());
                shadow.scalar.insert(comp.knee, state.knee.to_bits());
                shadow.scalar.insert(comp.attack, state.attack.to_bits());
                shadow.scalar.insert(comp.release, state.release.to_bits());
            }
        }
    }

    for input_cfg in &config.inputs {
        for route in &input_cfg.routing {
            let flat_idx = input_cfg.index as u16 * table.routing.cols as u16 + route.output as u16;
            shadow.selector.insert(
                table.routing.status_base + flat_idx,
                if profile.dialect.value_format == minidsp_protocol::dialect::ValueFormat::Float32 {
                    if route.enabled { 2 } else { 1 }
                } else if route.enabled {
                    1
                } else {
                    0
                },
            );
            shadow
                .scalar
                .insert(table.routing.gain_base + flat_idx, route.gain_db.to_bits());
            if let Some(polarity_base) = table.routing.polarity_base {
                if let Some(inverted) = route.polarity_inverted {
                    shadow
                        .selector
                        .insert(polarity_base + flat_idx, if inverted { 1 } else { 0 });
                }
            }
        }
    }

    shadow
}

fn hex_token(token: [u8; 4]) -> String {
    format!(
        "{:02x}{:02x}{:02x}{:02x}",
        token[0], token[1], token[2], token[3]
    )
}

fn parse_hex_token(token: &str) -> Result<[u8; 4], UsbHidError> {
    if token.len() != 8 {
        return Err(UsbHidError::Protocol(format!(
            "invalid token '{token}': expected 8 hex chars"
        )));
    }
    let mut parsed = [0u8; 4];
    for (idx, chunk) in token.as_bytes().chunks(2).enumerate() {
        let text = std::str::from_utf8(chunk)
            .map_err(|_| UsbHidError::Protocol(format!("invalid token '{token}'")))?;
        parsed[idx] = u8::from_str_radix(text, 16).map_err(|_| {
            UsbHidError::Protocol(format!("invalid token '{token}': non-hex byte '{text}'"))
        })?;
    }
    Ok(parsed)
}

fn approx_equal(left: f32, right: f32, tolerance: f32) -> bool {
    (left - right).abs() <= tolerance
}
