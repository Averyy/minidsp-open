use crate::client::{ClientError, MiniDspClient, PresetShadowState};
use minidsp_protocol::commands;
use minidsp_protocol::default_preset::{DefaultPresetImage, default_preset_image_for_dsp};
use minidsp_protocol::device::DeviceProfile;
use minidsp_protocol::device::dsp::{Compressor, FirFilter};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

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
struct OfficialPresetFile {
    shadow: PresetShadowState,
    fir_blocks: HashMap<u16, Vec<f32>>,
}

#[derive(Debug, Clone, Default)]
struct PresetSource {
    shadow: Option<PresetShadowState>,
    fir_blocks: Vec<SnapshotFirBlock>,
    provenance: Vec<SnapshotProvenance>,
    issues: Vec<String>,
}

impl MiniDspClient {
    pub async fn capture_device_snapshot(&self) -> Result<DeviceSnapshot, ClientError> {
        self.refresh_modification_tokens().await?;
        let active_config = self.read_device_config().await?;
        let info = self.device_info();
        let shadow_state = self.export_shadow_state().await;
        let token_map = shadow_state.preset_tokens;
        let preset_shadows = shadow_state.presets;
        let trusted = shadow_state.trusted_snapshot_presets;
        let official_files = load_official_preset_files(
            self.profile(),
            &info.product_name,
            info.version.serial,
            &token_map,
        );

        let active_preset = active_config.master_status.preset;
        let mut active_preset_shadow = shadow_from_device_config(self.profile(), &active_config);
        populate_live_fir_blocks(self, &mut active_preset_shadow).await?;

        let mut presets = Vec::with_capacity(self.profile().presets as usize);
        let mut issues = Vec::new();
        let mut notes = Vec::new();
        let default_image = default_preset_image_for_dsp(self.dsp_version());

        for slot in 0..self.profile().presets {
            let token = token_map.get(&slot).copied();
            let source = if trusted.contains(&slot) {
                match preset_shadows.get(&slot) {
                    Some(shadow) => build_shadow_preset_source(self.profile(), shadow),
                    None => PresetSource {
                        issues: vec![format!(
                            "preset {slot} is marked trusted but no controller snapshot is cached"
                        )],
                        ..PresetSource::default()
                    },
                }
            } else if token == Some(RECOVERY_MODIFICATION_TOKEN) {
                build_default_preset_source(self.profile(), default_image)
            } else if let Some(file) = official_files.get(&slot) {
                build_official_xml_preset_source(self.profile(), file)
            } else if slot == active_preset {
                build_live_preset_source(self.profile(), &active_preset_shadow)
            } else {
                PresetSource {
                    issues: vec![
                        "no token-matched official XML or default-image source for this preset"
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
                shadow: source.shadow,
                fir_blocks: source.fir_blocks,
            });
        }

        if !official_files.is_empty() {
            notes.push(
                "Token-matched official Device Console setting files were used where available."
                    .to_string(),
            );
        }
        if !trusted.is_empty() {
            notes.push(
                "Trusted preset snapshots came from prior snapshot learn/import/restore on this controller.".to_string(),
            );
        }
        if token_map
            .values()
            .any(|token| *token == RECOVERY_MODIFICATION_TOKEN)
        {
            notes.push(
                "Presets carrying the recovery token were exported from the bundled default preset image."
                    .to_string(),
            );
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
                serial: info.version.serial,
                hw_id: info.version.hw_id,
                dsp_version: info.version.dsp_version,
                fw_major: info.version.fw_major,
                fw_minor: info.version.fw_minor,
                product_name: info.product_name,
                preset_count: self.profile().presets,
            },
            transport: snapshot_transport(self.transport_url()),
            master: SnapshotMasterState {
                active_preset,
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

    pub async fn learn_device_snapshot(&self) -> Result<DeviceSnapshot, ClientError> {
        let original_preset = self.get_master_status().await?.preset;
        let token_map = self.export_shadow_state().await.preset_tokens;

        let learn_result = async {
            for slot in 0..self.profile().presets {
                self.set_config(slot).await?;
                let (shadow, _source) = capture_live_preset_source(self).await?;
                let trust_live = token_map.contains_key(&slot)
                    && !self.profile().has_unreliable_selector_readback;
                self.store_preset_shadow_snapshot(slot, shadow).await;
                if trust_live {
                    self.mark_snapshot_trusted(slot).await;
                }
            }
            Ok::<(), ClientError>(())
        }
        .await;

        let restore_result = self.set_config(original_preset).await;
        match (learn_result, restore_result) {
            (Err(err), _) => Err(err),
            (Ok(()), Err(err)) => Err(err),
            (Ok(()), Ok(_)) => self.capture_device_snapshot().await,
        }
    }

    pub async fn restore_device_snapshot(
        &self,
        snapshot: &DeviceSnapshot,
    ) -> Result<(), ClientError> {
        validate_snapshot_target(self, snapshot)?;
        if !snapshot.is_strict_complete() {
            return Err(ClientError::Protocol(
                "refusing to import an incomplete snapshot".to_string(),
            ));
        }

        let mut presets = snapshot.presets.clone();
        presets.sort_by_key(|preset| preset.slot);
        for preset in &presets {
            self.restore_preset_snapshot(preset).await?;
        }

        self.restore_master_state(&snapshot.master).await?;

        let verified = self.capture_device_snapshot().await?;
        match snapshot.semantically_matches(&verified) {
            Ok(()) => Ok(()),
            Err(mismatches) => Err(ClientError::Protocol(format!(
                "post-restore verification failed: {}",
                mismatches.join("; ")
            ))),
        }
    }

    async fn restore_preset_snapshot(&self, preset: &PresetSnapshot) -> Result<(), ClientError> {
        let shadow = preset.shadow.as_ref().ok_or_else(|| {
            ClientError::Protocol(format!(
                "preset {} has no restorable shadow payload",
                preset.slot
            ))
        })?;

        self.set_config_with_mode(preset.slot, commands::PRESET_CHANGE_WITHOUT_RESET)
            .await?;
        apply_shadow_to_active_preset(self, shadow, &preset.fir_blocks).await?;

        if let Some(token) = preset.token.as_deref() {
            self.write_preset_token_exact(preset.slot, parse_hex_token(token)?)
                .await?;
        }
        let mut stored_shadow = shadow.clone();
        stored_shadow.fir_blocks = preset
            .fir_blocks
            .iter()
            .map(|block| (block.data_start, block.coefficients.clone()))
            .collect();
        self.store_preset_shadow_snapshot(preset.slot, stored_shadow)
            .await;
        self.mark_snapshot_trusted(preset.slot).await;
        Ok(())
    }

    async fn restore_master_state(&self, master: &SnapshotMasterState) -> Result<(), ClientError> {
        self.set_config_with_mode(master.active_preset, commands::PRESET_CHANGE_WITHOUT_RESET)
            .await?;

        let source = minidsp_protocol::source::Source::from_str_pascal(&master.source)
            .or_else(|| minidsp_protocol::source::Source::from_str_lowercase(&master.source))
            .ok_or_else(|| ClientError::Protocol(format!("unknown source '{}'", master.source)))?;
        let source_idx = self
            .profile()
            .sources
            .index_for_source(source)
            .ok_or_else(|| {
                ClientError::Protocol(format!(
                    "source '{}' not available on this device",
                    master.source
                ))
            })?;
        self.set_source(source_idx).await?;
        self.set_volume_db(master.volume).await?;
        self.set_mute(master.mute).await?;
        if let Some(dirac) = master.dirac {
            self.set_dirac_bypass(!dirac).await?;
        }
        Ok(())
    }
}

fn validate_snapshot_target(
    client: &MiniDspClient,
    snapshot: &DeviceSnapshot,
) -> Result<(), ClientError> {
    if snapshot.device.serial != client.serial() {
        return Err(ClientError::Protocol(format!(
            "snapshot serial {} does not match device serial {}",
            snapshot.device.serial,
            client.serial()
        )));
    }
    if snapshot.device.hw_id != client.hw_id().hw_id {
        return Err(ClientError::Protocol(format!(
            "snapshot hw_id {} does not match device hw_id {}",
            snapshot.device.hw_id,
            client.hw_id().hw_id
        )));
    }
    if snapshot.device.dsp_version != client.dsp_version() {
        return Err(ClientError::Protocol(format!(
            "snapshot dsp_version {} does not match device dsp_version {}",
            snapshot.device.dsp_version,
            client.dsp_version()
        )));
    }
    Ok(())
}

async fn apply_shadow_to_active_preset(
    client: &MiniDspClient,
    shadow: &PresetShadowState,
    fir_blocks: &[SnapshotFirBlock],
) -> Result<(), ClientError> {
    let profile = client.profile();
    let table = profile.dsp_table.ok_or(ClientError::Unsupported)?;
    let mut handled_peq = HashSet::new();
    let mut handled_selector = HashSet::new();
    let mut handled_scalar = HashSet::new();
    let fir_by_start: HashMap<u16, &SnapshotFirBlock> = fir_blocks
        .iter()
        .map(|block| (block.data_start, block))
        .collect();

    for input in table.inputs {
        if let Some(mute_addr) = input.mute
            && let Some(value) = shadow.selector.get(&mute_addr).copied()
        {
            client.set_channel_mute(mute_addr, value == 1).await?;
            handled_selector.insert(mute_addr);
        }
        if let Some(gain_addr) = input.gain
            && let Some(raw) = shadow.scalar.get(&gain_addr).copied()
        {
            client
                .set_channel_gain_persisted(gain_addr, raw_scalar_to_f32(profile, raw)?)
                .await?;
            handled_scalar.insert(gain_addr);
        }
        if let Some(polarity_addr) = input.polarity
            && let Some(value) = shadow.selector.get(&polarity_addr).copied()
        {
            client.set_invert(polarity_addr, value != 0).await?;
            handled_selector.insert(polarity_addr);
        }
        for band in input.peq {
            if let Some(entry) = shadow.peq.get(&band.addr) {
                client.write_biquad(band.addr, &entry.coeffs).await?;
                client.bypass_filter(band.addr, entry.bypass).await?;
                handled_peq.insert(band.addr);
            }
        }
        if let Some(fir) = input.fir {
            apply_fir_state(client, shadow, &fir_by_start, fir, &mut handled_selector).await?;
        }
    }

    for output in table.outputs {
        if let Some(value) = shadow.selector.get(&output.mute).copied() {
            client.set_channel_mute(output.mute, value == 1).await?;
            handled_selector.insert(output.mute);
        }
        if let Some(raw) = shadow.scalar.get(&output.gain).copied() {
            client
                .set_channel_gain_persisted(output.gain, raw_scalar_to_f32(profile, raw)?)
                .await?;
            handled_scalar.insert(output.gain);
        }
        if let Some(delay_addr) = output.delay
            && let Some(raw) = shadow.scalar.get(&delay_addr).copied()
        {
            client
                .set_channel_delay_samples_persisted(delay_addr, raw)
                .await?;
            handled_scalar.insert(delay_addr);
        }
        if let Some(polarity_addr) = output.polarity
            && let Some(value) = shadow.selector.get(&polarity_addr).copied()
        {
            client.set_invert(polarity_addr, value != 0).await?;
            handled_selector.insert(polarity_addr);
        }
        for band in output.peq {
            if let Some(entry) = shadow.peq.get(&band.addr) {
                client.write_biquad(band.addr, &entry.coeffs).await?;
                client.bypass_filter(band.addr, entry.bypass).await?;
                handled_peq.insert(band.addr);
            }
        }
        if let Some(crossover) = output.crossover {
            for base in [crossover.lpf_addr, crossover.hpf_addr] {
                for idx in 0..4u16 {
                    let addr = base + idx * 5;
                    if let Some(entry) = shadow.peq.get(&addr) {
                        client.write_biquad(addr, &entry.coeffs).await?;
                        handled_peq.insert(addr);
                    }
                }
                if let Some(entry) = shadow.peq.get(&base) {
                    client.bypass_filter(base, entry.bypass).await?;
                }
            }
        }
        if let Some(fir) = output.fir {
            apply_fir_state(client, shadow, &fir_by_start, fir, &mut handled_selector).await?;
        }
        if let Some(comp) = output.compressor {
            apply_compressor_state(
                client,
                shadow,
                comp,
                &mut handled_selector,
                &mut handled_scalar,
            )
            .await?;
        }
    }

    apply_routing_state(client, shadow, &mut handled_selector, &mut handled_scalar).await?;

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
        return Err(ClientError::Protocol(format!(
            "snapshot restore left unhandled addresses: peq={unhandled_peq:?}, selector={unhandled_selector:?}, scalar={unhandled_scalar:?}"
        )));
    }

    Ok(())
}

async fn apply_compressor_state(
    client: &MiniDspClient,
    shadow: &PresetShadowState,
    comp: Compressor,
    handled_selector: &mut HashSet<u16>,
    handled_scalar: &mut HashSet<u16>,
) -> Result<(), ClientError> {
    if let Some(value) = shadow.selector.get(&comp.bypass).copied() {
        client
            .set_status_bypass(comp.bypass, matches!(value, 1 | 3))
            .await?;
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
            client
                .write_dsp_verified(addr, f32::from_bits(raw), true, label)
                .await?;
            handled_scalar.insert(addr);
        }
    }
    Ok(())
}

async fn apply_routing_state(
    client: &MiniDspClient,
    shadow: &PresetShadowState,
    handled_selector: &mut HashSet<u16>,
    handled_scalar: &mut HashSet<u16>,
) -> Result<(), ClientError> {
    let table = client.profile().dsp_table.ok_or(ClientError::Unsupported)?;
    let routing = table.routing;
    let span = routing.rows as u16 * routing.cols as u16;
    for offset in 0..span {
        let input = (offset / routing.cols as u16) as u8;
        let output = (offset % routing.cols as u16) as u8;
        let status_addr = routing.status_base + offset;
        let gain_addr = routing.gain_base + offset;
        let gain_raw = shadow.scalar.get(&gain_addr).copied().unwrap_or(0);
        let gain_db = raw_scalar_to_f32(client.profile(), gain_raw)?;
        let enabled = match shadow.selector.get(&status_addr).copied() {
            Some(raw)
                if client.profile().dialect.value_format
                    == minidsp_protocol::dialect::ValueFormat::Float32 =>
            {
                raw == 2
            }
            Some(raw) => raw != 0,
            None => false,
        };
        client
            .set_routing_persisted(input, output, enabled, gain_db)
            .await?;
        handled_selector.insert(status_addr);
        handled_scalar.insert(gain_addr);

        if let Some(polarity_base) = routing.polarity_base {
            let polarity_addr = polarity_base + offset;
            if let Some(value) = shadow.selector.get(&polarity_addr).copied() {
                client
                    .set_routing_polarity(input, output, value != 0)
                    .await?;
                handled_selector.insert(polarity_addr);
            }
        }
    }
    Ok(())
}

async fn apply_fir_state(
    client: &MiniDspClient,
    shadow: &PresetShadowState,
    fir_by_start: &HashMap<u16, &SnapshotFirBlock>,
    fir: FirFilter,
    handled_selector: &mut HashSet<u16>,
) -> Result<(), ClientError> {
    let taps = shadow.selector.get(&fir.taps_count).copied().unwrap_or(0);
    let bypass_raw = shadow.selector.get(&fir.bypass).copied().unwrap_or(3);
    let bypass = matches!(bypass_raw, 1 | 3);

    if taps > 0 {
        let block = fir_by_start.get(&fir.data_start).ok_or_else(|| {
            ClientError::Protocol(format!(
                "missing FIR coefficients for data_start {}",
                fir.data_start
            ))
        })?;
        client
            .load_fir_coefficients(fir, &block.coefficients)
            .await?;
    } else {
        client
            .write_dsp_int_verified(fir.taps_count, 0, "set fir tap count")
            .await?;
    }
    client.set_status_bypass(fir.bypass, bypass).await?;
    handled_selector.insert(fir.taps_count);
    handled_selector.insert(fir.bypass);
    Ok(())
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

    let mut shadow = default_preset_shadow(profile, image);
    let fir_blocks = default_fir_blocks(profile, image, &shadow);
    for block in &fir_blocks {
        shadow
            .fir_blocks
            .entry(block.data_start)
            .or_insert_with(|| block.coefficients.clone());
    }
    let mut issues = Vec::new();
    let _ = collect_fir_blocks_from_shadow(profile, &shadow, &mut issues);

    PresetSource {
        shadow: Some(snapshot_shadow_payload(shadow)),
        fir_blocks,
        provenance: vec![SnapshotProvenance::DefaultPresetImage],
        issues,
    }
}

fn build_official_xml_preset_source(
    profile: &DeviceProfile,
    file: &OfficialPresetFile,
) -> PresetSource {
    let mut issues = Vec::new();
    let mut fir_blocks = Vec::new();
    if let Some(table) = profile.dsp_table {
        for input in table.inputs {
            if let Some(fir) = input.fir {
                if let Some(block) =
                    required_fir_block(&file.shadow, &file.fir_blocks, fir, &mut issues)
                {
                    fir_blocks.push(block);
                }
            }
        }
        for output in table.outputs {
            if let Some(fir) = output.fir
                && let Some(block) =
                    required_fir_block(&file.shadow, &file.fir_blocks, fir, &mut issues)
            {
                fir_blocks.push(block);
            }
        }
    }

    PresetSource {
        shadow: Some(snapshot_shadow_payload(file.shadow.clone())),
        fir_blocks,
        provenance: vec![SnapshotProvenance::TokenMatchedOfficialXml],
        issues,
    }
}

fn build_live_preset_source(profile: &DeviceProfile, shadow: &PresetShadowState) -> PresetSource {
    let mut issues = Vec::new();
    if profile.has_unreliable_selector_readback {
        issues.push(
            "active preset fell back to live readback on a profile with non-authoritative runtime selector/scalar fields"
                .to_string(),
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

fn build_shadow_preset_source(profile: &DeviceProfile, shadow: &PresetShadowState) -> PresetSource {
    let mut issues = Vec::new();
    let fir_blocks = collect_fir_blocks_from_shadow(profile, shadow, &mut issues);
    PresetSource {
        shadow: Some(snapshot_shadow_payload(shadow.clone())),
        fir_blocks,
        provenance: vec![SnapshotProvenance::ControllerShadow],
        issues,
    }
}

fn required_fir_block(
    shadow: &PresetShadowState,
    fir_blocks: &HashMap<u16, Vec<f32>>,
    fir: FirFilter,
    issues: &mut Vec<String>,
) -> Option<SnapshotFirBlock> {
    let taps = shadow.selector.get(&fir.taps_count).copied().unwrap_or(0) as usize;
    if taps == 0 {
        return None;
    }
    let Some(coefficients) = fir_blocks.get(&fir.data_start) else {
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
        if let Some(fir) = input.fir
            && let Some(block) = required_fir_block(shadow, &shadow.fir_blocks, fir, issues)
        {
            blocks.push(block);
        }
    }
    for output in table.outputs {
        if let Some(fir) = output.fir
            && let Some(block) = required_fir_block(shadow, &shadow.fir_blocks, fir, issues)
        {
            blocks.push(block);
        }
    }
    blocks
}

fn snapshot_shadow_payload(shadow: PresetShadowState) -> PresetShadowState {
    let mut shadow = shadow;
    shadow.fir_blocks.clear();
    shadow
}

fn raw_scalar_to_f32(profile: &DeviceProfile, raw: u32) -> Result<f32, ClientError> {
    use minidsp_protocol::dialect::ValueFormat;
    match profile.dialect.value_format {
        ValueFormat::Float32 => Ok(f32::from_bits(raw)),
        ValueFormat::FixedPoint => Err(ClientError::Unsupported),
    }
}

fn snapshot_timestamp() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

fn snapshot_transport(transport_url: &str) -> SnapshotTransport {
    let kind = if transport_url.starts_with("tcp://") {
        "tcp"
    } else if transport_url.starts_with("ws://") || transport_url.starts_with("http://") {
        "bridge"
    } else if transport_url.starts_with("usb://") || transport_url.starts_with("usb:") {
        "usb"
    } else {
        "unknown"
    };

    SnapshotTransport {
        kind: kind.to_string(),
        target: transport_url.to_string(),
    }
}

fn hex_token(token: [u8; 4]) -> String {
    format!(
        "{:02x}{:02x}{:02x}{:02x}",
        token[0], token[1], token[2], token[3]
    )
}

fn parse_hex_token(token: &str) -> Result<[u8; 4], ClientError> {
    if token.len() != 8 {
        return Err(ClientError::Protocol(format!(
            "invalid token '{token}': expected 8 hex chars"
        )));
    }
    let mut parsed = [0u8; 4];
    for (idx, chunk) in token.as_bytes().chunks(2).enumerate() {
        let text = std::str::from_utf8(chunk)
            .map_err(|_| ClientError::Protocol(format!("invalid token '{token}'")))?;
        parsed[idx] = u8::from_str_radix(text, 16).map_err(|_| {
            ClientError::Protocol(format!("invalid token '{token}': non-hex byte '{text}'"))
        })?;
    }
    Ok(parsed)
}

fn load_official_preset_files(
    profile: &DeviceProfile,
    product_name: &str,
    serial: u32,
    tokens: &HashMap<u8, [u8; 4]>,
) -> HashMap<u8, OfficialPresetFile> {
    if tokens.is_empty() {
        return HashMap::new();
    }
    let Some(root) = official_app_settings_root() else {
        return HashMap::new();
    };

    let (selector_addrs, scalar_addrs) = official_xml_shadow_addrs(profile);
    let mut xml_files = Vec::new();
    collect_setting_xml_files(&root, &mut xml_files);

    let product_slug = official_product_slug(product_name);
    let serial_text = serial.to_string();
    let serial_suffix = if serial_text.len() > 5 {
        &serial_text[serial_text.len() - 5..]
    } else {
        &serial_text
    };

    let mut by_dir: HashMap<PathBuf, Vec<(u8, OfficialPresetFile)>> = HashMap::new();
    let mut scores: HashMap<PathBuf, usize> = HashMap::new();

    for path in xml_files {
        let Some(slot) = official_setting_slot(&path) else {
            continue;
        };
        let Some(token) = tokens.get(&slot).copied() else {
            continue;
        };
        let Some((timestamp, parsed)) =
            parse_official_setting_xml(&path, &selector_addrs, &scalar_addrs, profile.dsp_version)
        else {
            continue;
        };
        if timestamp.to_be_bytes() != token {
            continue;
        }
        let Some(dir) = path.parent().map(Path::to_path_buf) else {
            continue;
        };
        by_dir.entry(dir.clone()).or_default().push((slot, parsed));

        let score = scores.entry(dir.clone()).or_default();
        *score += 100;
        let path_text = dir.to_string_lossy().to_ascii_lowercase();
        if !product_slug.is_empty() && path_text.contains(&product_slug) {
            *score += 10;
        }
        if path_text.contains(&format!("sn{}", serial_text)) {
            *score += 10;
        } else if path_text.contains(&format!("sn{}", serial_suffix)) {
            *score += 5;
        }
    }

    let Some((_, presets)) = by_dir
        .into_iter()
        .max_by_key(|(dir, presets)| (scores.get(dir).copied().unwrap_or(0), presets.len()))
    else {
        return HashMap::new();
    };

    presets.into_iter().collect()
}

fn official_app_settings_root() -> Option<PathBuf> {
    let mut path = PathBuf::from(std::env::var_os("HOME")?);
    path.push("Documents");
    path.push("miniDSP");
    path.push("MiniDSP Device Console");
    path.exists().then_some(path)
}

fn collect_setting_xml_files(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_setting_xml_files(&path, files);
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.starts_with("setting") && name.ends_with(".xml") {
            files.push(path);
        }
    }
}

fn official_product_slug(product_name: &str) -> String {
    product_name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

fn official_setting_slot(path: &Path) -> Option<u8> {
    let name = path.file_name()?.to_str()?;
    let digits = name
        .strip_prefix("setting")?
        .strip_suffix(".xml")?
        .parse::<u8>()
        .ok()?;
    digits.checked_sub(1)
}

fn official_xml_shadow_addrs(profile: &DeviceProfile) -> (HashSet<u16>, HashSet<u16>) {
    let mut selector = HashSet::new();
    let mut scalar = HashSet::new();
    let Some(table) = profile.dsp_table else {
        return (selector, scalar);
    };

    for input in table.inputs {
        if let Some(mute) = input.mute {
            selector.insert(mute);
        }
        if let Some(gain) = input.gain {
            scalar.insert(gain);
        }
        if let Some(polarity) = input.polarity {
            selector.insert(polarity);
        }
        if let Some(fir) = input.fir {
            selector.insert(fir.bypass);
            selector.insert(fir.taps_count);
        }
    }

    for output in table.outputs {
        selector.insert(output.mute);
        scalar.insert(output.gain);
        if let Some(delay) = output.delay {
            scalar.insert(delay);
        }
        if let Some(polarity) = output.polarity {
            selector.insert(polarity);
        }
        if let Some(fir) = output.fir {
            selector.insert(fir.bypass);
            selector.insert(fir.taps_count);
        }
        if let Some(comp) = output.compressor {
            selector.insert(comp.bypass);
            scalar.insert(comp.threshold);
            scalar.insert(comp.gain);
            scalar.insert(comp.ratio);
            scalar.insert(comp.knee);
            scalar.insert(comp.attack);
            scalar.insert(comp.release);
        }
    }

    let routing_cells = table.routing.rows as u16 * table.routing.cols as u16;
    for offset in 0..routing_cells {
        selector.insert(table.routing.status_base + offset);
        scalar.insert(table.routing.gain_base + offset);
        if let Some(polarity_base) = table.routing.polarity_base {
            selector.insert(polarity_base + offset);
        }
    }

    (selector, scalar)
}

fn parse_official_setting_xml(
    path: &Path,
    selector_addrs: &HashSet<u16>,
    scalar_addrs: &HashSet<u16>,
    expected_dsp_version: u8,
) -> Option<(u32, OfficialPresetFile)> {
    let content = fs::read_to_string(path).ok()?;

    enum Bucket {
        Selector,
        Scalar,
        Filter,
        Fir,
        Ignore,
    }

    let mut timestamp = None;
    let mut dsp_version = None;
    let mut current_addr: Option<u16> = None;
    let mut current_bucket = Bucket::Ignore;
    let mut current_filter_bypass: Option<bool> = None;
    let mut current_filter_coeffs: Option<[f32; 5]> = None;
    let mut current_fir = Vec::new();
    let mut selector = HashMap::new();
    let mut scalar = HashMap::new();
    let mut peq = HashMap::new();
    let mut fir_blocks = HashMap::new();

    for line in content.lines() {
        let line = line.trim();
        if let Some(text) = extract_simple_tag_text(line, "timestamp") {
            timestamp = text.parse::<u32>().ok();
            continue;
        }
        if let Some(text) = extract_simple_tag_text(line, "dspversion") {
            dsp_version = text.parse::<u8>().ok();
            continue;
        }
        if line.starts_with("<item ") {
            let addr = extract_item_addr(line)?;
            current_addr = Some(addr);
            current_bucket = if selector_addrs.contains(&addr) {
                Bucket::Selector
            } else if scalar_addrs.contains(&addr) {
                Bucket::Scalar
            } else {
                Bucket::Ignore
            };
            continue;
        }
        if line.starts_with("<filter ") {
            current_addr = Some(extract_item_addr(line)?);
            current_bucket = Bucket::Filter;
            current_filter_bypass = None;
            current_filter_coeffs = None;
            continue;
        }
        if line.starts_with("<fir ") {
            current_addr = Some(extract_item_addr(line)?);
            current_bucket = Bucket::Fir;
            current_fir.clear();
            continue;
        }

        match current_bucket {
            Bucket::Filter => {
                if let Some(text) = extract_simple_tag_text(line, "bypass") {
                    current_filter_bypass = text.parse::<u8>().ok().map(|v| v != 0);
                    continue;
                }
                if let Some(text) = extract_simple_tag_text(line, "hex") {
                    current_filter_coeffs = parse_filter_hex_coeffs(text);
                    continue;
                }
                if line.starts_with("</filter>") {
                    if let (Some(addr), Some(coeffs)) = (current_addr, current_filter_coeffs) {
                        peq.insert(
                            addr,
                            crate::client::CachedPeqShadow {
                                coeffs,
                                bypass: current_filter_bypass.unwrap_or(false),
                            },
                        );
                    }
                    current_addr = None;
                    current_bucket = Bucket::Ignore;
                    current_filter_bypass = None;
                    current_filter_coeffs = None;
                }
                continue;
            }
            Bucket::Fir => {
                if let Some(text) = extract_tag_text_with_attrs(line, "subpara") {
                    current_fir.extend(parse_fir_coeff_row(text)?);
                    continue;
                }
                if line.starts_with("</fir>") {
                    if let Some(addr) = current_addr {
                        fir_blocks.insert(addr, current_fir.clone());
                    }
                    current_addr = None;
                    current_bucket = Bucket::Ignore;
                    current_fir.clear();
                }
                continue;
            }
            Bucket::Selector | Bucket::Scalar | Bucket::Ignore => {}
        }

        if let Some(hex) = extract_simple_tag_text(line, "hex") {
            let addr = current_addr?;
            let value = u32::from_str_radix(hex, 16).ok()?;
            match current_bucket {
                Bucket::Selector => {
                    selector.insert(addr, value as u16);
                }
                Bucket::Scalar => {
                    scalar.insert(addr, value);
                }
                Bucket::Filter | Bucket::Fir | Bucket::Ignore => {}
            }
            continue;
        }
        if line.starts_with("</item>") {
            current_addr = None;
            current_bucket = Bucket::Ignore;
        }
    }

    if dsp_version? != expected_dsp_version {
        return None;
    }

    Some((
        timestamp?,
        OfficialPresetFile {
            shadow: PresetShadowState {
                peq,
                selector,
                scalar,
                fir_blocks: HashMap::new(),
            },
            fir_blocks,
        },
    ))
}

fn extract_simple_tag_text<'a>(line: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = line.find(&open)? + open.len();
    let end = line[start..].find(&close)? + start;
    Some(&line[start..end])
}

fn extract_tag_text_with_attrs<'a>(line: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}");
    let start = line.find(&open)?;
    let content_start = line[start..].find('>')? + start + 1;
    let close = format!("</{tag}>");
    let content_end = line[content_start..].find(&close)? + content_start;
    Some(&line[content_start..content_end])
}

fn extract_item_addr(line: &str) -> Option<u16> {
    let start = line.find("addr=\"")? + 6;
    let end = line[start..].find('"')? + start;
    line[start..end].parse::<u16>().ok()
}

fn parse_filter_hex_coeffs(text: &str) -> Option<[f32; 5]> {
    let mut coeffs = [0f32; 5];
    let mut iter = text.split(',');
    for slot in &mut coeffs {
        let word = iter.next()?.trim();
        let bits = u32::from_str_radix(word, 16).ok()?;
        *slot = f32::from_bits(bits);
    }
    Some(coeffs)
}

fn parse_fir_coeff_row(text: &str) -> Option<Vec<f32>> {
    let mut row = Vec::new();
    for word in text.split(',') {
        let bits = u32::from_str_radix(word.trim(), 16).ok()?;
        row.push(f32::from_bits(bits));
    }
    Some(row)
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

fn default_bypass_map(image: &DefaultPresetImage) -> HashMap<u16, bool> {
    let mut map = HashMap::new();
    for chunk in image.bypass.chunks_exact(3) {
        let addr = u16::from_be_bytes([chunk[1], chunk[2]]);
        map.insert(addr, chunk[0] & 0x80 != 0);
    }
    map
}

fn default_preset_shadow(profile: &DeviceProfile, image: &DefaultPresetImage) -> PresetShadowState {
    let mut snapshot = PresetShadowState::default();
    let bypass = default_bypass_map(image);
    let Some(table) = profile.dsp_table else {
        return snapshot;
    };

    for input in table.inputs {
        if let Some(mute) = input.mute
            && let Some(value) = default_param_word(image, mute)
        {
            snapshot.selector.insert(mute, value as u16);
        }
        if let Some(gain) = input.gain
            && let Some(value) = default_param_word(image, gain)
        {
            snapshot.scalar.insert(gain, value);
        }
        if let Some(polarity) = input.polarity
            && let Some(value) = default_param_word(image, polarity)
        {
            snapshot.selector.insert(polarity, value as u16);
        }
        for band in input.peq {
            if let Some(coeffs) = default_param_coeffs(image, band.addr) {
                snapshot.peq.insert(
                    band.addr,
                    crate::client::CachedPeqShadow {
                        coeffs,
                        bypass: bypass.get(&band.addr).copied().unwrap_or(false),
                    },
                );
            }
        }
        if let Some(fir) = input.fir {
            if let Some(value) = default_param_word(image, fir.bypass) {
                snapshot.selector.insert(fir.bypass, value as u16);
            }
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
        if let Some(delay) = output.delay
            && let Some(value) = default_param_word(image, delay)
        {
            snapshot.scalar.insert(delay, value);
        }
        if let Some(polarity) = output.polarity
            && let Some(value) = default_param_word(image, polarity)
        {
            snapshot.selector.insert(polarity, value as u16);
        }
        for band in output.peq {
            if let Some(coeffs) = default_param_coeffs(image, band.addr) {
                snapshot.peq.insert(
                    band.addr,
                    crate::client::CachedPeqShadow {
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
        }
        if let Some(crossover) = output.crossover {
            for base in [crossover.lpf_addr, crossover.hpf_addr] {
                for idx in 0..4u16 {
                    let addr = base + idx * 5;
                    if let Some(coeffs) = default_param_coeffs(image, addr) {
                        snapshot.peq.insert(
                            addr,
                            crate::client::CachedPeqShadow {
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

fn default_fir_blocks(
    profile: &DeviceProfile,
    image: &DefaultPresetImage,
    shadow: &PresetShadowState,
) -> Vec<SnapshotFirBlock> {
    let mut blocks = Vec::new();
    let Some(table) = profile.dsp_table else {
        return blocks;
    };

    for input in table.inputs {
        if let Some(fir) = input.fir {
            if let Some(block) = default_fir_block(image, shadow, fir) {
                blocks.push(block);
            }
        }
    }
    for output in table.outputs {
        if let Some(fir) = output.fir {
            if let Some(block) = default_fir_block(image, shadow, fir) {
                blocks.push(block);
            }
        }
    }
    blocks
}

fn default_fir_block(
    image: &DefaultPresetImage,
    shadow: &PresetShadowState,
    fir: FirFilter,
) -> Option<SnapshotFirBlock> {
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
    Some(SnapshotFirBlock {
        data_start: fir.data_start,
        coefficients,
    })
}

async fn capture_live_preset_source(
    client: &MiniDspClient,
) -> Result<(PresetShadowState, PresetSource), ClientError> {
    let config = client.read_device_config().await?;
    let mut shadow = shadow_from_device_config(client.profile(), &config);
    populate_live_fir_blocks(client, &mut shadow).await?;
    let source = build_live_preset_source(client.profile(), &shadow);
    Ok((shadow, source))
}

async fn populate_live_fir_blocks(
    client: &MiniDspClient,
    shadow: &mut PresetShadowState,
) -> Result<(), ClientError> {
    let Some(table) = client.profile().dsp_table else {
        return Ok(());
    };

    for input in table.inputs {
        if let Some(fir) = input.fir
            && let Some(block) = read_live_fir_block(client, shadow, fir).await?
        {
            shadow
                .fir_blocks
                .insert(block.data_start, block.coefficients);
        }
    }
    for output in table.outputs {
        if let Some(fir) = output.fir
            && let Some(block) = read_live_fir_block(client, shadow, fir).await?
        {
            shadow
                .fir_blocks
                .insert(block.data_start, block.coefficients);
        }
    }
    Ok(())
}

async fn read_live_fir_block(
    client: &MiniDspClient,
    shadow: &PresetShadowState,
    fir: FirFilter,
) -> Result<Option<SnapshotFirBlock>, ClientError> {
    let taps = shadow.selector.get(&fir.taps_count).copied().unwrap_or(0) as usize;
    if taps == 0 {
        return Ok(None);
    }

    let mut coefficients = Vec::with_capacity(taps);
    let mut offset = 0usize;
    while offset < taps {
        let chunk = (taps - offset).min(14);
        let values = client
            .read_floats(fir.data_start + offset as u16, chunk as u8)
            .await?;
        if values.len() != chunk {
            return Err(ClientError::Protocol(format!(
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

fn shadow_from_device_config(
    profile: &DeviceProfile,
    config: &minidsp_protocol::status::DeviceConfig,
) -> PresetShadowState {
    let mut shadow = PresetShadowState::default();
    let Some(table) = profile.dsp_table else {
        return shadow;
    };

    for input_cfg in &config.inputs {
        let Some(input) = table.inputs.get(input_cfg.index as usize) else {
            continue;
        };
        if let Some(addr) = input.gain
            && let Some(gain) = input_cfg.gain
        {
            shadow.scalar.insert(addr, gain.to_bits());
        }
        if let Some(addr) = input.mute
            && let Some(mute) = input_cfg.mute
        {
            shadow.selector.insert(addr, if mute { 1 } else { 2 });
        }
        if let Some(addr) = input.polarity {
            shadow.selector.insert(addr, 0);
        }
        for band in &input_cfg.peq {
            if let Some(def) = input.peq.get(band.index as usize) {
                shadow.peq.insert(
                    def.addr,
                    crate::client::CachedPeqShadow {
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
                    crate::client::CachedPeqShadow {
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
                        crate::client::CachedPeqShadow {
                            coeffs: filter.coeff.to_array(),
                            bypass: group.bypass,
                        },
                    );
                }
            }
        }
        if let Some(fir) = output.fir
            && let Some(state) = &output_cfg.fir
        {
            shadow
                .selector
                .insert(fir.bypass, if state.bypass { 3 } else { 2 });
            shadow.selector.insert(fir.taps_count, state.active_taps);
        }
        if let Some(comp) = output.compressor
            && let Some(state) = &output_cfg.compressor
        {
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
            if let Some(polarity_base) = table.routing.polarity_base
                && let Some(inverted) = route.polarity_inverted
            {
                shadow
                    .selector
                    .insert(polarity_base + flat_idx, if inverted { 1 } else { 0 });
            }
        }
    }

    shadow
}

fn approx_equal(left: f32, right: f32, tolerance: f32) -> bool {
    (left - right).abs() <= tolerance
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidsp_protocol::device::probe;

    #[test]
    fn parse_hex_token_round_trips() {
        let token = [0xab, 0xfa, 0x0c, 0xc3];
        let text = hex_token(token);
        assert_eq!(text, "abfa0cc3");
        assert_eq!(parse_hex_token(&text).unwrap(), token);
    }

    #[test]
    fn parse_official_setting_xml_extracts_shadow_and_fir() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../app/minidsp-snapshots/20260408-130500-preset1-after-reset-live/official-setting1.xml");
        let profile = probe::identify(10, 100);
        let (selector_addrs, scalar_addrs) = official_xml_shadow_addrs(&profile);
        let (timestamp, parsed) =
            parse_official_setting_xml(&path, &selector_addrs, &scalar_addrs, 100)
                .expect("parse xml");

        assert_eq!(timestamp, 0xfacc_93dd);
        assert_eq!(parsed.shadow.selector.get(&6), Some(&2));
        assert_eq!(parsed.shadow.scalar.get(&40), Some(&(-30.0f32).to_bits()));
        assert_eq!(
            parsed.fir_blocks.get(&85).map(|coeffs| coeffs[0]),
            Some(1.0),
            "FIR_3_0 first tap should decode"
        );
        assert!(
            parsed.shadow.peq.contains_key(&8280),
            "PEQ_1_1 should be present"
        );
    }

    #[test]
    fn default_source_for_dsp100_is_complete() {
        let profile = probe::identify(10, 100);
        let source = build_default_preset_source(&profile, default_preset_image_for_dsp(100));
        assert!(source.shadow.is_some());
        assert!(source.issues.is_empty());
    }
}
