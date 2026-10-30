//! High-level MiniDSP client — wraps transport + command queue + device profile.

use crate::queue::{CommandQueue, Priority};
use crate::transport::{Transport, TransportError};
use getrandom::getrandom;
use minidsp_protocol::commands::{self, HardwareId, Response};
use minidsp_protocol::default_preset::{DefaultPresetImage, default_preset_image_for_dsp};
use minidsp_protocol::device::DeviceProfile;
use minidsp_protocol::device::dsp::{BassManagement, FirFilter};
use minidsp_protocol::device::probe;
use minidsp_protocol::dialect::ValueFormat;
use minidsp_protocol::eeprom;
use minidsp_protocol::shadow::{PeqShadow, build_peq_shadow, sanitize_shadow_preferred_scalar};
use minidsp_protocol::status::{
    BiquadCoeffs, BmChannelConfig, BmChannelPatch, BmMixerCellConfig, BmMixerCellPatch,
    BmStateConfig, CompressorConfig, ConfigReadback, ConfigReadbackSource, CrossoverGroupConfig,
    DeviceConfig, DeviceInfo, FirConfig, IndexedBiquadConfig, InputChannelConfig, MasterStatusView,
    OutputChannelConfig, PeqBandConfig, RoutingConfig, StatusSummary,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

#[path = "client/shadow.rs"]
mod shadow;
use shadow::*;

/// Error type for client operations.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("transport error: {0}")]
    Transport(#[from] TransportError),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("device busy")]
    Busy,
    #[error("timeout")]
    Timeout,
    #[error("unsupported operation for this device")]
    Unsupported,
}

/// Saved PEQ state for measurement mode restore.
#[derive(Debug, Clone)]
pub struct SavedPeqState {
    /// (addr, coefficients, bypass) tuples for all PEQ bands that were saved.
    pub bands: Vec<(u16, [f32; 5], bool)>,
}

#[derive(Debug, Clone)]
struct PendingWrite {
    bands: Vec<(u16, [f32; 5], bool)>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CachedPeqShadow {
    pub coeffs: [f32; 5],
    pub bypass: bool,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PresetShadowState {
    pub peq: HashMap<u16, CachedPeqShadow>,
    pub selector: HashMap<u16, u16>,
    pub scalar: HashMap<u16, u32>,
    #[serde(default)]
    pub fir_blocks: HashMap<u16, Vec<f32>>,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DeviceShadowState {
    #[serde(default)]
    pub presets: HashMap<u8, PresetShadowState>,
    #[serde(default)]
    pub peq: HashMap<u16, CachedPeqShadow>,
    #[serde(default)]
    pub selector: HashMap<u16, u16>,
    #[serde(default)]
    pub scalar: HashMap<u16, u32>,
    #[serde(default)]
    pub preset_tokens: HashMap<u8, [u8; 4]>,
    #[serde(default)]
    pub trusted_snapshot_presets: HashSet<u8>,
}

const MASTER_STATUS_VERIFY_TIMEOUT: Duration = Duration::from_secs(10);
// A louder write through a bridge that is already at its volume ceiling leaves
// the volume unchanged; accept that after this long instead of the full timeout.
const BRIDGED_CEILING_SETTLE: Duration = Duration::from_secs(2);
const MASTER_STATUS_POLL_INTERVAL: Duration = Duration::from_millis(100);
// On the live 2x4 HD, master-status can report the new preset slightly before
// follow-on PEQ writes reliably land on that preset. A short settle window keeps
// `minidsp -f` batch writes from racing the hardware bank switch.
const POST_SET_CONFIG_SETTLE_DELAY: Duration = Duration::from_secs(1);
const MASTER_STATUS_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
// Raw TCP/WS bridge implementations may verify the preset switch before they
// reply, so `SetConfig` cannot assume an immediate transport ack.
const SET_CONFIG_RESPONSE_TIMEOUT: Duration = Duration::from_secs(12);
const RESET_RESPONSE_TIMEOUT: Duration = Duration::from_secs(2);
const RESET_VERIFY_TIMEOUT: Duration = Duration::from_secs(12);
const RESET_STATUS_READ_TIMEOUT: Duration = Duration::from_millis(250);
const RESET_MIN_OUTAGE: Duration = Duration::from_millis(500);
const FIR_LOAD_END_TIMEOUT: Duration = Duration::from_secs(60);
const GAIN_VERIFY_TOLERANCE_DB: f32 = 0.2;
const SILENT_METER_DB: f32 = -140.0;
const DSP_READ_TIMEOUT: Duration = Duration::from_millis(750);
const METER_READ_TIMEOUT: Duration = Duration::from_millis(750);
const FLASH_BLOCK_PRESET_VALUES: u8 = 0x01;
const FLASH_BLOCK_PRESET_BYPASS: u8 = 0x02;
const RECOVERY_MODIFICATION_TOKEN: [u8; 4] = [0xFE, 0xEF, 0x8F, 0xCF];

fn generate_modification_token() -> Result<[u8; 4], ClientError> {
    let mut token = [0u8; 4];
    getrandom(&mut token).map_err(|err| {
        ClientError::Protocol(format!("failed to generate modification token: {err}"))
    })?;
    Ok(token)
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
    for chunk in image.bypass.as_chunks::<3>().0.iter() {
        let addr = u16::from_be_bytes([chunk[1], chunk[2]]);
        map.insert(addr, chunk[0] & 0x80 != 0);
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
                    CachedPeqShadow {
                        coeffs,
                        bypass: bypass.get(&band.addr).copied().unwrap_or(false),
                    },
                );
            }
        }
        if let Some(fir) = input.fir
            && let Some(value) = default_param_word(image, fir.taps_count)
        {
            snapshot.selector.insert(fir.taps_count, value as u16);
            if let Some(bypass) = default_param_word(image, fir.bypass) {
                snapshot.selector.insert(fir.bypass, bypass as u16);
            }
            if let Some(block) = default_fir_block(image, &snapshot, fir) {
                snapshot
                    .fir_blocks
                    .insert(block.data_start, block.coefficients);
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
    fir: FirFilter,
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

/// Connected MiniDSP device client.
pub struct MiniDspClient {
    volume_settings:
        std::sync::RwLock<Arc<tokio::sync::RwLock<minidsp_protocol::settings::Settings>>>,
    queue: Arc<CommandQueue>,
    transport_url: String,
    profile: DeviceProfile,
    hw_id: HardwareId,
    dsp_version: u8,
    serial: u32,
    /// Saved PEQ state from measurement mode entry, for later restore.
    saved_peq: Mutex<Option<SavedPeqState>>,
    /// PEQ batch write that should be replayed after a reconnect.
    pending_write: Arc<Mutex<Option<PendingWrite>>>,
    /// Shadow state for PEQ bands when the device does not expose live coeff readback.
    peq_shadow: Arc<Mutex<HashMap<u16, PeqShadow>>>,
    peq_readback_known: Mutex<HashMap<u16, (bool, bool)>>,
    scalar_readback_known: Mutex<HashSet<u16>>,
    selector_readback_known: Mutex<HashSet<u16>>,
    pub(crate) invalidated_snapshot_presets: Mutex<HashSet<u8>>,
    readback_epoch: Mutex<u64>,
    /// Shadow state for selector-style integer slots and other SHARC integer
    /// controls whose live DSP100 readback can collapse or stay at `0`.
    selector_shadow: Arc<Mutex<HashMap<u16, u16>>>,
    /// Shadow state for scalar float32 config writes on paths where live
    /// DSP100 readback is not authoritative.
    scalar_shadow: Arc<Mutex<HashMap<u16, u32>>>,
    /// Shadowed FIR coefficient blocks keyed by the block start address.
    fir_shadow: Arc<Mutex<HashMap<u16, Vec<f32>>>>,
    /// Shadow state snapshots keyed by preset slot so non-authoritative DSP100
    /// values do not leak across preset switches.
    preset_shadows: Arc<Mutex<HashMap<u8, PresetShadowState>>>,
    /// Last observed raw modification-token bytes per preset slot.
    preset_tokens: Arc<Mutex<HashMap<u8, [u8; 4]>>>,
    /// Preset snapshots known to be complete and trustworthy for strict export.
    trusted_snapshot_presets: Arc<Mutex<HashSet<u8>>>,
    /// Presets whose freshly stored trusted snapshot should survive the next
    /// preset switch without being re-captured from the active shadow.
    skip_trusted_shadow_capture_once: Arc<Mutex<HashSet<u8>>>,
    /// Last observed active preset slot.
    active_preset: Mutex<Option<u8>>,
    /// Serializes state transitions so overlapping preset/reset calls cannot
    /// race against stale status while the device is still applying one.
    pub(crate) state_transition_lock: Mutex<()>,
}

impl MiniDspClient {
    fn ensure_write_response(resp: Response, context: &str) -> Result<(), ClientError> {
        match resp {
            Response::Ack | Response::PresetComplete | Response::ResetComplete => Ok(()),
            Response::Error(data) => Err(ClientError::Protocol(format!(
                "{context}: device error {data:02x?}"
            ))),
            other => Err(ClientError::Protocol(format!(
                "{context}: unexpected response {other:?}"
            ))),
        }
    }

    async fn execute_write(
        &self,
        payload: Vec<u8>,
        priority: Priority,
        context: &str,
    ) -> Result<(), ClientError> {
        let mutates_preset = matches!(
            payload.first(),
            Some(&commands::CMD_WRITE_DSP)
                | Some(&commands::CMD_WRITE_BIQUAD)
                | Some(&commands::CMD_BYPASS_FILTER)
                | Some(&commands::CMD_FIR_LOAD_DATA)
        );
        self.invalidate_config_readback_for_write(&payload).await;
        let result = match self.queue.execute(payload.clone(), priority).await {
            Ok(resp) => Self::ensure_write_response(resp, context),
            Err(err) => Err(err.into()),
        };
        if result.is_err() {
            self.revoke_trust_after_failed_write(&payload).await;
        }
        result?;
        if mutates_preset {
            self.note_preset_mutation().await;
        }
        Ok(())
    }

    async fn execute_write_with_timeout(
        &self,
        payload: Vec<u8>,
        priority: Priority,
        timeout: Duration,
        context: &str,
    ) -> Result<(), ClientError> {
        self.invalidate_config_readback_for_write(&payload).await;
        let result = match self
            .queue
            .execute_with_timeout(payload.clone(), priority, timeout)
            .await
        {
            Ok(resp) => Self::ensure_write_response(resp, context),
            Err(err) => Err(err.into()),
        };
        if result.is_err() {
            self.revoke_trust_after_failed_write(&payload).await;
        }
        result
    }

    pub async fn close(&self) -> Result<(), ClientError> {
        self.queue.close().await?;
        Ok(())
    }

    /// Connect to a device via transport, probe its identity, and return a client.
    pub async fn connect(transport: Box<dyn Transport>) -> Result<Self, ClientError> {
        let transport_url = transport.url();
        let queue = Arc::new(CommandQueue::new(transport));

        // Step 1: Read hardware ID
        let hw_resp = queue
            .execute(commands::read_hardware_id(), Priority::Critical)
            .await?;
        let hw_id = match hw_resp {
            Response::HardwareId(id) => id,
            _ => {
                return Err(ClientError::Protocol(
                    "unexpected response to ReadHardwareId".into(),
                ));
            }
        };

        // Step 2: Read DSP version from EEPROM
        let flash_resp = queue
            .execute(
                commands::read_flash(eeprom::DSP_VERSION_ADDR.0, eeprom::DSP_VERSION_ADDR.1, 1),
                Priority::Critical,
            )
            .await?;
        let dsp_version = match flash_resp {
            Response::FlashData(data) => eeprom::parse_dsp_version(&data).unwrap_or(0),
            _ => 0,
        };

        // Step 3: Read serial number
        let serial_resp = queue
            .execute(
                commands::read_flash(eeprom::SERIAL_ADDR.0, eeprom::SERIAL_ADDR.1, 2),
                Priority::Critical,
            )
            .await?;
        let serial = match serial_resp {
            Response::FlashData(data) => eeprom::parse_serial(&data).unwrap_or(0),
            _ => 0,
        };

        // Step 4: Identify device profile
        let profile = probe::identify(hw_id.hw_id, dsp_version);

        // Step 5: Set dialect on queue so responses are parsed correctly
        queue.set_dialect(profile.dialect).await;

        let peq_shadow = Arc::new(Mutex::new(build_peq_shadow(&profile)));
        let pending_write = Arc::new(Mutex::new(None));
        let selector_shadow = Arc::new(Mutex::new(HashMap::new()));
        let scalar_shadow = Arc::new(Mutex::new(HashMap::new()));
        let fir_shadow = Arc::new(Mutex::new(HashMap::new()));
        let preset_shadows = Arc::new(Mutex::new(HashMap::new()));
        let preset_tokens = Arc::new(Mutex::new(HashMap::new()));
        let trusted_snapshot_presets = Arc::new(Mutex::new(HashSet::new()));
        let skip_trusted_shadow_capture_once = Arc::new(Mutex::new(HashSet::new()));

        let client = Self {
            queue,
            transport_url,
            profile,
            hw_id,
            dsp_version,
            serial,
            saved_peq: Mutex::new(None),
            volume_settings: Default::default(),
            pending_write,
            peq_shadow,
            peq_readback_known: Mutex::new(HashMap::new()),
            scalar_readback_known: Mutex::new(HashSet::new()),
            selector_readback_known: Mutex::new(HashSet::new()),
            invalidated_snapshot_presets: Mutex::new(HashSet::new()),
            readback_epoch: Mutex::new(0),
            selector_shadow,
            scalar_shadow,
            fir_shadow,
            preset_shadows,
            preset_tokens,
            trusted_snapshot_presets,
            skip_trusted_shadow_capture_once,
            active_preset: Mutex::new(None),
            state_transition_lock: Mutex::new(()),
        };

        // Seed the active preset cache early so shadow snapshots can stay
        // preset-scoped even before the daemon starts polling status.
        let _ = client.get_master_status().await;

        Ok(client)
    }

    pub fn profile(&self) -> &DeviceProfile {
        &self.profile
    }

    pub fn hw_id(&self) -> &HardwareId {
        &self.hw_id
    }

    pub fn serial(&self) -> u32 {
        self.serial
    }

    pub fn dsp_version(&self) -> u8 {
        self.dsp_version
    }

    /// Get device info for the /devices list endpoint.
    pub fn device_info(&self) -> DeviceInfo {
        minidsp_protocol::status::device_info(
            self.transport_url.clone(),
            &self.hw_id,
            self.dsp_version,
            self.serial,
        )
    }

    pub fn transport_url(&self) -> &str {
        &self.transport_url
    }

    /// Increments each time the queue reconnects its transport.
    pub fn reconnect_epoch(&self) -> u64 {
        self.queue.reconnect_epoch()
    }

    async fn get_master_status_with_priority(
        &self,
        priority: Priority,
    ) -> Result<eeprom::MasterStatusRaw, ClientError> {
        self.get_master_status_with_timeout_and_priority(priority, MASTER_STATUS_COMMAND_TIMEOUT)
            .await
    }

    async fn get_master_status_with_timeout_and_priority(
        &self,
        priority: Priority,
        timeout: Duration,
    ) -> Result<eeprom::MasterStatusRaw, ClientError> {
        let resp = self
            .queue
            .execute_with_timeout(
                commands::read_flash(
                    eeprom::MASTER_STATUS_ADDR.0,
                    eeprom::MASTER_STATUS_ADDR.1,
                    4,
                ),
                priority,
                timeout,
            )
            .await?;
        match resp {
            Response::FlashData(data) => {
                let master = eeprom::MasterStatusRaw::from_bytes(&data)
                    .ok_or_else(|| ClientError::Protocol("failed to parse master status".into()))?;
                self.update_active_preset(master.preset).await;
                Ok(master)
            }
            _ => Err(ClientError::Protocol("unexpected response".into())),
        }
    }

    /// Read master status from EEPROM.
    pub async fn get_master_status(&self) -> Result<eeprom::MasterStatusRaw, ClientError> {
        self.get_master_status_with_priority(Priority::Normal).await
    }

    async fn get_master_status_with_timeout(
        &self,
        timeout: Duration,
    ) -> Result<eeprom::MasterStatusRaw, ClientError> {
        self.get_master_status_with_timeout_and_priority(Priority::Normal, timeout)
            .await
    }

    async fn wait_for_master_status_until<F>(
        &self,
        timeout: Duration,
        mut matches: F,
    ) -> Result<eeprom::MasterStatusRaw, ClientError>
    where
        F: FnMut(&eeprom::MasterStatusRaw) -> bool,
    {
        let deadline = Instant::now() + timeout;
        let mut last_status = None;

        loop {
            match self.get_master_status().await {
                Ok(master) => {
                    let matched = matches(&master);
                    last_status = Some(master);
                    if matched {
                        return Ok(last_status.expect("just set"));
                    }
                }
                Err(err @ ClientError::Transport(TransportError::Disconnected)) => {
                    return Err(err);
                }
                Err(_) => {}
            }

            if Instant::now() >= deadline {
                return if let Some(status) = last_status {
                    Ok(status)
                } else {
                    Err(ClientError::Timeout)
                };
            }

            tokio::time::sleep(MASTER_STATUS_POLL_INTERVAL).await;
        }
    }

    async fn wait_for_reset_recovery_until(
        &self,
        timeout: Duration,
        initial_disconnect_epoch: u64,
    ) -> Result<eeprom::MasterStatusRaw, ClientError> {
        let deadline = Instant::now() + timeout;
        let mut outage_started: Option<Instant> = None;
        let mut transient_failures = 0usize;
        let mut saw_disconnect = false;
        let mut last_err = None;

        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(last_err.unwrap_or_else(|| {
                    ClientError::Protocol(
                        "reset verification never observed device recovery".into(),
                    )
                }));
            }

            match self
                .get_master_status_with_timeout(remaining.min(RESET_STATUS_READ_TIMEOUT))
                .await
            {
                Ok(master) => {
                    if self.queue.disconnect_epoch() != initial_disconnect_epoch {
                        return Ok(master);
                    }

                    if let Some(started) = outage_started {
                        let outage_duration = started.elapsed();
                        if saw_disconnect
                            || transient_failures >= 2
                            || outage_duration >= RESET_MIN_OUTAGE
                        {
                            return Ok(master);
                        }

                        outage_started = None;
                        transient_failures = 0;
                        saw_disconnect = false;
                        last_err = None;
                    }
                }
                Err(err @ ClientError::Transport(TransportError::Disconnected)) => {
                    saw_disconnect = true;
                    transient_failures += 1;
                    outage_started.get_or_insert_with(Instant::now);
                    last_err = Some(err);
                }
                Err(
                    err @ (ClientError::Transport(TransportError::Timeout)
                    | ClientError::Transport(TransportError::Hid(_))
                    | ClientError::Transport(TransportError::Io(_))
                    | ClientError::Protocol(_)
                    | ClientError::Timeout),
                ) => {
                    transient_failures += 1;
                    outage_started.get_or_insert_with(Instant::now);
                    last_err = Some(err);
                }
                Err(err) => return Err(err),
            }

            tokio::time::sleep(MASTER_STATUS_POLL_INTERVAL).await;
        }
    }

    async fn get_status_summary_with_priority(
        &self,
        priority: Priority,
    ) -> Result<StatusSummary, ClientError> {
        let master = self.get_master_status_with_priority(priority).await?;
        let source_name = self
            .profile
            .sources
            .source_for_index(master.source)
            .map(|s| s.pascal_case().to_string())
            .unwrap_or_else(|| format!("Unknown({})", master.source));

        // Read level meters if available
        let (input_levels, output_levels) = if let Some(table) = self.profile.dsp_table {
            let input_levels = self
                .read_status_meters_with_priority(table.meters.input, priority)
                .await;
            let output_levels = self
                .read_status_meters_with_priority(table.meters.output, priority)
                .await;
            (input_levels, output_levels)
        } else {
            (vec![], vec![])
        };

        let dirac = if self.profile.has_dirac {
            // Read Dirac bypass state from EEPROM 0xFFE0
            let dirac_resp = self
                .queue
                .execute(commands::read_flash(0xFF, 0xE0, 1), priority)
                .await;
            match dirac_resp {
                Ok(Response::FlashData(data)) if !data.is_empty() => {
                    Some(data[0] == 0) // 0 = Dirac active (not bypassed), 1 = bypassed
                }
                _ => Some(false),
            }
        } else {
            None
        };

        Ok(StatusSummary {
            master: MasterStatusView {
                preset: master.preset,
                source: source_name,
                volume: master.volume_db(),
                mute: master.is_muted(),
                dirac,
            },
            input_levels,
            output_levels,
        })
    }

    /// Get full status summary (compatible with minidsp-rs StatusSummary).
    pub async fn get_status_summary(&self) -> Result<StatusSummary, ClientError> {
        self.get_status_summary_with_priority(Priority::Normal)
            .await
    }

    /// Get a status summary using the specified queue priority.
    pub async fn get_status_summary_polled(&self) -> Result<StatusSummary, ClientError> {
        self.get_status_summary_with_priority(Priority::Low).await
    }

    /// Bind a daemon's live policy; direct clients default to no ceiling.
    pub fn bind_volume_settings(
        &self,
        settings: Arc<tokio::sync::RwLock<minidsp_protocol::settings::Settings>>,
    ) {
        *self
            .volume_settings
            .write()
            .unwrap_or_else(|e| e.into_inner()) = settings;
    }

    fn volume_settings(&self) -> Arc<tokio::sync::RwLock<minidsp_protocol::settings::Settings>> {
        self.volume_settings
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Called while the owner holds the settings write lock. Only lowers volume.
    pub async fn enforce_volume_settings(
        &self,
        settings: minidsp_protocol::settings::Settings,
    ) -> Result<(), ClientError> {
        if settings.max_volume_db().is_some() {
            let current = self.get_master_status().await?.volume;
            let bounded = settings.clamp_raw(current);
            if bounded != current {
                self.set_volume_unchecked(bounded).await?;
            }
        }
        Ok(())
    }

    /// Set volume (raw byte value, gain = -0.5 * value).
    pub async fn set_volume(&self, value: u8) -> Result<(), ClientError> {
        let settings = self.volume_settings();
        let settings = settings.read().await;
        self.set_volume_unchecked(settings.clamp_raw(value)).await
    }

    async fn set_volume_unchecked(&self, value: u8) -> Result<(), ClientError> {
        let before = self.get_master_status().await?;
        if before.volume == value {
            return Ok(());
        }

        self.execute_write(commands::set_volume(value), Priority::High, "set volume")
            .await?;
        // A minidsp-open daemon or board between this client and the device
        // may clamp the write to its maximum volume, which this client can't
        // see. Larger raw values are quieter. Direct USB stays strict.
        let bridged = !self.transport_url.starts_with("usb:");
        let louder = value < before.volume;
        let timeout = if bridged && louder {
            BRIDGED_CEILING_SETTLE
        } else {
            MASTER_STATUS_VERIFY_TIMEOUT
        };
        let after = self
            .wait_for_master_status_until(timeout, |master| {
                master.volume == value || (bridged && master.volume != before.volume)
            })
            .await?;

        if after.volume == value {
            Ok(())
        } else if bridged && after.volume > value {
            tracing::warn!(
                "volume is {:.1} dB, quieter than the requested {:.1} dB; the controller's maximum volume may have limited it",
                -0.5 * f64::from(after.volume),
                -0.5 * f64::from(value)
            );
            Ok(())
        } else {
            Err(ClientError::Protocol(format!(
                "set volume did not persist (before={}, expected={}, after={})",
                before.volume, value, after.volume
            )))
        }
    }

    /// Set volume in dB.
    pub async fn set_volume_db(&self, db: f32) -> Result<(), ClientError> {
        let value = (-2.0 * db).clamp(0.0, 255.0) as u8;
        self.set_volume(value).await
    }

    /// Set mute state.
    pub async fn set_mute(&self, muted: bool) -> Result<(), ClientError> {
        let before = self.get_master_status().await?;
        if before.is_muted() == muted {
            return Ok(());
        }

        self.execute_write(commands::set_mute(muted), Priority::High, "set mute")
            .await?;
        let after = self
            .wait_for_master_status_until(MASTER_STATUS_VERIFY_TIMEOUT, |master| {
                master.is_muted() == muted
            })
            .await?;

        if after.is_muted() == muted {
            Ok(())
        } else {
            Err(ClientError::Protocol(format!(
                "set mute did not persist (before={}, expected={}, after={})",
                before.is_muted(),
                muted,
                after.is_muted()
            )))
        }
    }

    /// Switch preset (0-3) using the device's normal interactive mode.
    pub async fn set_config(&self, preset: u8) -> Result<eeprom::MasterStatusRaw, ClientError> {
        self.set_config_with_mode(preset, commands::PRESET_CHANGE_NORMAL)
            .await
    }

    pub(crate) async fn set_config_with_mode_locked(
        &self,
        preset: u8,
        reset_mode: u8,
    ) -> Result<eeprom::MasterStatusRaw, ClientError> {
        if preset >= self.profile.presets {
            return Err(ClientError::Protocol(format!(
                "preset {} out of range (max {})",
                preset,
                self.profile.presets - 1
            )));
        }
        let before = self.get_master_status().await?;
        if before.preset == preset && reset_mode != commands::PRESET_CHANGE_RELOAD_SAME_SLOT {
            return Ok(before);
        }
        self.invalidate_readback_for_command(&commands::set_config(preset, reset_mode))
            .await;
        let _levels = self.queue.pause_levels();

        async {
            match self
                .execute_write_with_timeout(
                    commands::set_config(preset, reset_mode),
                    Priority::Critical,
                    SET_CONFIG_RESPONSE_TIMEOUT,
                    "set config",
                )
                .await
            {
                Ok(()) => {}
                Err(ClientError::Transport(TransportError::Timeout)) => {
                    tracing::debug!(
                        "set config response timed out for preset {} (mode {:#04x}); verifying via master status",
                        preset,
                        reset_mode
                    );
                }
                Err(err) => return Err(err),
            }
            let after = self
                .wait_for_master_status_until(MASTER_STATUS_VERIFY_TIMEOUT, |master| {
                    master.preset == preset
                })
                .await?;

            if after.preset == preset {
                tokio::time::sleep(POST_SET_CONFIG_SETTLE_DELAY).await;
                Ok(after)
            } else {
                Err(ClientError::Protocol(format!(
                    "set config did not persist (before={}, expected={}, after={})",
                    before.preset, preset, after.preset
                )))
            }
        }
        .await
    }

    /// Switch preset (0-3) using an explicit device reset mode.
    pub async fn set_config_with_mode(
        &self,
        preset: u8,
        reset_mode: u8,
    ) -> Result<eeprom::MasterStatusRaw, ClientError> {
        let _state_transition_guard = self.state_transition_lock.lock().await;
        self.set_config_with_mode_locked(preset, reset_mode).await
    }

    /// Reset the device and verify that it becomes temporarily unavailable and
    /// then responsive again when the hardware does not return `ResetComplete`.
    async fn reset_with_verification_locked(&self) -> Result<(), ClientError> {
        self.invalidate_readback_for_command(&commands::reset())
            .await;
        let _levels = self.queue.pause_levels();
        let disconnect_epoch = self.queue.disconnect_epoch();

        async {
            match self
                .execute_write_with_timeout(
                    commands::reset(),
                    Priority::Critical,
                    RESET_RESPONSE_TIMEOUT,
                    "reset",
                )
                .await
            {
                Ok(()) => Ok(()),
                Err(ClientError::Transport(TransportError::Disconnected))
                | Err(ClientError::Transport(TransportError::Timeout))
                | Err(ClientError::Transport(TransportError::Hid(_)))
                | Err(ClientError::Transport(TransportError::Io(_)))
                | Err(ClientError::Timeout) => {
                    tracing::debug!(
                        "reset response dropped during device reset; verifying via master-status recovery"
                    );
                    self.wait_for_reset_recovery_until(RESET_VERIFY_TIMEOUT, disconnect_epoch)
                        .await?;
                    Ok(())
                }
                Err(err) => {
                    tracing::warn!("reset command failed before recovery verification: {err}");
                    Err(err)
                }
            }
        }
        .await
    }

    /// Reboot the device using the official initialize-all flow semantics:
    /// `EnterBootloader (0x3E)` followed by an expected temporary outage and
    /// master-status recovery.
    async fn reboot_with_verification_locked(&self) -> Result<(), ClientError> {
        self.invalidate_readback_for_command(&commands::enter_bootloader())
            .await;
        let _levels = self.queue.pause_levels();
        let disconnect_epoch = self.queue.disconnect_epoch();

        async {
            match self
                .execute_write_with_timeout(
                    commands::enter_bootloader(),
                    Priority::Critical,
                    RESET_RESPONSE_TIMEOUT,
                    "enter bootloader reboot",
                )
                .await
            {
                Ok(()) => {
                    tracing::debug!(
                        "enter bootloader returned an explicit response; verifying recovery"
                    );
                }
                Err(ClientError::Transport(TransportError::Timeout))
                | Err(ClientError::Transport(TransportError::Disconnected))
                | Err(ClientError::Transport(TransportError::Hid(_)))
                | Err(ClientError::Transport(TransportError::Io(_)))
                | Err(ClientError::Timeout) => {
                    tracing::debug!(
                        "enter bootloader response dropped during reboot; verifying via master-status recovery"
                    );
                }
                Err(err) => {
                    tracing::warn!("reboot command failed before recovery verification: {err}");
                    return Err(err);
                }
            }

            if let Err(err) = self
                .wait_for_reset_recovery_until(RESET_VERIFY_TIMEOUT, disconnect_epoch)
                .await
            {
                tracing::warn!("reboot recovery verification failed: {err}");
                return Err(err);
            }
            Ok(())
        }
        .await
    }

    /// Reset the device and verify that it becomes temporarily unavailable and
    /// then responsive again when the hardware does not return `ResetComplete`.
    pub async fn reset_with_verification(&self) -> Result<(), ClientError> {
        let _state_transition_guard = self.state_transition_lock.lock().await;
        self.reset_with_verification_locked().await
    }

    /// Set source by index.
    pub async fn set_source(&self, source: u8) -> Result<(), ClientError> {
        let before = self.get_master_status().await?;
        if before.source == source {
            return Ok(());
        }

        let _levels = self.queue.pause_levels();

        async {
            self.execute_write(commands::set_source(source), Priority::High, "set source")
                .await?;
            let after = self
                .wait_for_master_status_until(MASTER_STATUS_VERIFY_TIMEOUT, |master| {
                    master.source == source
                })
                .await?;

            if after.source == source {
                Ok(())
            } else {
                Err(ClientError::Protocol(format!(
                    "set source did not persist (before={}, expected={}, after={})",
                    before.source, source, after.source
                )))
            }
        }
        .await
    }

    /// Set Dirac bypass state.
    pub async fn set_dirac_bypass(&self, bypass: bool) -> Result<(), ClientError> {
        if !self.profile.has_dirac {
            return Err(ClientError::Unsupported);
        }
        self.execute_write(
            commands::dirac_bypass(bypass),
            Priority::High,
            "set dirac bypass",
        )
        .await
    }

    /// Restore output mute values only if device/preset continuity still matches.
    /// Queue serialization covers raw bridge commands too; no replay after lost ACK.
    pub async fn restore_output_mutes_if_snapshot(
        &self,
        values: &[(u8, bool)],
        expected: &str,
    ) -> Result<(), ClientError> {
        if values.is_empty()
            || expected.is_empty()
            || expected.len() > 256
            || !expected.bytes().all(|b| (0x21..=0x7e).contains(&b))
        {
            return Err(ClientError::Protocol(
                "invalid conditional mute restoration".into(),
            ));
        }
        let table = self.profile.dsp_table.ok_or(ClientError::Unsupported)?;
        let mut seen = HashSet::new();
        let mut payloads = Vec::with_capacity(values.len());
        for &(index, muted) in values {
            if !seen.insert(index) {
                return Err(ClientError::Protocol("duplicate output index".into()));
            }
            let output = table
                .outputs
                .get(index as usize)
                .ok_or_else(|| ClientError::Protocol("invalid output index".into()))?;
            let payload = match self.profile.dialect.value_format {
                ValueFormat::Float32 => commands::write_dsp_u32_le_with_ctrl(
                    output.mute,
                    if muted { 1 } else { 2 },
                    0x80,
                ),
                ValueFormat::FixedPoint => {
                    let bytes =
                        minidsp_protocol::dialect::encode_mute(muted, &self.profile.dialect);
                    let raw = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                    commands::write_dsp(
                        output.mute,
                        minidsp_protocol::dialect::fixed_point_to_float(raw),
                        true,
                        &self.profile.dialect,
                    )
                }
            };
            payloads.push(payload);
        }
        // Observe a changed device-selected preset before queueing any restoration.
        let started_in = self.get_master_status().await?.preset;
        for payload in &payloads {
            self.invalidate_config_readback_for_write(payload).await;
        }
        let responses = match self
            .queue
            .execute_batch_with_snapshot(
                payloads.clone(),
                Priority::Critical,
                Some(expected.to_owned()),
            )
            .await
        {
            Ok(responses) => responses,
            Err(err) => {
                // Earlier mutes in the batch may have been written, into the
                // preset that was active when the restore began.
                self.revoke_snapshot_trust(started_in).await;
                self.invalidate_active_snapshot_readback().await;
                return Err(err.into());
            }
        };
        if self.queue.snapshot_token().as_deref() != Some(expected) {
            self.invalidate_readback_knowledge().await;
            return Err(TransportError::PreconditionFailed.into());
        }
        for (payload, response) in payloads.iter().zip(responses) {
            Self::ensure_write_response(response, "conditional output mute restoration")?;
            self.mirror_raw_command_if_success(payload, &[commands::RESP_OK])
                .await;
        }
        if self.queue.snapshot_token().as_deref() != Some(expected) {
            self.invalidate_readback_knowledge().await;
            return Err(TransportError::PreconditionFailed.into());
        }
        Ok(())
    }

    /// Set channel mute using dialect-aware encoding.
    pub async fn set_channel_mute(&self, addr: u16, muted: bool) -> Result<(), ClientError> {
        match self.profile.dialect.value_format {
            ValueFormat::Float32 => {
                let before = self.read_param_word(addr).await? as u16;
                let previous_shadow = self.selector_shadow_value(addr).await;
                // Mute status: 1=muted, 2=unmuted (matches minidsp-rs WriteInt).
                self.write_selector_int(addr, if muted { 1 } else { 2 }, "set channel mute")
                    .await?;
                self.verify_selector_write_raw(
                    addr,
                    before,
                    previous_shadow,
                    "set channel mute",
                    if muted { "1" } else { "2" },
                    |raw| raw == if muted { 1 } else { 2 },
                )
                .await?;
            }
            ValueFormat::FixedPoint => {
                use minidsp_protocol::dialect::encode_mute;
                let mute_bytes = encode_mute(muted, &self.profile.dialect);
                let raw = u32::from_be_bytes([
                    mute_bytes[0],
                    mute_bytes[1],
                    mute_bytes[2],
                    mute_bytes[3],
                ]);
                let val = minidsp_protocol::dialect::fixed_point_to_float(raw);
                self.write_dsp(addr, val, true).await?;
            }
        }
        Ok(())
    }

    /// Set channel gain using dialect-aware encoding.
    pub async fn set_channel_gain(&self, addr: u16, db: f32) -> Result<(), ClientError> {
        let before = self.read_config_gain_db(addr).await?;
        match self.profile.dialect.value_format {
            ValueFormat::Float32 => self.write_dsp(addr, db, false).await?,
            ValueFormat::FixedPoint => {
                let gain_linear = 10.0f32.powf(db / 20.0);
                self.write_dsp(addr, gain_linear, true).await?;
            }
        }
        let after = self.read_config_gain_db(addr).await?;
        if (after - db).abs() > GAIN_VERIFY_TOLERANCE_DB {
            if self.prefers_scalar_shadow_for_addr(addr) {
                tracing::info!(
                    "accepting shadow-backed gain write at addr {addr} despite non-authoritative live readback (before={before:.6}, expected={db:.6}, after={after:.6})"
                );
                return Ok(());
            }
            return Err(ClientError::Protocol(format!(
                "set channel gain at addr {addr} did not persist (before={before:.6}, expected={db:.6}, after={after:.6})"
            )));
        }
        Ok(())
    }

    /// Set channel gain and request persistence to preset storage on float32
    /// families instead of the lighter RAM-only interactive path.
    pub async fn set_channel_gain_persisted(&self, addr: u16, db: f32) -> Result<(), ClientError> {
        match self.profile.dialect.value_format {
            ValueFormat::Float32 => {
                self.write_dsp_verified(addr, db, true, "set channel gain")
                    .await
            }
            ValueFormat::FixedPoint => self.set_channel_gain(addr, db).await,
        }
    }

    /// Set one input->output routing entry.
    pub async fn set_routing(
        &self,
        input: u8,
        output: u8,
        enabled: bool,
        gain_db: f32,
    ) -> Result<(), ClientError> {
        let table = self.profile.dsp_table.ok_or(ClientError::Unsupported)?;
        let routing = table.routing;
        if input >= routing.rows || output >= routing.cols {
            return Err(ClientError::Protocol(format!(
                "routing index ({input}, {output}) out of range (max {}x{})",
                routing.rows, routing.cols
            )));
        }

        let flat_idx = input as u16 * routing.cols as u16 + output as u16;
        let status_addr = routing.status_base + flat_idx;
        let gain_addr = routing.gain_base + flat_idx;

        self.set_channel_gain(gain_addr, gain_db).await?;
        match self.profile.dialect.value_format {
            ValueFormat::Float32 => {
                let before = self.read_param_word(status_addr).await? as u16;
                let previous_shadow = self.selector_shadow_value(status_addr).await;
                // Mixer status: 2=enabled, 1=disabled (matches minidsp-rs WriteInt).
                self.write_selector_int(
                    status_addr,
                    if enabled { 2 } else { 1 },
                    "set routing status",
                )
                .await?;
                let after = self.read_param_word(status_addr).await? as u16;
                if after == 0 {
                    self.restore_selector_shadow(status_addr, previous_shadow)
                        .await;
                    return Err(ClientError::Protocol(format!(
                        "set routing status at addr {status_addr} did not persist (before={before}, expected={}, after={after})",
                        if enabled {
                            "non-zero / enabled"
                        } else {
                            "1 / disabled"
                        }
                    )));
                }
            }
            ValueFormat::FixedPoint => {
                let before = self.read_param_scalar_u32(status_addr).await?;
                self.write_dsp(status_addr, if enabled { 1.0 } else { 0.0 }, true)
                    .await?;
                let after = self.read_param_scalar_u32(status_addr).await?;
                let expected = if enabled { 1 } else { 0 };
                if after != expected {
                    return Err(ClientError::Protocol(format!(
                        "set routing status at addr {status_addr} did not persist (before={before}, expected={expected}, after={after})"
                    )));
                }
            }
        }
        Ok(())
    }

    /// Set one input->output routing entry while persisting the gain word for
    /// float32 families.
    pub async fn set_routing_persisted(
        &self,
        input: u8,
        output: u8,
        enabled: bool,
        gain_db: f32,
    ) -> Result<(), ClientError> {
        let table = self.profile.dsp_table.ok_or(ClientError::Unsupported)?;
        let routing = table.routing;
        if input >= routing.rows || output >= routing.cols {
            return Err(ClientError::Protocol(format!(
                "routing index ({input}, {output}) out of range (max {}x{})",
                routing.rows, routing.cols
            )));
        }

        let flat_idx = input as u16 * routing.cols as u16 + output as u16;
        let status_addr = routing.status_base + flat_idx;
        let gain_addr = routing.gain_base + flat_idx;

        self.set_channel_gain_persisted(gain_addr, gain_db).await?;
        match self.profile.dialect.value_format {
            ValueFormat::Float32 => {
                let before = self.read_param_word(status_addr).await? as u16;
                let previous_shadow = self.selector_shadow_value(status_addr).await;
                self.write_selector_int(
                    status_addr,
                    if enabled { 2 } else { 1 },
                    "set routing status",
                )
                .await?;
                let after = self.read_param_word(status_addr).await? as u16;
                if after == 0 {
                    self.restore_selector_shadow(status_addr, previous_shadow)
                        .await;
                    return Err(ClientError::Protocol(format!(
                        "set routing status at addr {status_addr} did not persist (before={before}, expected={}, after={after})",
                        if enabled {
                            "non-zero / enabled"
                        } else {
                            "1 / disabled"
                        }
                    )));
                }
            }
            ValueFormat::FixedPoint => {
                let before = self.read_param_scalar_u32(status_addr).await?;
                self.write_dsp(status_addr, if enabled { 1.0 } else { 0.0 }, true)
                    .await?;
                let after = self.read_param_scalar_u32(status_addr).await?;
                let expected = if enabled { 1 } else { 0 };
                if after != expected {
                    return Err(ClientError::Protocol(format!(
                        "set routing status at addr {status_addr} did not persist (before={before}, expected={expected}, after={after})"
                    )));
                }
            }
        }
        Ok(())
    }

    /// Set per-cell polarity inversion for the main routing matrix.
    ///
    /// Only families whose `RoutingMatrix.polarity_base` is populated support
    /// this — currently DSP_110 (Flex Eight), DSP_113 / DSP_115 (Flex HT/HTx),
    /// and the legacy DDRC-88BM. Returns [`ClientError::Unsupported`] for
    /// other families.
    pub async fn set_routing_polarity(
        &self,
        input: u8,
        output: u8,
        inverted: bool,
    ) -> Result<(), ClientError> {
        let table = self.profile.dsp_table.ok_or(ClientError::Unsupported)?;
        let routing = table.routing;
        if input >= routing.rows || output >= routing.cols {
            return Err(ClientError::Protocol(format!(
                "routing index ({input}, {output}) out of range (max {}x{})",
                routing.rows, routing.cols
            )));
        }
        let polarity_base = routing.polarity_base.ok_or(ClientError::Unsupported)?;
        let flat_idx = input as u16 * routing.cols as u16 + output as u16;
        let polarity_addr = polarity_base + flat_idx;
        self.set_invert(polarity_addr, inverted).await
    }

    /// Bass-management address table for this device, or `None` if the
    /// family doesn't expose the HT bass-management block. Currently only
    /// DSP_113 / DSP_115 (Flex HT / HTx 113 + 115 firmware) populate this.
    pub fn bass_management_table(&self) -> Option<&'static BassManagement> {
        self.profile.dsp_table.and_then(|t| t.bass_management)
    }

    /// True if this device exposes the HT bass-management block.
    pub fn supports_bass_management(&self) -> bool {
        self.bass_management_table().is_some()
    }

    fn require_bm_table(&self) -> Result<&'static BassManagement, ClientError> {
        self.bass_management_table().ok_or(ClientError::Unsupported)
    }

    /// Number of BM gain channels (always 8 on the supported HT families).
    const BM_NUM_CHANNELS: u8 = 8;

    fn check_bm_channel(ch: u8) -> Result<(), ClientError> {
        if ch >= Self::BM_NUM_CHANNELS {
            return Err(ClientError::Protocol(format!(
                "bm channel {ch} out of range (0..{})",
                Self::BM_NUM_CHANNELS
            )));
        }
        Ok(())
    }

    /// Read one bass-management channel (gain + mute state).
    pub async fn read_bm_channel(&self, ch: u8) -> Result<BmChannelConfig, ClientError> {
        let bm = self.require_bm_table()?;
        Self::check_bm_channel(ch)?;
        let status_addr = bm.gain_status_base + ch as u16;
        let gain_addr = bm.gain_base + ch as u16;
        let muted = self.read_param_word(status_addr).await? as u16 == 1;
        let gain_db = self.read_config_gain_db(gain_addr).await?;
        Ok(BmChannelConfig {
            index: ch,
            muted,
            gain_db,
        })
    }

    /// Set the gain (dB) for one bass-management channel.
    pub async fn set_bm_channel_gain(&self, ch: u8, gain_db: f32) -> Result<(), ClientError> {
        let bm = self.require_bm_table()?;
        Self::check_bm_channel(ch)?;
        let addr = bm.gain_base + ch as u16;
        self.set_channel_gain(addr, gain_db).await
    }

    /// Set the mute state for one bass-management channel.
    pub async fn set_bm_channel_mute(&self, ch: u8, muted: bool) -> Result<(), ClientError> {
        let bm = self.require_bm_table()?;
        Self::check_bm_channel(ch)?;
        let addr = bm.gain_status_base + ch as u16;
        self.set_channel_mute(addr, muted).await
    }

    /// Read one cell of the BM matrix mixer (status / gain / polarity).
    pub async fn read_bm_mixer_cell(
        &self,
        row: u8,
        col: u8,
    ) -> Result<BmMixerCellConfig, ClientError> {
        let bm = self.require_bm_table()?;
        let mixer = bm.bm_routing;
        let status_addr = mixer.status_addr(row, col).ok_or_else(|| {
            ClientError::Protocol(format!(
                "bm mixer cell ({row},{col}) out of range ({}x{})",
                mixer.rows, mixer.cols
            ))
        })?;
        let gain_addr = mixer.gain_addr(row, col).expect("validated by status_addr");
        let polarity_addr = mixer
            .polarity_addr(row, col)
            .expect("validated by status_addr");
        let enabled = self.read_param_word(status_addr).await? as u16 == 2;
        let gain_db = self.read_config_gain_db(gain_addr).await?;
        let polarity_inverted = self.read_param_word(polarity_addr).await? as u16 != 0;
        Ok(BmMixerCellConfig {
            row,
            col,
            enabled,
            gain_db,
            polarity_inverted,
        })
    }

    /// Apply a patch to one cell of the BM matrix mixer. Only the populated
    /// fields of `patch` are written.
    pub async fn set_bm_mixer_cell(
        &self,
        row: u8,
        col: u8,
        patch: BmMixerCellPatch,
    ) -> Result<(), ClientError> {
        let bm = self.require_bm_table()?;
        let mixer = bm.bm_routing;
        let status_addr = mixer.status_addr(row, col).ok_or_else(|| {
            ClientError::Protocol(format!(
                "bm mixer cell ({row},{col}) out of range ({}x{})",
                mixer.rows, mixer.cols
            ))
        })?;
        let gain_addr = mixer.gain_addr(row, col).expect("validated by status_addr");
        let polarity_addr = mixer
            .polarity_addr(row, col)
            .expect("validated by status_addr");
        if let Some(gain_db) = patch.gain_db {
            self.set_channel_gain(gain_addr, gain_db).await?;
        }
        if let Some(enabled) = patch.enabled {
            // Mixer status: 2=enabled, 1=disabled — same convention as
            // `set_routing` for the main routing matrix.
            let before = self.read_param_word(status_addr).await? as u16;
            let previous_shadow = self.selector_shadow_value(status_addr).await;
            self.write_selector_int(
                status_addr,
                if enabled { 2 } else { 1 },
                "set bm mixer status",
            )
            .await?;
            let after = self.read_param_word(status_addr).await? as u16;
            if after == 0 {
                self.restore_selector_shadow(status_addr, previous_shadow)
                    .await;
                return Err(ClientError::Protocol(format!(
                    "set bm mixer status at addr {status_addr} did not persist (before={before}, after={after})"
                )));
            }
        }
        if let Some(polarity_inverted) = patch.polarity_inverted {
            self.set_invert(polarity_addr, polarity_inverted).await?;
        }
        Ok(())
    }

    /// Apply a patch to one bass-management gain channel.
    pub async fn set_bm_channel(&self, ch: u8, patch: BmChannelPatch) -> Result<(), ClientError> {
        if let Some(gain_db) = patch.gain_db {
            self.set_bm_channel_gain(ch, gain_db).await?;
        }
        if let Some(muted) = patch.muted {
            self.set_bm_channel_mute(ch, muted).await?;
        }
        Ok(())
    }

    /// Read the full bass-management state snapshot (8 channels + 9×8 mixer).
    pub async fn read_bass_management_state(&self) -> Result<BmStateConfig, ClientError> {
        let bm = self.require_bm_table()?;
        let mixer = bm.bm_routing;
        let mut channels = Vec::with_capacity(Self::BM_NUM_CHANNELS as usize);
        for ch in 0..Self::BM_NUM_CHANNELS {
            channels.push(self.read_bm_channel(ch).await?);
        }
        let mut mixer_cells = Vec::with_capacity(mixer.cell_count() as usize);
        for row in 0..mixer.rows {
            for col in 0..mixer.cols {
                mixer_cells.push(self.read_bm_mixer_cell(row, col).await?);
            }
        }
        Ok(BmStateConfig {
            channels,
            mixer: mixer_cells,
            rows: mixer.rows,
            cols: mixer.cols,
        })
    }

    /// Write biquad coefficients to a PEQ band.
    /// Warns (but does not reject) unstable filters for minidsp-rs compatibility.
    pub async fn write_biquad(&self, addr: u16, coeffs: &[f32; 5]) -> Result<(), ClientError> {
        self.peq_readback_known
            .lock()
            .await
            .entry(addr)
            .or_default()
            .0 = false;
        if !minidsp_protocol::biquad::is_stable_any_convention(coeffs) {
            tracing::warn!(
                "writing potentially unstable biquad at addr {addr}: a1={}, a2={}",
                coeffs[3],
                coeffs[4]
            );
        }
        self.execute_write(
            commands::write_biquad(addr, coeffs, &self.profile.dialect),
            Priority::High,
            "write biquad",
        )
        .await?;
        let mut shadow = self.peq_shadow.lock().await;
        let entry = shadow.entry(addr).or_insert(PeqShadow {
            coeffs: minidsp_protocol::biquad::BYPASS,
            bypass: false,
        });
        entry.coeffs = *coeffs;
        self.peq_readback_known
            .lock()
            .await
            .entry(addr)
            .or_default()
            .0 = true;
        Ok(())
    }

    /// Set PEQ bypass state.
    pub async fn bypass_filter(&self, addr: u16, bypass: bool) -> Result<(), ClientError> {
        self.peq_readback_known
            .lock()
            .await
            .entry(addr)
            .or_default()
            .1 = false;
        self.execute_write(
            commands::bypass_filter(addr, bypass, &self.profile.dialect),
            Priority::High,
            "set peq bypass",
        )
        .await?;
        let mut shadow = self.peq_shadow.lock().await;
        let entry = shadow.entry(addr).or_insert(PeqShadow {
            coeffs: minidsp_protocol::biquad::BYPASS,
            bypass: false,
        });
        entry.bypass = bypass;
        self.peq_readback_known
            .lock()
            .await
            .entry(addr)
            .or_default()
            .1 = true;
        Ok(())
    }

    /// Set channel polarity inversion using dialect-aware encoding.
    pub async fn set_invert(&self, addr: u16, inverted: bool) -> Result<(), ClientError> {
        match self.profile.dialect.value_format {
            ValueFormat::Float32 => {
                let before = self.read_channel_invert(addr).await?;
                self.write_dsp_int(addr, if inverted { 1 } else { 0 }, "set invert")
                    .await?;
                let after = self.read_channel_invert(addr).await?;
                if after != inverted {
                    return Err(ClientError::Protocol(format!(
                        "set invert at addr {addr} did not persist (before={before}, expected={inverted}, after={after})"
                    )));
                }
                Ok(())
            }
            ValueFormat::FixedPoint => {
                let before = self.read_channel_invert(addr).await?;
                let val =
                    minidsp_protocol::dialect::polarity_value(inverted, &self.profile.dialect);
                self.write_dsp(addr, val, true).await?;
                let after = self.read_channel_invert(addr).await?;
                if after != inverted {
                    return Err(ClientError::Protocol(format!(
                        "set invert at addr {addr} did not persist (before={before}, expected={inverted}, after={after})"
                    )));
                }
                Ok(())
            }
        }
    }

    /// Write DSP parameter.
    pub async fn write_dsp(&self, addr: u16, value: f32, save: bool) -> Result<(), ClientError> {
        self.scalar_readback_known.lock().await.remove(&addr);
        self.execute_write(
            commands::write_dsp(addr, value, save, &self.profile.dialect),
            Priority::High,
            "write dsp",
        )
        .await?;
        if self.profile.dialect.value_format == ValueFormat::Float32 {
            self.scalar_shadow
                .lock()
                .await
                .insert(addr, value.to_bits());
            self.scalar_readback_known.lock().await.insert(addr);
        }
        Ok(())
    }

    /// Write a float DSP/config parameter and verify the typed word readback on
    /// modern float32 devices.
    pub async fn write_dsp_verified(
        &self,
        addr: u16,
        value: f32,
        save: bool,
        context: &str,
    ) -> Result<(), ClientError> {
        if self.profile.dialect.value_format != ValueFormat::Float32 {
            return self.write_dsp(addr, value, save).await;
        }

        let before = self.read_param_word(addr).await?;
        let expected = value.to_bits();

        self.write_dsp(addr, value, save).await?;

        let after = match self.read_param_word(addr).await {
            Ok(value) => value,
            Err(error) => {
                self.scalar_readback_known.lock().await.remove(&addr);
                return Err(error);
            }
        };
        if after != expected {
            if self.prefers_scalar_shadow_for_addr(addr) {
                tracing::info!(
                    "accepting shadow-backed scalar write at addr {addr} despite non-authoritative live readback (before=0x{before:08X}, expected=0x{expected:08X}, after=0x{after:08X})"
                );
                return Ok(());
            }
            self.scalar_readback_known.lock().await.remove(&addr);
            return Err(ClientError::Protocol(format!(
                "{context} at addr {addr} did not persist (before=0x{before:08X}, expected=0x{expected:08X}, after=0x{after:08X})"
            )));
        }

        Ok(())
    }

    /// Write an integer DSP/config parameter using the modern little-endian
    /// `WriteInt` encoding used by `minidsp-rs` on SHARC devices.
    pub async fn write_dsp_int(
        &self,
        addr: u16,
        value: u16,
        context: &str,
    ) -> Result<(), ClientError> {
        self.execute_write(
            commands::write_dsp_int(addr, value),
            Priority::High,
            context,
        )
        .await
    }

    /// Write an integer DSP/config parameter and verify the typed word readback
    /// on modern float32 devices.
    pub async fn write_dsp_int_verified(
        &self,
        addr: u16,
        value: u16,
        context: &str,
    ) -> Result<(), ClientError> {
        if self.profile.dialect.value_format != ValueFormat::Float32 {
            return self.write_dsp_int(addr, value, context).await;
        }

        let before = self.read_param_word(addr).await?;
        let expected = value as u32;

        self.write_dsp_int(addr, value, context).await?;

        let after = self.read_param_word(addr).await?;
        if after != expected {
            return Err(ClientError::Protocol(format!(
                "{context} at addr {addr} did not persist (before=0x{before:08X}, expected=0x{expected:08X}, after=0x{after:08X})"
            )));
        }

        Ok(())
    }

    /// Write a raw 32-bit DSP/config parameter word.
    pub async fn write_dsp_u32(
        &self,
        addr: u16,
        value: u32,
        save: bool,
    ) -> Result<(), ClientError> {
        self.execute_write(
            commands::write_dsp_u32(addr, value, save),
            Priority::High,
            "write dsp u32",
        )
        .await
    }

    async fn write_selector_int(
        &self,
        addr: u16,
        value: u16,
        context: &str,
    ) -> Result<(), ClientError> {
        self.selector_readback_known.lock().await.remove(&addr);
        self.execute_write(
            commands::write_dsp_u32_le_with_ctrl(addr, value as u32, 0x80),
            Priority::High,
            context,
        )
        .await?;
        self.selector_shadow.lock().await.insert(addr, value);
        self.selector_readback_known.lock().await.insert(addr);
        Ok(())
    }

    async fn restore_selector_shadow(&self, addr: u16, previous: Option<u16>) {
        self.selector_readback_known.lock().await.remove(&addr);
        let mut shadow = self.selector_shadow.lock().await;
        if let Some(value) = previous {
            shadow.insert(addr, value);
        } else {
            shadow.remove(&addr);
        }
    }

    async fn verify_selector_write_raw<F>(
        &self,
        addr: u16,
        before: u16,
        previous_shadow: Option<u16>,
        context: &str,
        expected: &str,
        matches_expected: F,
    ) -> Result<(), ClientError>
    where
        F: Fn(u16) -> bool,
    {
        let after = match self.read_param_word(addr).await {
            Ok(value) => value as u16,
            Err(error) => {
                self.restore_selector_shadow(addr, previous_shadow).await;
                return Err(error);
            }
        };
        if matches_expected(after) {
            return Ok(());
        }
        self.restore_selector_shadow(addr, previous_shadow).await;
        Err(ClientError::Protocol(format!(
            "{context} at addr {addr} did not persist (before={before}, expected={expected}, after={after})"
        )))
    }

    pub async fn set_status_bypass(&self, addr: u16, bypass: bool) -> Result<(), ClientError> {
        match self.profile.dialect.value_format {
            ValueFormat::Float32 => {
                let before = self.read_param_word(addr).await? as u16;
                let previous_shadow = self.selector_shadow_value(addr).await;
                // Compressor/FIR bypass status uses 3=bypassed, 2=enabled on DSP100.
                self.write_selector_int(addr, if bypass { 3 } else { 2 }, "set bypass status")
                    .await?;
                self.verify_selector_write_raw(
                    addr,
                    before,
                    previous_shadow,
                    "set bypass status",
                    if bypass { "1 or 3" } else { "2" },
                    |raw| {
                        if bypass {
                            matches!(raw, 1 | 3)
                        } else {
                            raw == 2
                        }
                    },
                )
                .await
            }
            ValueFormat::FixedPoint => {
                // Keep the existing legacy path until live old-dialect hardware
                // proves a stronger typed-selector rule.
                self.write_dsp(addr, if bypass { 1.0 } else { 0.0 }, true)
                    .await
            }
        }
    }

    /// Write a DSP selector/status word using the official app's raw scalar
    /// `LoadDspParam` shape for modern devices.
    pub async fn write_dsp_selector_u32(&self, addr: u16, value: u32) -> Result<(), ClientError> {
        self.execute_write(
            commands::write_dsp_u32_le_with_ctrl(addr, value, 0x80),
            Priority::High,
            "write dsp selector",
        )
        .await
    }

    /// Write a linked-int 32-bit DSP/config parameter word.
    pub async fn write_dsp_linked_u32(
        &self,
        addr: u16,
        value: u32,
        linked_addrs: &[u16],
    ) -> Result<(), ClientError> {
        self.execute_write(
            commands::write_dsp_linked_u32(addr, value, linked_addrs),
            Priority::High,
            "write dsp linked u32",
        )
        .await
    }

    /// Read float values from DSP memory.
    pub async fn read_floats(&self, addr: u16, count: u8) -> Result<Vec<f32>, ClientError> {
        self.read_floats_with_priority(addr, count, Priority::Normal)
            .await
    }

    async fn read_floats_with_priority(
        &self,
        addr: u16,
        count: u8,
        priority: Priority,
    ) -> Result<Vec<f32>, ClientError> {
        self.read_floats_with_timeout(addr, count, priority, Some(DSP_READ_TIMEOUT))
            .await
    }

    async fn read_floats_with_timeout(
        &self,
        addr: u16,
        count: u8,
        priority: Priority,
        timeout: Option<Duration>,
    ) -> Result<Vec<f32>, ClientError> {
        let payload = commands::read_floats(addr, count, &self.profile.dialect);
        let resp = match timeout {
            Some(timeout) => {
                self.queue
                    .execute_with_timeout(payload, priority, timeout)
                    .await?
            }
            None => self.queue.execute(payload, priority).await?,
        };
        match resp {
            Response::Floats(values) => Ok(values),
            _ => Err(ClientError::Protocol(
                "unexpected response to ReadFloats".into(),
            )),
        }
    }

    /// Read typed 32-bit DSP/config parameter words.
    pub async fn read_param_words(&self, addr: u16, count: u8) -> Result<Vec<u32>, ClientError> {
        let req = commands::read_floats(addr, count, &self.profile.dialect);
        let raw = self
            .queue
            .execute_typed_raw_with_timeout(req.clone(), Priority::Normal, DSP_READ_TIMEOUT)
            .await?;
        commands::parse_param_words_response_for_request(&req, &raw).ok_or_else(|| {
            ClientError::Protocol("unexpected raw response to typed ReadFloats".into())
        })
    }

    async fn read_param_words_be(&self, addr: u16, count: u8) -> Result<Vec<u32>, ClientError> {
        let req = commands::read_floats(addr, count, &self.profile.dialect);
        let raw = self
            .queue
            .execute_typed_raw_with_timeout(req.clone(), Priority::Normal, DSP_READ_TIMEOUT)
            .await?;
        commands::parse_param_words_be_response_for_request(&req, &raw).ok_or_else(|| {
            ClientError::Protocol("unexpected raw response to big-endian ReadFloats".into())
        })
    }

    /// Read the verify-program status byte (0xFFA3) and return it.
    ///
    /// Mirrors Device Console's `verifyProgram()` flow: after a preset save
    /// the device writes a status code into [`eeprom::VERIFY_PROGRAM_ADDR`];
    /// values `<= 0x0D` mean the flash block was persisted cleanly.
    pub async fn read_verify_program_status(&self) -> Result<u8, ClientError> {
        let (hi, lo) = eeprom::VERIFY_PROGRAM_ADDR;
        let resp = self
            .queue
            .execute(commands::read_flash(hi, lo, 1), Priority::Critical)
            .await?;
        match resp {
            Response::FlashData(data) if !data.is_empty() => Ok(data[0]),
            Response::FlashData(_) => Err(ClientError::Protocol(
                "verify-program read returned empty payload".into(),
            )),
            _ => Err(ClientError::Protocol(
                "unexpected response to verify-program ReadFlash".into(),
            )),
        }
    }

    /// Send `CopyPreset` and then verify the save by reading
    /// [`eeprom::VERIFY_PROGRAM_ADDR`]. Returns an error if the device
    /// reports a status byte above [`eeprom::VERIFY_PROGRAM_OK_MAX`], which
    /// Device Console treats as a failed flash persist.
    pub async fn copy_preset_verified(&self) -> Result<(), ClientError> {
        self.invalidate_readback_for_command(&commands::copy_preset(true))
            .await;
        self.queue
            .execute(commands::copy_preset(true), Priority::Critical)
            .await?;
        let status = self.read_verify_program_status().await?;
        if !eeprom::is_verify_program_ok(status) {
            return Err(ClientError::Protocol(format!(
                "verifyProgram failed after CopyPreset: status=0x{status:02X} > 0x{:02X}",
                eeprom::VERIFY_PROGRAM_OK_MAX
            )));
        }
        Ok(())
    }

    fn default_preset_image(&self) -> Result<&'static DefaultPresetImage, ClientError> {
        default_preset_image_for_dsp(self.dsp_version).ok_or(ClientError::Unsupported)
    }

    async fn write_flash_image_block(
        &self,
        block_id: u8,
        block: &[u8],
        context: &str,
    ) -> Result<(), ClientError> {
        for (chunk_idx, chunk) in block.chunks(0x20).enumerate() {
            self.execute_write(
                commands::write_flash_block(block_id, chunk, chunk_idx == 0),
                Priority::Critical,
                context,
            )
            .await?;
        }
        self.execute_write(
            commands::finish_flash_block(block_id),
            Priority::Critical,
            context,
        )
        .await?;
        Ok(())
    }

    async fn write_preset_defaults_locked(&self) -> Result<(), ClientError> {
        self.invalidate_readback_knowledge().await;
        if let Some(slot) = *self.active_preset.lock().await {
            self.trusted_snapshot_presets.lock().await.remove(&slot);
            self.invalidated_snapshot_presets.lock().await.insert(slot);
        }
        let image = self.default_preset_image()?;
        self.write_flash_image_block(
            FLASH_BLOCK_PRESET_VALUES,
            &build_preset_values_block(image.params),
            "write default preset values",
        )
        .await?;
        self.write_flash_image_block(
            FLASH_BLOCK_PRESET_BYPASS,
            &build_preset_bypass_block(image.bypass),
            "write default preset bypass",
        )
        .await?;

        let status = self.read_verify_program_status().await?;
        if !eeprom::is_verify_program_ok(status) {
            return Err(ClientError::Protocol(format!(
                "verifyProgram failed after default preset write: status=0x{status:02X} > 0x{:02X}",
                eeprom::VERIFY_PROGRAM_OK_MAX
            )));
        }
        Ok(())
    }

    async fn save_preset_token_locked(
        &self,
        preset: u8,
        token: [u8; 4],
    ) -> Result<(), ClientError> {
        let (hi, lo) = eeprom::token_addr_for_slot(preset);
        self.execute_write(
            commands::write_flash(hi, lo, &token),
            Priority::High,
            "save modification token",
        )
        .await?;
        self.set_preset_token(preset, token).await;
        Ok(())
    }

    /// Returns true when this profile has an exact default-preset image
    /// bundled for device-side reset/recovery.
    pub fn supports_default_preset_reset(&self) -> bool {
        self.default_preset_image().is_ok()
    }

    /// Rebuild the active preset from the official default preset image and
    /// persist a fresh modification token for it.
    pub async fn reset_active_preset_to_defaults(&self) -> Result<u8, ClientError> {
        let _state_transition_guard = self.state_transition_lock.lock().await;
        let preset = self.get_master_status().await?.preset;
        self.write_preset_defaults_locked().await?;
        let token = generate_modification_token()?;
        self.save_preset_token_locked(preset, token).await?;
        self.set_config_with_mode_locked(preset, commands::PRESET_CHANGE_RELOAD_SAME_SLOT)
            .await?;
        let snapshot = default_preset_shadow(self.profile(), self.default_preset_image()?);
        self.store_preset_shadow_snapshot(preset, snapshot).await;
        self.mark_snapshot_trusted(preset).await;
        Ok(preset)
    }

    /// Switch to a preset without resetting the DSP pipeline, rebuild that slot
    /// from the official default preset image, and persist a fresh token.
    pub async fn reset_preset_to_defaults(&self, preset: u8) -> Result<(), ClientError> {
        let _state_transition_guard = self.state_transition_lock.lock().await;
        self.set_config_with_mode_locked(preset, commands::PRESET_CHANGE_WITHOUT_RESET)
            .await?;
        self.write_preset_defaults_locked().await?;
        let token = generate_modification_token()?;
        self.save_preset_token_locked(preset, token).await?;
        self.set_config_with_mode_locked(preset, commands::PRESET_CHANGE_RELOAD_SAME_SLOT)
            .await?;
        let snapshot = default_preset_shadow(self.profile(), self.default_preset_image()?);
        self.store_preset_shadow_snapshot(preset, snapshot).await;
        self.mark_snapshot_trusted(preset).await;
        Ok(())
    }

    /// Device-only recovery path matching the official "initialize to default
    /// settings" flow for the bundled modern SHARC families: rebuild preset 1,
    /// copy it to the remaining slots, overwrite all four tokens with the
    /// recovery marker, and optionally reboot the unit.
    pub async fn initialize_all_presets_to_defaults(
        &self,
        reboot: bool,
    ) -> Result<(), ClientError> {
        let _state_transition_guard = self.state_transition_lock.lock().await;
        self.set_config_with_mode_locked(0, commands::PRESET_CHANGE_WITHOUT_RESET)
            .await?;
        self.write_preset_defaults_locked().await?;
        self.save_preset_token_locked(0, RECOVERY_MODIFICATION_TOKEN)
            .await?;
        self.set_config_with_mode_locked(0, commands::PRESET_CHANGE_RELOAD_SAME_SLOT)
            .await?;
        self.copy_preset_verified().await?;

        let token_bytes = RECOVERY_MODIFICATION_TOKEN;
        let mut all_tokens = Vec::with_capacity(self.profile.presets as usize * 4);
        for _ in 0..self.profile.presets {
            all_tokens.extend_from_slice(&token_bytes);
        }
        self.execute_write(
            commands::write_flash(eeprom::TOKENS_ADDR.0, eeprom::TOKENS_ADDR.1, &all_tokens),
            Priority::High,
            "write recovery modification tokens",
        )
        .await?;

        let snapshot = default_preset_shadow(self.profile(), self.default_preset_image()?);
        for preset in 0..self.profile.presets {
            self.store_preset_shadow_snapshot(preset, snapshot.clone())
                .await;
            self.set_preset_token(preset, token_bytes).await;
            self.mark_snapshot_trusted(preset).await;
        }

        if reboot {
            self.reboot_with_verification_locked().await?;
        }
        Ok(())
    }

    /// Manual reboot of the MiniDSP unit.
    pub async fn reboot_device(&self) -> Result<(), ClientError> {
        let _state_transition_guard = self.state_transition_lock.lock().await;
        self.reboot_with_verification_locked().await
    }

    /// Read raw bytes from the device's full flash-address space.
    pub async fn read_flash_full_addr(&self, addr: u32, count: u8) -> Result<Vec<u8>, ClientError> {
        let resp = self
            .queue
            .execute(
                commands::read_flash_full_addr(addr, count),
                Priority::Normal,
            )
            .await?;
        match resp {
            Response::FlashData(data) => Ok(data),
            _ => Err(ClientError::Protocol(
                "unexpected response to ReadFlashFullAddr".into(),
            )),
        }
    }

    pub async fn set_channel_delay_samples(
        &self,
        addr: u16,
        samples: u32,
    ) -> Result<(), ClientError> {
        let before = self.read_param_scalar_u32(addr).await?;
        let expected = match self.profile.dialect.value_format {
            ValueFormat::Float32 => samples.min(u16::MAX as u32),
            ValueFormat::FixedPoint => samples,
        };
        self.scalar_readback_known.lock().await.remove(&addr);
        self.execute_write(
            commands::write_dsp_delay(
                addr,
                samples,
                self.profile.dialect.value_format == ValueFormat::FixedPoint,
                &self.profile.dialect,
            ),
            Priority::High,
            "write dsp delay",
        )
        .await?;
        if self.profile.dialect.value_format == ValueFormat::Float32 {
            self.scalar_shadow.lock().await.insert(addr, expected);
            self.scalar_readback_known.lock().await.insert(addr);
        }
        let after = match self.read_param_scalar_u32(addr).await {
            Ok(after) => after,
            Err(err) => {
                self.scalar_readback_known.lock().await.remove(&addr);
                return Err(err);
            }
        };
        if after != expected {
            if self.prefers_scalar_shadow_for_addr(addr) {
                tracing::info!(
                    "accepting shadow-backed delay write at addr {addr} despite non-authoritative live readback (before={before}, expected={expected}, after={after})"
                );
                return Ok(());
            }
            self.scalar_readback_known.lock().await.remove(&addr);
            return Err(ClientError::Protocol(format!(
                "set channel delay at addr {addr} did not persist (before={before}, expected={expected}, after={after})"
            )));
        }
        Ok(())
    }

    /// Set channel delay and request persistence to preset storage on float32
    /// families instead of the lighter RAM-only interactive path.
    pub async fn set_channel_delay_samples_persisted(
        &self,
        addr: u16,
        samples: u32,
    ) -> Result<(), ClientError> {
        let before = self.read_param_scalar_u32(addr).await?;
        let expected = match self.profile.dialect.value_format {
            ValueFormat::Float32 => samples.min(u16::MAX as u32),
            ValueFormat::FixedPoint => samples,
        };
        self.scalar_readback_known.lock().await.remove(&addr);
        self.execute_write(
            commands::write_dsp_delay(addr, samples, true, &self.profile.dialect),
            Priority::High,
            "write dsp delay",
        )
        .await?;
        if self.profile.dialect.value_format == ValueFormat::Float32 {
            self.scalar_shadow.lock().await.insert(addr, expected);
            self.scalar_readback_known.lock().await.insert(addr);
        }
        let after = match self.read_param_scalar_u32(addr).await {
            Ok(after) => after,
            Err(err) => {
                self.scalar_readback_known.lock().await.remove(&addr);
                return Err(err);
            }
        };
        if after != expected {
            if self.prefers_scalar_shadow_for_addr(addr) {
                tracing::info!(
                    "accepting shadow-backed delay write at addr {addr} despite non-authoritative live readback (before={before}, expected={expected}, after={after})"
                );
                return Ok(());
            }
            self.scalar_readback_known.lock().await.remove(&addr);
            return Err(ClientError::Protocol(format!(
                "set channel delay at addr {addr} did not persist (before={before}, expected={expected}, after={after})"
            )));
        }
        Ok(())
    }

    async fn read_param_word(&self, addr: u16) -> Result<u32, ClientError> {
        self.read_param_words(addr, 1)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| ClientError::Protocol("empty typed param response".into()))
    }

    async fn read_param_scalar_u32(&self, addr: u16) -> Result<u32, ClientError> {
        match self.profile.dialect.value_format {
            ValueFormat::Float32 => self.read_param_word(addr).await,
            ValueFormat::FixedPoint => self
                .read_param_words_be(addr, 1)
                .await?
                .into_iter()
                .next()
                .ok_or_else(|| ClientError::Protocol("empty integer param response".into())),
        }
    }

    async fn read_config_gain_db(&self, addr: u16) -> Result<f32, ClientError> {
        match self.profile.dialect.value_format {
            ValueFormat::Float32 => {
                if self.prefers_scalar_shadow_for_addr(addr)
                    && let Some(raw) = self.scalar_shadow.lock().await.get(&addr).copied()
                {
                    return Ok(f32::from_bits(raw));
                }
                let raw = self.read_param_word(addr).await?;
                if self.prefers_scalar_shadow_for_addr(addr)
                    && let Some(value) = sanitize_shadow_preferred_scalar(raw)
                {
                    return Ok(value);
                }
                Ok(f32::from_bits(raw))
            }
            ValueFormat::FixedPoint => {
                let linear = self
                    .read_floats(addr, 1)
                    .await?
                    .into_iter()
                    .next()
                    .ok_or_else(|| ClientError::Protocol("empty gain response".into()))?;
                Ok(20.0 * linear.max(1e-10).log10())
            }
        }
    }

    async fn read_config_mute(&self, addr: u16) -> Result<bool, ClientError> {
        match self.profile.dialect.value_format {
            ValueFormat::Float32 => Ok(self.read_selector_word(addr).await? == 1),
            ValueFormat::FixedPoint => {
                let value = self
                    .read_floats(addr, 1)
                    .await?
                    .into_iter()
                    .next()
                    .ok_or_else(|| ClientError::Protocol("empty mute response".into()))?;
                Ok(value == 0.0)
            }
        }
    }

    async fn read_selector_word(&self, addr: u16) -> Result<u16, ClientError> {
        let raw = self.read_param_word(addr).await? as u16;
        if raw == 1
            && let Some(&shadow) = self.selector_shadow.lock().await.get(&addr)
            && shadow != 1
        {
            return Ok(shadow);
        }
        Ok(raw)
    }

    async fn selector_shadow_value(&self, addr: u16) -> Option<u16> {
        self.selector_shadow.lock().await.get(&addr).copied()
    }

    async fn read_config_delay_ms(&self, addr: u16) -> Result<f32, ClientError> {
        match self.profile.dialect.value_format {
            ValueFormat::Float32 => {
                let samples = if self.prefers_scalar_shadow_for_addr(addr) {
                    if let Some(raw) = self.scalar_shadow.lock().await.get(&addr).copied() {
                        raw as f32
                    } else {
                        let raw = self.read_param_word(addr).await?;
                        if raw <= 2 { 0.0 } else { raw as f32 }
                    }
                } else {
                    self.read_param_word(addr).await? as f32
                };
                Ok(samples * 1000.0 / self.profile.sample_rate as f32)
            }
            ValueFormat::FixedPoint => {
                let samples = self
                    .read_param_words_be(addr, 1)
                    .await?
                    .into_iter()
                    .next()
                    .ok_or_else(|| ClientError::Protocol("empty delay response".into()))?;
                Ok(samples as f32 * 1000.0 / self.profile.sample_rate as f32)
            }
        }
    }

    pub async fn read_channel_invert(&self, addr: u16) -> Result<bool, ClientError> {
        match self.profile.dialect.value_format {
            ValueFormat::Float32 => Ok(self.read_param_word(addr).await? != 0),
            ValueFormat::FixedPoint => {
                let value = self
                    .read_floats(addr, 1)
                    .await?
                    .into_iter()
                    .next()
                    .ok_or_else(|| ClientError::Protocol("empty invert response".into()))?;
                Ok(value < 0.0)
            }
        }
    }

    /// Load FIR coefficients using the device's staged FIR transfer protocol.
    pub async fn load_fir_coefficients(
        &self,
        fir: FirFilter,
        coeffs: &[f32],
    ) -> Result<(), ClientError> {
        if coeffs.len() > fir.max_taps as usize {
            return Err(ClientError::Protocol(format!(
                "FIR has {} taps but device maximum is {}",
                coeffs.len(),
                fir.max_taps
            )));
        }

        let master = self.get_master_status().await?;
        let mut result = Ok(());
        for attempt in 0..2 {
            result = self.load_fir_transfer(fir, coeffs).await;
            if attempt == 0
                && matches!(
                    result,
                    Err(ClientError::Transport(
                        TransportError::Disconnected | TransportError::Timeout
                    ))
                )
            {
                // Data packets append without an offset. Recover at Start, never
                // by replaying an individual packet with uncertain delivery.
                continue;
            }
            break;
        }
        if result.is_err() {
            self.fir_shadow.lock().await.remove(&fir.data_start);
            if let Some(preset) = *self.active_preset.lock().await {
                self.trusted_snapshot_presets.lock().await.remove(&preset);
                self.preset_shadows.lock().await.remove(&preset);
            }
            if let Err(err) = self
                .execute_write_with_timeout(
                    commands::fir_load_end(),
                    Priority::Critical,
                    FIR_LOAD_END_TIMEOUT,
                    "abort fir load",
                )
                .await
            {
                tracing::warn!("failed to end FIR transfer after error: {err}");
            }
        }
        if result.is_ok() {
            self.selector_shadow
                .lock()
                .await
                .insert(fir.taps_count, coeffs.len() as u16);
            self.fir_shadow
                .lock()
                .await
                .insert(fir.data_start, coeffs.to_vec());
        }
        let mute_result = self.set_mute(master.is_muted()).await;
        if let Err(err) = &mute_result {
            tracing::warn!("failed to restore mute after FIR transfer: {err}");
        }
        result?;
        mute_result?;
        Ok(())
    }

    async fn load_fir_transfer(&self, fir: FirFilter, coeffs: &[f32]) -> Result<(), ClientError> {
        let max_coeffs = match self
            .raw_command(commands::fir_load_start(fir.index), Priority::Critical)
            .await?
        {
            Response::FirLoadInfo { max_coeffs } => max_coeffs,
            Response::Error(data) => {
                return Err(ClientError::Protocol(format!(
                    "fir load start: device error {data:02x?}"
                )));
            }
            other => {
                return Err(ClientError::Protocol(format!(
                    "fir load start: unexpected response {other:?}"
                )));
            }
        };
        if coeffs.len() > max_coeffs as usize {
            return Err(ClientError::Protocol(format!(
                "FIR has {} taps but device reported runtime maximum {}",
                coeffs.len(),
                max_coeffs,
            )));
        }
        self.execute_write(
            commands::write_dsp_int(fir.taps_count, coeffs.len() as u16),
            Priority::Critical,
            "set fir tap count",
        )
        .await?;
        for chunk in coeffs.chunks(14) {
            self.execute_write(
                commands::fir_load_data(fir.index, chunk),
                Priority::Critical,
                "fir load data",
            )
            .await?;
        }
        self.execute_write_with_timeout(
            commands::fir_load_end(),
            Priority::Critical,
            FIR_LOAD_END_TIMEOUT,
            "fir load end",
        )
        .await
    }

    /// Reset every PEQ band on every input and output channel to the
    /// passthrough identity biquad and clear all bypass flags. This matches
    /// the official Device Console "Initialize to default Settings" action,
    /// which writes the same identity biquad (`<dec>1,0,0,0,0</dec>`) to
    /// every PEQ slot per
    /// `protocol/assets/audio_firmware/{family}/DSP_default_parameters.xml`
    /// `<filter name="PEQ_*_*">` defaults.
    ///
    /// Returns the number of bands that were touched.
    ///
    /// The official app also seeds the *display* frequency for each band from
    /// the [`minidsp_protocol::device::dsp::DEFAULT_PEQ_FREQS_HZ`] doubling
    /// schedule (20, 40, 80, ..., 10240 Hz). The device itself doesn't store
    /// frequency — only biquad coefficients — so we expose that schedule
    /// separately via [`Self::default_peq_frequencies`] for clients that want
    /// to render an authentic "default" UI after a reset.
    pub async fn reset_all_peq_to_defaults(&self) -> Result<usize, ClientError> {
        let table = self.profile.dsp_table.ok_or(ClientError::Unsupported)?;

        let mut touched = 0usize;
        for input in table.inputs {
            for band in input.peq {
                self.write_biquad(band.addr, &minidsp_protocol::biquad::BYPASS)
                    .await?;
                self.bypass_filter(band.addr, false).await?;
                touched += 1;
            }
        }
        for output in table.outputs {
            for band in output.peq {
                self.write_biquad(band.addr, &minidsp_protocol::biquad::BYPASS)
                    .await?;
                self.bypass_filter(band.addr, false).await?;
                touched += 1;
            }
        }
        Ok(touched)
    }

    /// Returns the official 10-band default PEQ frequency schedule (20, 40,
    /// 80, ..., 10240 Hz) for clients that want to render the same default
    /// UI labels Device Console uses after a reset. The device itself does
    /// not store frequency — see [`Self::reset_all_peq_to_defaults`] for the
    /// hardware-side reset.
    pub fn default_peq_frequencies(&self) -> &'static [u32; 10] {
        minidsp_protocol::device::dsp::default_peq_frequencies()
    }

    /// Live readback of the active FIR tap count for a given FIR index using
    /// `CMD_GET_NUM_FIR_TAPS (0x39)` — the same opcode that's already issued
    /// inside `load_fir`, but exposed standalone so `/devices/{id}/config` and
    /// the CLI can ask the device "how many taps does this FIR slot currently
    /// have loaded?" without kicking off an upload cycle.
    ///
    /// Device Console calls this `getNumAvailTaps(chanIdx)` and uses it as the
    /// authoritative source for the "available taps" UI element, which
    /// directly plugs the PROGRESS.md gap about DSP100 fresh-client FIR tap
    /// count needing verified-shadow fallback — the count is *live-queryable*,
    /// we just weren't exposing it.
    pub async fn read_fir_tap_count(&self, fir_index: u8) -> Result<u16, ClientError> {
        match self
            .raw_command(commands::get_num_fir_taps(fir_index), Priority::Normal)
            .await?
        {
            Response::FirLoadInfo { max_coeffs } => Ok(max_coeffs),
            Response::Error(data) => Err(ClientError::Protocol(format!(
                "get_num_fir_taps({fir_index}): device error {data:02x?}"
            ))),
            other => Err(ClientError::Protocol(format!(
                "get_num_fir_taps({fir_index}): unexpected response {other:?}"
            ))),
        }
    }

    /// Send raw command payload. For advanced/raw access.
    pub async fn raw_command(
        &self,
        payload: Vec<u8>,
        priority: Priority,
    ) -> Result<Response, ClientError> {
        Ok(self.queue.execute(payload, priority).await?)
    }

    /// Access the command queue directly.
    pub fn queue(&self) -> &Arc<CommandQueue> {
        &self.queue
    }

    /// Override the device profile (for --force-kind).
    pub async fn override_profile(&mut self, profile: DeviceProfile) {
        self.queue.set_dialect(profile.dialect).await;
        self.profile = profile;
    }

    pub async fn saved_measurement_state(&self) -> Option<SavedPeqState> {
        self.saved_peq.lock().await.clone()
    }

    pub async fn set_saved_measurement_state(&self, state: Option<SavedPeqState>) {
        *self.saved_peq.lock().await = state;
    }

    /// Enter measurement mode: save all PEQ states, then bypass all PEQ bands.
    ///
    /// Reads current coefficients and bypass state for every PEQ band on every
    /// input and output channel, stores them internally, then sets all bands to bypass.
    pub async fn enter_measurement_mode(&self) -> Result<(), ClientError> {
        self.synchronize_readback_epoch().await;
        let table = self.profile.dsp_table.ok_or(ClientError::Unsupported)?;
        let mut bands: Vec<(u16, [f32; 5], bool)> = Vec::new();

        // Collect all PEQ band addresses from inputs
        for input in table.inputs {
            for band in input.peq {
                let coeffs = self.read_peq_coeffs(band.addr).await?;
                let bypass = self.read_peq_bypass(band.addr).await.unwrap_or(false);
                bands.push((band.addr, coeffs, bypass));
            }
        }

        // Collect all PEQ band addresses from outputs
        for output in table.outputs {
            for band in output.peq {
                let coeffs = self.read_peq_coeffs(band.addr).await?;
                let bypass = self.read_peq_bypass(band.addr).await.unwrap_or(false);
                bands.push((band.addr, coeffs, bypass));
            }
        }

        // Save the state
        *self.saved_peq.lock().await = Some(SavedPeqState {
            bands: bands.clone(),
        });

        // Now bypass all bands
        let mut bypass_cmds = Vec::new();
        for &(addr, _, _) in &bands {
            bypass_cmds.push(commands::bypass_filter(addr, true, &self.profile.dialect));
        }
        for payload in &bypass_cmds {
            self.invalidate_config_readback_for_write(payload).await;
        }
        ensure_write_batch(
            self.queue
                .execute_batch(bypass_cmds, Priority::Critical)
                .await?,
            "enter measurement mode bypass",
        )?;
        apply_peq_shadow_bypass(&self.peq_shadow, &bands, true).await;
        {
            let mut known = self.peq_readback_known.lock().await;
            for &(addr, _, _) in &bands {
                known.entry(addr).or_default().1 = true;
            }
        }

        tracing::info!(
            "measurement mode: saved and bypassed {} PEQ bands",
            bands.len()
        );
        Ok(())
    }

    /// Restore saved PEQ state from measurement mode.
    ///
    /// Writes back all saved coefficients and bypass states using safe batch ordering.
    pub async fn restore_measurement_mode(&self) -> Result<(), ClientError> {
        let saved = self
            .saved_peq
            .lock()
            .await
            .take()
            .ok_or_else(|| ClientError::Protocol("no saved measurement mode state".into()))?;

        self.restore_measurement_mode_with(&saved).await?;

        tracing::info!("measurement mode: restored {} PEQ bands", saved.bands.len());
        Ok(())
    }

    pub async fn restore_measurement_mode_with(
        &self,
        saved: &SavedPeqState,
    ) -> Result<(), ClientError> {
        {
            let mut known = self.peq_readback_known.lock().await;
            for &(addr, _, _) in &saved.bands {
                known.remove(&addr);
            }
        }
        self.synchronize_readback_epoch().await;
        let result = self.restore_measurement_batches(saved).await;
        if result.is_err() {
            self.invalidate_active_snapshot_readback().await;
        }
        result
    }

    async fn restore_measurement_batches(&self, saved: &SavedPeqState) -> Result<(), ClientError> {
        let current_shadow = self.peq_shadow.lock().await.clone();
        let (phase1_cmds, phase2_cmds, phase3_cmds) =
            build_measurement_restore_batches(&current_shadow, &saved.bands, &self.profile.dialect);

        if !phase1_cmds.is_empty() {
            ensure_write_batch(
                self.queue
                    .execute_batch(phase1_cmds, Priority::Critical)
                    .await?,
                "measurement restore: bypass before coefficient write",
            )?;
        }

        if !phase2_cmds.is_empty() {
            ensure_write_batch(
                self.queue
                    .execute_batch(phase2_cmds, Priority::Critical)
                    .await?,
                "measurement restore: write coefficients",
            )?;
        }

        if !phase3_cmds.is_empty() {
            ensure_write_batch(
                self.queue
                    .execute_batch(phase3_cmds, Priority::Critical)
                    .await?,
                "measurement restore: restore bypass",
            )?;
        }

        let mut shadow = self.peq_shadow.lock().await;
        for &(addr, coeffs, bypass) in &saved.bands {
            shadow.insert(addr, PeqShadow { coeffs, bypass });
            self.peq_readback_known
                .lock()
                .await
                .insert(addr, (true, true));
        }
        Ok(())
    }

    /// Check if measurement mode state is currently saved.
    pub async fn has_saved_measurement_state(&self) -> bool {
        self.saved_peq.lock().await.is_some()
    }

    /// Batch write PEQ coefficients with partial-write recovery.
    ///
    /// Safe ordering:
    /// 1. Bypass all bands first (safe state)
    /// 2. Write coefficients
    /// 3. Set final bypass states
    ///
    /// Uses execute_batch for contiguous execution.
    pub async fn write_peq_batch(
        &self,
        bands: &[(u16, [f32; 5], bool)],
    ) -> Result<(), ClientError> {
        if bands.is_empty() {
            return Ok(());
        }
        {
            let mut known = self.peq_readback_known.lock().await;
            for &(addr, _, _) in bands {
                known.remove(&addr);
            }
        }

        {
            let mut pending = self.pending_write.lock().await;
            *pending = Some(PendingWrite {
                bands: bands.to_vec(),
            });
        }

        self.synchronize_readback_epoch().await;
        let mut reconnect_epoch = self.queue.reconnect_epoch();
        loop {
            let replay = {
                let pending = self.pending_write.lock().await;
                pending.as_ref().map(|pending| pending.bands.clone())
            };
            let replay =
                replay.ok_or_else(|| ClientError::Protocol("missing pending PEQ write".into()))?;

            if let Err(err) = execute_peq_batch(
                &self.queue,
                &self.profile.dialect,
                &self.peq_shadow,
                &replay,
            )
            .await
            {
                self.invalidate_active_snapshot_readback().await;
                return Err(err);
            }

            let current_epoch = self.queue.reconnect_epoch();
            if current_epoch == reconnect_epoch {
                break;
            }

            tracing::warn!(
                "replaying pending PEQ write after reconnect (epoch {} -> {})",
                reconnect_epoch,
                current_epoch
            );
            reconnect_epoch = current_epoch;
        }

        {
            let mut known = self.peq_readback_known.lock().await;
            for &(addr, _, _) in bands {
                known.insert(addr, (true, true));
            }
        }
        *self.pending_write.lock().await = None;
        self.note_preset_mutation().await;
        Ok(())
    }

    /// Read 5 biquad coefficients from a PEQ band address.
    async fn read_peq_coeffs(&self, addr: u16) -> Result<[f32; 5], ClientError> {
        if self.profile.has_unreliable_selector_readback {
            // Devices with unreliable selector readback also have non-authoritative
            // PEQ/crossover coefficient reads. Prefer the shadow/default model
            // directly so `/config` remains fast instead of timing out one band
            // at a time on all-zero placeholder reads.
            let shadow = self.peq_shadow.lock().await;
            return Ok(shadow
                .get(&addr)
                .map(|s| s.coeffs)
                .unwrap_or(minidsp_protocol::biquad::BYPASS));
        }

        let values = self.read_floats(addr, 5).await.unwrap_or_default();
        if values.len() >= 5 {
            let coeffs = [values[0], values[1], values[2], values[3], values[4]];
            if coeffs != [0.0; 5] {
                return Ok(coeffs);
            }
        }
        let shadow = self.peq_shadow.lock().await;
        Ok(shadow
            .get(&addr)
            .map(|s| s.coeffs)
            .unwrap_or(minidsp_protocol::biquad::BYPASS))
    }

    /// Return cached PEQ bypass state, or None when this address is absent.
    /// This helper performs no device read and does not establish provenance.
    async fn read_peq_bypass(&self, addr: u16) -> Option<bool> {
        self.peq_shadow.lock().await.get(&addr).map(|s| s.bypass)
    }

    pub(super) async fn invalidate_readback_for_command(&self, payload: &[u8]) {
        if !payload
            .first()
            .copied()
            .is_some_and(commands::invalidates_config_readback)
        {
            return;
        }
        self.invalidate_readback_knowledge().await;
        // Switching, reloading, resetting or rebooting leaves the stored presets
        // as they were, so their trust stays. A copy replaces a stored preset.
        if payload.first() == Some(&commands::CMD_COPY_PRESET) {
            self.revoke_all_snapshot_trust().await;
        }
    }

    pub(super) async fn invalidate_readback_knowledge(&self) {
        self.peq_readback_known.lock().await.clear();
        self.scalar_readback_known.lock().await.clear();
        self.selector_readback_known.lock().await.clear();
    }

    async fn config_gain_readback(
        &self,
        addr: u16,
        report: &mut ConfigReadback,
        path: String,
    ) -> f32 {
        let result: Result<(f32, ConfigReadbackSource), ClientError> = async {
            if matches!(self.profile.dialect.value_format, ValueFormat::Float32) {
                let preferred = self.prefers_scalar_shadow_for_addr(addr);
                if preferred && let Some(raw) = self.scalar_shadow.lock().await.get(&addr).copied()
                {
                    return Ok((
                        f32::from_bits(raw),
                        if self.scalar_readback_known.lock().await.contains(&addr) {
                            ConfigReadbackSource::Shadow
                        } else {
                            ConfigReadbackSource::Default
                        },
                    ));
                }
                let raw = self.read_param_word(addr).await?;
                let value = if preferred {
                    sanitize_shadow_preferred_scalar(raw).unwrap_or(f32::from_bits(raw))
                } else {
                    f32::from_bits(raw)
                };
                Ok((
                    value,
                    if preferred {
                        ConfigReadbackSource::Default
                    } else {
                        ConfigReadbackSource::Device
                    },
                ))
            } else {
                self.read_config_gain_db(addr)
                    .await
                    .map(|v| (v, ConfigReadbackSource::Device))
            }
        }
        .await;
        match result {
            Ok((value, source)) => {
                report.record(
                    path,
                    source,
                    (!value.is_finite()).then(|| "non-finite gain readback".into()),
                );
                value
            }
            Err(error) => {
                report.record(path, ConfigReadbackSource::Default, Some(error.to_string()));
                0.0
            }
        }
    }

    async fn config_selector_readback(
        &self,
        addr: u16,
        bypass: bool,
        report: &mut ConfigReadback,
        path: String,
    ) -> bool {
        let result = if matches!(self.profile.dialect.value_format, ValueFormat::Float32) {
            match self.read_param_word(addr).await {
                Ok(raw) => {
                    let raw = raw as u16;
                    let shadow = self.selector_shadow_value(addr).await;
                    let replacement = if raw == 1 {
                        shadow.filter(|&v| v != 1)
                    } else {
                        None
                    };
                    let value = replacement.unwrap_or(raw);
                    let source = if replacement.is_some()
                        || (self.profile.has_unreliable_selector_readback && shadow == Some(raw))
                    {
                        if self.selector_readback_known.lock().await.contains(&addr) {
                            ConfigReadbackSource::Shadow
                        } else {
                            ConfigReadbackSource::Default
                        }
                    } else if self.profile.has_unreliable_selector_readback {
                        ConfigReadbackSource::Default
                    } else {
                        ConfigReadbackSource::Device
                    };
                    Ok((
                        if bypass {
                            matches!(value, 1 | 3)
                        } else {
                            value == 1
                        },
                        source,
                    ))
                }
                Err(error) => Err(error),
            }
        } else {
            self.read_config_mute(addr)
                .await
                .map(|value| (value, ConfigReadbackSource::Device))
        };
        match result {
            Ok((value, source)) => {
                report.record(path, source, None);
                value
            }
            Err(error) => {
                report.record(path, ConfigReadbackSource::Default, Some(error.to_string()));
                false
            }
        }
    }

    async fn config_routing_readback(
        &self,
        addr: u16,
        report: &mut ConfigReadback,
        path: String,
    ) -> bool {
        if matches!(self.profile.dialect.value_format, ValueFormat::Float32)
            && let Some(shadow) = self.selector_shadow_value(addr).await
        {
            report.record(
                path,
                if self.selector_readback_known.lock().await.contains(&addr) {
                    ConfigReadbackSource::Shadow
                } else {
                    ConfigReadbackSource::Default
                },
                None,
            );
            return shadow == 2;
        }
        match self.read_param_word(addr).await {
            Ok(raw) => {
                let ambiguous = matches!(self.profile.dialect.value_format, ValueFormat::Float32)
                    && (self.profile.has_unreliable_selector_readback || raw == 1);
                report.record(
                    path,
                    if ambiguous {
                        ConfigReadbackSource::Default
                    } else {
                        ConfigReadbackSource::Device
                    },
                    None,
                );
                raw != 0
            }
            Err(error) => {
                report.record(path, ConfigReadbackSource::Default, Some(error.to_string()));
                false
            }
        }
    }

    async fn config_delay_readback(
        &self,
        addr: u16,
        report: &mut ConfigReadback,
        path: String,
    ) -> f32 {
        let source = if self.prefers_scalar_shadow_for_addr(addr) {
            if self.scalar_shadow.lock().await.contains_key(&addr)
                && self.scalar_readback_known.lock().await.contains(&addr)
            {
                ConfigReadbackSource::Shadow
            } else {
                ConfigReadbackSource::Default
            }
        } else {
            ConfigReadbackSource::Device
        };
        match self.read_config_delay_ms(addr).await {
            Ok(value) => {
                report.record(
                    path,
                    source,
                    (!value.is_finite()).then(|| "non-finite delay readback".into()),
                );
                value
            }
            Err(error) => {
                report.record(path, ConfigReadbackSource::Default, Some(error.to_string()));
                0.0
            }
        }
    }

    async fn config_peq_readback(
        &self,
        addr: u16,
        report: &mut ConfigReadback,
        path: String,
    ) -> [f32; 5] {
        let mut error = None;
        if !self.profile.has_unreliable_selector_readback {
            match self.read_floats(addr, 5).await {
                Ok(values) if values.len() >= 5 && values[..5] != [0.0; 5] => {
                    let coeffs = [values[0], values[1], values[2], values[3], values[4]];
                    report.record(
                        path,
                        ConfigReadbackSource::Device,
                        (!coeffs.iter().all(|v| v.is_finite()))
                            .then(|| "non-finite biquad readback".into()),
                    );
                    return coeffs;
                }
                Ok(_) => error = Some("missing or all-zero biquad readback".into()),
                Err(err) => error = Some(err.to_string()),
            }
        }
        let known = self
            .peq_readback_known
            .lock()
            .await
            .get(&addr)
            .is_some_and(|v| v.0);
        let shadow = self.peq_shadow.lock().await;
        let coeffs = shadow
            .get(&addr)
            .map(|s| s.coeffs)
            .unwrap_or(minidsp_protocol::biquad::BYPASS);
        if !coeffs.iter().all(|v| v.is_finite()) && error.is_none() {
            error = Some("non-finite biquad shadow".into());
        }
        report.record(
            path,
            if known && shadow.contains_key(&addr) {
                ConfigReadbackSource::Shadow
            } else {
                ConfigReadbackSource::Default
            },
            error,
        );
        coeffs
    }

    async fn config_peq_bypass_readback(
        &self,
        addr: u16,
        report: &mut ConfigReadback,
        path: String,
    ) -> bool {
        let known = self
            .peq_readback_known
            .lock()
            .await
            .get(&addr)
            .is_some_and(|v| v.1);
        let value = self.read_peq_bypass(addr).await;
        report.record(
            path,
            if known && value.is_some() {
                ConfigReadbackSource::Shadow
            } else {
                ConfigReadbackSource::Default
            },
            None,
        );
        value.unwrap_or(false)
    }

    /// Read the full device config including all PEQ coefficients.
    pub async fn read_device_config(&self) -> Result<DeviceConfig, ClientError> {
        let read_epoch = self.synchronize_readback_epoch().await;
        let snapshot_token = self.queue.snapshot_token();
        let master_raw = self.get_master_status().await?;
        let mut readback = ConfigReadback::default();
        for field in ["preset", "source", "volume", "mute"] {
            readback.record(
                format!("master_status.{field}"),
                ConfigReadbackSource::Device,
                None,
            );
        }
        let source_name = self
            .profile
            .sources
            .source_for_index(master_raw.source)
            .map(|s| s.pascal_case().to_string())
            .unwrap_or_else(|| format!("Unknown({})", master_raw.source));
        let dirac = if self.profile.has_dirac {
            let response = self
                .queue
                .execute(commands::read_flash(0xFF, 0xE0, 1), Priority::Normal)
                .await;
            match response {
                Ok(Response::FlashData(data)) if !data.is_empty() => {
                    readback.record("master_status.dirac", ConfigReadbackSource::Device, None);
                    Some(data[0] == 0)
                }
                other => {
                    readback.record(
                        "master_status.dirac",
                        ConfigReadbackSource::Default,
                        Some(format!("Dirac read failed: {other:?}")),
                    );
                    Some(false)
                }
            }
        } else {
            None
        };
        let master_status = MasterStatusView {
            preset: master_raw.preset,
            source: source_name,
            volume: master_raw.volume_db(),
            mute: master_raw.is_muted(),
            dirac,
        };
        let mut inputs = Vec::new();
        let mut outputs = Vec::new();
        if let Some(table) = self.profile.dsp_table {
            for (ch_idx, input) in table.inputs.iter().enumerate() {
                let prefix = format!("inputs.{ch_idx}");
                let gain = if let Some(addr) = input.gain {
                    let value = self
                        .config_gain_readback(addr, &mut readback, format!("{prefix}.gain"))
                        .await;
                    if readback.fields.last().is_some_and(|f| f.error.is_some()) {
                        readback.fields.last_mut().unwrap().source =
                            ConfigReadbackSource::Unavailable;
                        None
                    } else {
                        Some(value)
                    }
                } else {
                    None
                };
                let mute = if let Some(addr) = input.mute {
                    let value = self
                        .config_selector_readback(
                            addr,
                            false,
                            &mut readback,
                            format!("{prefix}.mute"),
                        )
                        .await;
                    if readback.fields.last().is_some_and(|f| f.error.is_some()) {
                        readback.fields.last_mut().unwrap().source =
                            ConfigReadbackSource::Unavailable;
                        None
                    } else {
                        Some(value)
                    }
                } else {
                    None
                };
                let mut peq = Vec::new();
                for (band_idx, band) in input.peq.iter().enumerate() {
                    let coeffs = self
                        .config_peq_readback(
                            band.addr,
                            &mut readback,
                            format!("{prefix}.peq.{band_idx}.coeff"),
                        )
                        .await;
                    let bypass = self
                        .config_peq_bypass_readback(
                            band.addr,
                            &mut readback,
                            format!("{prefix}.peq.{band_idx}.bypass"),
                        )
                        .await;
                    peq.push(PeqBandConfig {
                        index: band_idx as u8,
                        coeff: BiquadCoeffs::from_array(&coeffs),
                        bypass,
                    });
                }
                let mut routing_entries = Vec::new();
                let routing = table.routing;
                if ch_idx < routing.rows as usize {
                    for output in 0..routing.cols {
                        let flat_idx = ch_idx as u16 * routing.cols as u16 + output as u16;
                        let route = format!("{prefix}.routing.{output}");
                        let enabled = self
                            .config_routing_readback(
                                routing.status_base + flat_idx,
                                &mut readback,
                                format!("{route}.enabled"),
                            )
                            .await;
                        let gain_db = self
                            .config_gain_readback(
                                routing.gain_base + flat_idx,
                                &mut readback,
                                format!("{route}.gain_db"),
                            )
                            .await;
                        let polarity_inverted = if let Some(base) = routing.polarity_base {
                            match self.read_param_scalar_u32(base + flat_idx).await {
                                Ok(raw) => {
                                    readback.record(
                                        format!("{route}.polarity_inverted"),
                                        ConfigReadbackSource::Device,
                                        None,
                                    );
                                    Some(raw != 0)
                                }
                                Err(error) => {
                                    readback.record(
                                        format!("{route}.polarity_inverted"),
                                        ConfigReadbackSource::Unavailable,
                                        Some(error.to_string()),
                                    );
                                    None
                                }
                            }
                        } else {
                            None
                        };
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
                    gain,
                    mute,
                    peq,
                    routing: routing_entries,
                });
            }
            for (ch_idx, output) in table.outputs.iter().enumerate() {
                let prefix = format!("outputs.{ch_idx}");
                let gain_val = self
                    .config_gain_readback(output.gain, &mut readback, format!("{prefix}.gain"))
                    .await;
                let mute_val = self
                    .config_selector_readback(
                        output.mute,
                        false,
                        &mut readback,
                        format!("{prefix}.mute"),
                    )
                    .await;
                let delay_val = if let Some(addr) = output.delay {
                    self.config_delay_readback(addr, &mut readback, format!("{prefix}.delay"))
                        .await
                } else {
                    readback.record(
                        format!("{prefix}.delay"),
                        ConfigReadbackSource::Default,
                        None,
                    );
                    0.0
                };
                let mut peq = Vec::new();
                for (band_idx, band) in output.peq.iter().enumerate() {
                    let coeffs = self
                        .config_peq_readback(
                            band.addr,
                            &mut readback,
                            format!("{prefix}.peq.{band_idx}.coeff"),
                        )
                        .await;
                    let bypass = self
                        .config_peq_bypass_readback(
                            band.addr,
                            &mut readback,
                            format!("{prefix}.peq.{band_idx}.bypass"),
                        )
                        .await;
                    peq.push(PeqBandConfig {
                        index: band_idx as u8,
                        coeff: BiquadCoeffs::from_array(&coeffs),
                        bypass,
                    });
                }
                let mut crossover = Vec::new();
                if let Some(xover) = output.crossover {
                    for (group_idx, base_addr) in [(0u8, xover.lpf_addr), (1u8, xover.hpf_addr)] {
                        let group = format!("{prefix}.crossover.{group_idx}");
                        let mut filters = Vec::new();
                        for band_idx in 0..4u8 {
                            let coeffs = self
                                .config_peq_readback(
                                    base_addr + band_idx as u16 * 5,
                                    &mut readback,
                                    format!("{group}.filters.{band_idx}.coeff"),
                                )
                                .await;
                            filters.push(IndexedBiquadConfig {
                                index: band_idx,
                                coeff: BiquadCoeffs::from_array(&coeffs),
                            });
                        }
                        let bypass = self
                            .config_peq_bypass_readback(
                                base_addr,
                                &mut readback,
                                format!("{group}.bypass"),
                            )
                            .await;
                        crossover.push(CrossoverGroupConfig {
                            index: group_idx,
                            filters,
                            bypass,
                        });
                    }
                }
                let fir = if let Some(fir) = output.fir {
                    let shadow = self.selector_shadow_value(fir.taps_count).await;
                    let read = self.read_param_scalar_u32(fir.taps_count).await;
                    let (active_taps, source, error) = match read {
                        Ok(_value)
                            if self.profile.has_unreliable_selector_readback
                                && shadow.is_some() =>
                        {
                            (
                                shadow.unwrap(),
                                if self
                                    .selector_readback_known
                                    .lock()
                                    .await
                                    .contains(&fir.taps_count)
                                {
                                    ConfigReadbackSource::Shadow
                                } else {
                                    ConfigReadbackSource::Default
                                },
                                None,
                            )
                        }
                        Ok(value) if (1..=fir.max_taps as u32).contains(&value) => {
                            (value as u16, ConfigReadbackSource::Device, None)
                        }
                        other => (
                            shadow.unwrap_or(0),
                            if shadow.is_some()
                                && self
                                    .selector_readback_known
                                    .lock()
                                    .await
                                    .contains(&fir.taps_count)
                            {
                                ConfigReadbackSource::Shadow
                            } else {
                                ConfigReadbackSource::Default
                            },
                            Some(format!("FIR tap count unavailable: {other:?}")),
                        ),
                    };
                    readback.record(format!("{prefix}.fir.active_taps"), source, error);
                    let bypass = self
                        .config_selector_readback(
                            fir.bypass,
                            true,
                            &mut readback,
                            format!("{prefix}.fir.bypass"),
                        )
                        .await;
                    Some(FirConfig {
                        bypass,
                        active_taps,
                    })
                } else {
                    None
                };
                let compressor = if let Some(comp) = &output.compressor {
                    let base = format!("{prefix}.compressor");
                    Some(CompressorConfig {
                        bypass: self
                            .config_selector_readback(
                                comp.bypass,
                                true,
                                &mut readback,
                                format!("{base}.bypass"),
                            )
                            .await,
                        threshold: self
                            .config_gain_readback(
                                comp.threshold,
                                &mut readback,
                                format!("{base}.threshold"),
                            )
                            .await,
                        gain: self
                            .config_gain_readback(comp.gain, &mut readback, format!("{base}.gain"))
                            .await,
                        ratio: self
                            .config_gain_readback(
                                comp.ratio,
                                &mut readback,
                                format!("{base}.ratio"),
                            )
                            .await,
                        knee: self
                            .config_gain_readback(comp.knee, &mut readback, format!("{base}.knee"))
                            .await,
                        attack: self
                            .config_gain_readback(
                                comp.attack,
                                &mut readback,
                                format!("{base}.attack"),
                            )
                            .await,
                        release: self
                            .config_gain_readback(
                                comp.release,
                                &mut readback,
                                format!("{base}.release"),
                            )
                            .await,
                    })
                } else {
                    None
                };
                outputs.push(OutputChannelConfig {
                    index: ch_idx as u8,
                    gain: gain_val,
                    mute: mute_val,
                    delay: delay_val,
                    peq,
                    crossover,
                    fir,
                    compressor,
                });
            }
        }
        if self.queue.reconnect_epoch() != read_epoch {
            self.synchronize_readback_epoch().await;
            readback.invalidate("device reconnected during config readback");
        }
        if snapshot_token.is_some() && self.queue.snapshot_token() == snapshot_token {
            readback.snapshot_token = snapshot_token;
        }
        Ok(DeviceConfig {
            readback: Some(readback),
            master_status,
            inputs,
            outputs,
        })
    }

    async fn read_meters_with_priority(
        &self,
        addrs: &[u16],
        priority: Priority,
    ) -> Result<Vec<f32>, ClientError> {
        if addrs.is_empty() {
            return Ok(vec![]);
        }
        let mut all_values = Vec::with_capacity(addrs.len());

        // Check if addresses are contiguous for bulk read
        let is_contiguous = addrs.len() <= 14 && addrs.windows(2).all(|w| w[1] == w[0] + 1);

        if is_contiguous {
            all_values.extend(
                self.read_meter_run_best_effort(addrs[0], addrs.len(), priority)
                    .await,
            );
        } else {
            // Find contiguous runs and read each run in bulk
            let mut i = 0;
            while i < addrs.len() {
                let start = i;
                while i + 1 < addrs.len() && addrs[i + 1] == addrs[i] + 1 && (i - start + 1) < 14 {
                    i += 1;
                }
                let count = (i - start + 1).min(14);
                all_values.extend(
                    self.read_meter_run_best_effort(addrs[start], count, priority)
                        .await,
                );
                i += 1;
            }
        }

        Ok(sanitize_meter_values(&all_values))
    }

    async fn read_status_meters_with_priority(
        &self,
        addrs: &[u16],
        priority: Priority,
    ) -> Vec<f32> {
        match self.read_meters_with_priority(addrs, priority).await {
            Ok(values) => values,
            Err(err) => {
                tracing::warn!(
                    transport = %self.transport_url,
                    ?addrs,
                    ?priority,
                    "status meter read failed, using silence fallback: {err}"
                );
                vec![SILENT_METER_DB; addrs.len()]
            }
        }
    }

    async fn read_meter_run_with_priority(
        &self,
        addr: u16,
        count: usize,
        priority: Priority,
    ) -> Result<Vec<f32>, ClientError> {
        let values = self
            .read_floats_with_timeout(addr, count as u8, priority, Some(METER_READ_TIMEOUT))
            .await?;
        if values.len() != count {
            return Err(ClientError::Protocol(format!(
                "meter read at addr {addr} returned {} values, expected {count}",
                values.len()
            )));
        }
        Ok(values)
    }

    async fn read_meter_run_best_effort(
        &self,
        addr: u16,
        count: usize,
        priority: Priority,
    ) -> Vec<f32> {
        match self
            .read_meter_run_with_priority(addr, count, priority)
            .await
        {
            Ok(values) => values,
            Err(err) => {
                tracing::warn!(
                    transport = %self.transport_url,
                    addr,
                    count,
                    ?priority,
                    "bulk meter read failed, retrying per-address: {err}"
                );
                let mut values = Vec::with_capacity(count);
                for offset in 0..count {
                    let current_addr = addr + offset as u16;
                    match self
                        .read_meter_run_with_priority(current_addr, 1, priority)
                        .await
                    {
                        Ok(mut single) => values.push(single.remove(0)),
                        Err(single_err) => {
                            tracing::warn!(
                                transport = %self.transport_url,
                                addr = current_addr,
                                ?priority,
                                "single meter read failed, using silence fallback: {single_err}"
                            );
                            values.push(SILENT_METER_DB);
                        }
                    }
                }
                values
            }
        }
    }
}

fn sanitize_meter_values(values: &[f32]) -> Vec<f32> {
    // SHARC-family meter registers already report dB-like float values.
    // Re-applying log10() here turns valid negative readings like -120 dB
    // into misleading positive numbers (~41.58). Return the raw meter
    // values and only clamp pathological silence / NaN cases.
    values
        .iter()
        .map(|&v| {
            if !v.is_finite() || v <= SILENT_METER_DB || v.abs() < 1e-10 {
                SILENT_METER_DB
            } else {
                v
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidsp_protocol::dialect::Dialect;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

    struct DummyTransport;

    impl crate::transport::Transport for DummyTransport {
        fn send_frame(
            &self,
            _payload: &[u8],
        ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
            Box::pin(async { Ok(()) })
        }

        fn recv_frame(
            &self,
            _timeout_ms: u64,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, TransportError>> + Send + '_>> {
            Box::pin(async { Err(TransportError::Timeout) })
        }

        fn is_connected(&self) -> bool {
            true
        }

        fn close(&self) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
            Box::pin(async { Ok(()) })
        }

        fn url(&self) -> String {
            "mock://shadow".into()
        }
    }

    struct DelayedPresetTransport {
        current_preset: Arc<AtomicU8>,
        last_payload: Arc<Mutex<Vec<u8>>>,
        set_config_targets: Arc<Mutex<Vec<u8>>>,
    }

    impl DelayedPresetTransport {
        fn new(initial_preset: u8) -> Self {
            Self {
                current_preset: Arc::new(AtomicU8::new(initial_preset)),
                last_payload: Arc::new(Mutex::new(Vec::new())),
                set_config_targets: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl crate::transport::Transport for DelayedPresetTransport {
        fn send_frame(
            &self,
            payload: &[u8],
        ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
            let payload = payload.to_vec();
            let current_preset = self.current_preset.clone();
            let last_payload = self.last_payload.clone();
            let set_config_targets = self.set_config_targets.clone();

            Box::pin(async move {
                if payload.first().copied() == Some(commands::CMD_SET_CONFIG) && payload.len() >= 2
                {
                    let target = payload[1];
                    set_config_targets.lock().await.push(target);
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        current_preset.store(target, Ordering::SeqCst);
                    });
                }

                *last_payload.lock().await = payload;
                Ok(())
            })
        }

        fn recv_frame(
            &self,
            _timeout_ms: u64,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, TransportError>> + Send + '_>> {
            let current_preset = self.current_preset.clone();
            let last_payload = self.last_payload.clone();

            Box::pin(async move {
                let payload = last_payload.lock().await.clone();
                match payload.first().copied() {
                    Some(commands::CMD_SET_CONFIG) => Ok(payload),
                    Some(commands::CMD_READ_FLASH)
                        if payload.len() >= 4
                            && payload[1] == eeprom::MASTER_STATUS_ADDR.0
                            && payload[2] == eeprom::MASTER_STATUS_ADDR.1 =>
                    {
                        Ok(vec![
                            commands::CMD_READ_FLASH,
                            current_preset.load(Ordering::SeqCst),
                            0,
                            0,
                            0,
                        ])
                    }
                    Some(opcode) => Err(TransportError::Io(format!(
                        "unexpected transport payload {opcode:#04x}: {payload:02x?}"
                    ))),
                    None => Err(TransportError::Io("missing transport payload".into())),
                }
            })
        }

        fn is_connected(&self) -> bool {
            true
        }

        fn close(&self) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
            Box::pin(async { Ok(()) })
        }

        fn url(&self) -> String {
            "mock://preset-race".into()
        }
    }

    #[derive(Clone)]
    struct TimeoutingPresetTransport {
        current_preset: Arc<AtomicU8>,
        last_payload: Arc<Mutex<Vec<u8>>>,
    }

    impl TimeoutingPresetTransport {
        fn new(initial_preset: u8) -> Self {
            Self {
                current_preset: Arc::new(AtomicU8::new(initial_preset)),
                last_payload: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl crate::transport::Transport for TimeoutingPresetTransport {
        fn send_frame(
            &self,
            payload: &[u8],
        ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
            let payload = payload.to_vec();
            let current_preset = self.current_preset.clone();
            let last_payload = self.last_payload.clone();

            Box::pin(async move {
                if payload.first().copied() == Some(commands::CMD_SET_CONFIG) && payload.len() >= 2
                {
                    let target = payload[1];
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        current_preset.store(target, Ordering::SeqCst);
                    });
                }

                *last_payload.lock().await = payload;
                Ok(())
            })
        }

        fn recv_frame(
            &self,
            timeout_ms: u64,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, TransportError>> + Send + '_>> {
            let current_preset = self.current_preset.clone();
            let last_payload = self.last_payload.clone();

            Box::pin(async move {
                let payload = last_payload.lock().await.clone();
                match payload.first().copied() {
                    Some(commands::CMD_SET_CONFIG) => {
                        tokio::time::sleep(Duration::from_millis(timeout_ms)).await;
                        Err(TransportError::Timeout)
                    }
                    Some(commands::CMD_READ_FLASH)
                        if payload.len() >= 4
                            && payload[1] == eeprom::MASTER_STATUS_ADDR.0
                            && payload[2] == eeprom::MASTER_STATUS_ADDR.1 =>
                    {
                        Ok(vec![
                            commands::CMD_READ_FLASH,
                            current_preset.load(Ordering::SeqCst),
                            0,
                            0,
                            0,
                        ])
                    }
                    Some(opcode) => Err(TransportError::Io(format!(
                        "unexpected transport payload {opcode:#04x}: {payload:02x?}"
                    ))),
                    None => Err(TransportError::Io("missing transport payload".into())),
                }
            })
        }

        fn is_connected(&self) -> bool {
            true
        }

        fn close(&self) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
            Box::pin(async { Ok(()) })
        }

        fn reconnect(
            &self,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<Box<dyn crate::transport::Transport>, TransportError>>
                    + Send
                    + '_,
            >,
        > {
            let transport = self.clone();
            Box::pin(async move { Ok(Box::new(transport) as Box<dyn crate::transport::Transport>) })
        }

        fn url(&self) -> String {
            "mock://preset-timeout".into()
        }
    }

    #[derive(Clone)]
    struct RecoveringResetTransport {
        last_payload: Arc<Mutex<Vec<u8>>>,
        read_timeouts_remaining: Arc<AtomicUsize>,
        status_unavailable: Arc<AtomicBool>,
    }

    impl RecoveringResetTransport {
        fn new(read_timeouts_remaining: usize) -> Self {
            Self {
                last_payload: Arc::new(Mutex::new(Vec::new())),
                read_timeouts_remaining: Arc::new(AtomicUsize::new(read_timeouts_remaining)),
                status_unavailable: Arc::new(AtomicBool::new(false)),
            }
        }
    }

    impl crate::transport::Transport for RecoveringResetTransport {
        fn send_frame(
            &self,
            payload: &[u8],
        ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
            let payload = payload.to_vec();
            let last_payload = self.last_payload.clone();
            let status_unavailable = self.status_unavailable.clone();
            let read_timeouts_remaining = self.read_timeouts_remaining.clone();

            Box::pin(async move {
                if payload.first() == Some(&commands::CMD_READ_FLASH) {
                    let unavailable = read_timeouts_remaining
                        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                            remaining.checked_sub(1)
                        })
                        .is_ok();
                    status_unavailable.store(unavailable, Ordering::SeqCst);
                }
                *last_payload.lock().await = payload;
                Ok(())
            })
        }

        fn recv_frame(
            &self,
            timeout_ms: u64,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, TransportError>> + Send + '_>> {
            let last_payload = self.last_payload.clone();
            let status_unavailable = self.status_unavailable.clone();

            Box::pin(async move {
                let payload = last_payload.lock().await.clone();
                match payload.first().copied() {
                    Some(commands::CMD_RESET) => {
                        tokio::time::sleep(Duration::from_millis(timeout_ms)).await;
                        Err(TransportError::Timeout)
                    }
                    Some(commands::CMD_READ_FLASH)
                        if payload.len() >= 4
                            && payload[1] == eeprom::MASTER_STATUS_ADDR.0
                            && payload[2] == eeprom::MASTER_STATUS_ADDR.1 =>
                    {
                        if status_unavailable.load(Ordering::SeqCst) {
                            tokio::time::sleep(Duration::from_millis(timeout_ms)).await;
                            Err(TransportError::Timeout)
                        } else {
                            Ok(vec![commands::CMD_READ_FLASH, 0, 0, 0, 0])
                        }
                    }
                    Some(opcode) => Err(TransportError::Io(format!(
                        "unexpected transport payload {opcode:#04x}: {payload:02x?}"
                    ))),
                    None => Err(TransportError::Io("missing transport payload".into())),
                }
            })
        }

        fn is_connected(&self) -> bool {
            true
        }

        fn close(&self) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
            Box::pin(async { Ok(()) })
        }

        fn reconnect(
            &self,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<Box<dyn crate::transport::Transport>, TransportError>>
                    + Send
                    + '_,
            >,
        > {
            let transport = self.clone();
            Box::pin(async move { Ok(Box::new(transport) as Box<dyn crate::transport::Transport>) })
        }

        fn url(&self) -> String {
            "mock://reset-timeout".into()
        }
    }

    #[derive(Clone)]
    struct RebootCommandDropTransport {
        last_payload: Arc<Mutex<Vec<u8>>>,
        drop_reboot_send_once: Arc<AtomicBool>,
        reopen_failures_remaining: Arc<AtomicUsize>,
    }

    impl RebootCommandDropTransport {
        fn new() -> Self {
            Self {
                last_payload: Arc::new(Mutex::new(Vec::new())),
                drop_reboot_send_once: Arc::new(AtomicBool::new(true)),
                reopen_failures_remaining: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    impl crate::transport::Transport for RebootCommandDropTransport {
        fn send_frame(
            &self,
            payload: &[u8],
        ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
            let payload = payload.to_vec();
            let last_payload = self.last_payload.clone();
            let drop_reboot_send_once = self.drop_reboot_send_once.clone();

            Box::pin(async move {
                *last_payload.lock().await = payload.clone();
                if matches!(
                    payload.first().copied(),
                    Some(commands::CMD_ENTER_BOOTLOADER | commands::CMD_RESET)
                ) && drop_reboot_send_once.swap(false, Ordering::SeqCst)
                {
                    return Err(TransportError::Hid(
                        "device vanished while reboot command was in flight".into(),
                    ));
                }
                Ok(())
            })
        }

        fn recv_frame(
            &self,
            _timeout_ms: u64,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, TransportError>> + Send + '_>> {
            let last_payload = self.last_payload.clone();

            Box::pin(async move {
                let payload = last_payload.lock().await.clone();
                match payload.first().copied() {
                    Some(commands::CMD_READ_FLASH)
                        if payload.len() >= 4
                            && payload[1] == eeprom::MASTER_STATUS_ADDR.0
                            && payload[2] == eeprom::MASTER_STATUS_ADDR.1 =>
                    {
                        Ok(vec![commands::CMD_READ_FLASH, 0, 0, 0, 0])
                    }
                    Some(opcode) => Err(TransportError::Io(format!(
                        "unexpected transport payload {opcode:#04x}: {payload:02x?}"
                    ))),
                    None => Err(TransportError::Io("missing transport payload".into())),
                }
            })
        }

        fn is_connected(&self) -> bool {
            true
        }

        fn close(&self) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
            Box::pin(async { Ok(()) })
        }

        fn reconnect(
            &self,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<Box<dyn crate::transport::Transport>, TransportError>>
                    + Send
                    + '_,
            >,
        > {
            let transport = self.clone();
            Box::pin(async move {
                if transport
                    .reopen_failures_remaining
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                        remaining.checked_sub(1)
                    })
                    .is_ok()
                {
                    return Err(TransportError::NotFound);
                }
                Ok(Box::new(transport) as Box<dyn crate::transport::Transport>)
            })
        }

        fn url(&self) -> String {
            "mock://reboot-command-drop".into()
        }
    }

    fn scripted_client(transport: Box<dyn crate::transport::Transport>) -> MiniDspClient {
        MiniDspClient {
            queue: Arc::new(CommandQueue::new(transport)),
            transport_url: "mock://shadow".into(),
            profile: probe::identify(10, 100),
            hw_id: HardwareId {
                fw_major: 1,
                fw_minor: 0,
                hw_id: 10,
            },
            dsp_version: 100,
            serial: 912345,
            saved_peq: Mutex::new(None),
            volume_settings: Default::default(),
            pending_write: Arc::new(Mutex::new(None)),
            peq_shadow: Arc::new(Mutex::new(HashMap::new())),
            peq_readback_known: Mutex::new(HashMap::new()),
            scalar_readback_known: Mutex::new(HashSet::new()),
            selector_readback_known: Mutex::new(HashSet::new()),
            invalidated_snapshot_presets: Mutex::new(HashSet::new()),
            readback_epoch: Mutex::new(0),
            selector_shadow: Arc::new(Mutex::new(HashMap::new())),
            scalar_shadow: Arc::new(Mutex::new(HashMap::new())),
            fir_shadow: Arc::new(Mutex::new(HashMap::new())),
            preset_shadows: Arc::new(Mutex::new(HashMap::new())),
            preset_tokens: Arc::new(Mutex::new(HashMap::new())),
            trusted_snapshot_presets: Arc::new(Mutex::new(HashSet::new())),
            skip_trusted_shadow_capture_once: Arc::new(Mutex::new(HashSet::new())),
            active_preset: Mutex::new(Some(0)),
            state_transition_lock: Mutex::new(()),
        }
    }

    /// A device behind a bridge that clamps volume writes to `ceiling` (raw).
    #[derive(Clone)]
    struct CeilingTransport {
        volume: Arc<AtomicU8>,
        ceiling: u8,
        url: &'static str,
        last_payload: Arc<Mutex<Vec<u8>>>,
    }

    impl crate::transport::Transport for CeilingTransport {
        fn send_frame(
            &self,
            payload: &[u8],
        ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
            let payload = payload.to_vec();
            Box::pin(async move {
                if payload.first() == Some(&commands::CMD_SET_VOLUME) {
                    self.volume
                        .store(payload[1].max(self.ceiling), Ordering::SeqCst);
                }
                *self.last_payload.lock().await = payload;
                Ok(())
            })
        }

        fn recv_frame(
            &self,
            _timeout_ms: u64,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, TransportError>> + Send + '_>> {
            Box::pin(async move {
                let payload = self.last_payload.lock().await.clone();
                match payload.first().copied() {
                    Some(commands::CMD_SET_VOLUME) => Ok(payload),
                    Some(commands::CMD_READ_FLASH) => Ok(vec![
                        commands::CMD_READ_FLASH,
                        0,
                        0,
                        self.volume.load(Ordering::SeqCst),
                        0,
                    ]),
                    other => Err(TransportError::Io(format!(
                        "unexpected transport payload {other:02x?}"
                    ))),
                }
            })
        }

        fn is_connected(&self) -> bool {
            true
        }

        fn close(&self) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
            Box::pin(async { Ok(()) })
        }

        fn url(&self) -> String {
            self.url.into()
        }
    }

    fn ceiling_client(
        url: &'static str,
        volume: u8,
        ceiling: u8,
    ) -> (MiniDspClient, Arc<AtomicU8>) {
        let volume = Arc::new(AtomicU8::new(volume));
        let transport = CeilingTransport {
            volume: volume.clone(),
            ceiling,
            url,
            last_payload: Arc::new(Mutex::new(Vec::new())),
        };
        let mut client = scripted_client(Box::new(transport));
        client.transport_url = url.into();
        (client, volume)
    }

    #[tokio::test]
    async fn bridged_volume_clamp_is_accepted_without_the_full_timeout() {
        // -30 dB, asking for 0 dB through a bridge capped at -20 dB.
        let (client, volume) = ceiling_client("tcp://bridge:5333", 60, 40);
        let started = Instant::now();
        client.set_volume(0).await.unwrap();
        assert_eq!(volume.load(Ordering::SeqCst), 40);
        assert!(started.elapsed() < BRIDGED_CEILING_SETTLE);

        // Already at the ceiling: the write changes nothing, so it settles.
        let (client, volume) = ceiling_client("ws://bridge:5380/devices/0/ws", 40, 40);
        let started = Instant::now();
        client.set_volume(0).await.unwrap();
        assert_eq!(volume.load(Ordering::SeqCst), 40);
        assert!(started.elapsed() < MASTER_STATUS_VERIFY_TIMEOUT);
    }

    #[tokio::test]
    async fn direct_usb_volume_writes_still_verify_exactly() {
        let (client, volume) = ceiling_client("usb:test", 60, 0);
        client.set_volume(20).await.unwrap();
        assert_eq!(volume.load(Ordering::SeqCst), 20);
    }

    #[derive(Clone)]
    struct ScriptTransport {
        responses: Arc<Mutex<std::collections::VecDeque<Vec<u8>>>>,
        disconnect_data_once: Option<Arc<AtomicBool>>,
        sent: Arc<Mutex<Vec<Vec<u8>>>>,
    }

    impl crate::transport::Transport for ScriptTransport {
        fn send_frame(
            &self,
            payload: &[u8],
        ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
            let payload = payload.to_vec();
            Box::pin(async move {
                self.sent.lock().await.push(payload.clone());
                Ok(())
            })
        }
        fn recv_frame(
            &self,
            _: u64,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, TransportError>> + Send + '_>> {
            Box::pin(async move {
                if self
                    .sent
                    .lock()
                    .await
                    .last()
                    .and_then(|payload| payload.first())
                    == Some(&commands::CMD_FIR_LOAD_DATA)
                    && self
                        .disconnect_data_once
                        .as_ref()
                        .is_some_and(|flag| flag.swap(false, Ordering::SeqCst))
                {
                    return Err(TransportError::Disconnected);
                }
                self.responses
                    .lock()
                    .await
                    .pop_front()
                    .ok_or_else(|| TransportError::Io("script exhausted".into()))
            })
        }
        fn is_connected(&self) -> bool {
            true
        }
        fn close(&self) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
            Box::pin(async { Ok(()) })
        }
        fn reconnect(
            &self,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<Box<dyn crate::transport::Transport>, TransportError>>
                    + Send
                    + '_,
            >,
        > {
            let transport = self.clone();
            Box::pin(async move { Ok(Box::new(transport) as Box<dyn crate::transport::Transport>) })
        }
        fn url(&self) -> String {
            "script://client".into()
        }
    }

    struct BlockingRestoreTransport {
        opcode: AtomicU8,
        sent: Arc<Mutex<Vec<Vec<u8>>>>,
        writing: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }

    impl crate::transport::Transport for BlockingRestoreTransport {
        fn send_frame(
            &self,
            payload: &[u8],
        ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
            self.opcode.store(payload[0], Ordering::SeqCst);
            let payload = payload.to_vec();
            Box::pin(async move {
                self.sent.lock().await.push(payload);
                Ok(())
            })
        }
        fn recv_frame(
            &self,
            _: u64,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, TransportError>> + Send + '_>> {
            Box::pin(async move {
                if self.opcode.load(Ordering::SeqCst) == commands::CMD_WRITE_BIQUAD {
                    self.writing.notify_one();
                    self.release.notified().await;
                    Ok(vec![commands::RESP_ERROR, 1])
                } else {
                    Ok(vec![commands::CMD_READ_FLASH, 0, 0, 0, 0])
                }
            })
        }
        fn is_connected(&self) -> bool {
            true
        }
        fn close(&self) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
            Box::pin(async { Ok(()) })
        }
        fn url(&self) -> String {
            "script://restore".into()
        }
    }

    #[tokio::test]
    async fn same_slot_snapshot_restore_revokes_mute_token_even_when_payload_replay_fails() {
        let writing = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let sent = Arc::new(Mutex::new(Vec::new()));
        let client = Arc::new(scripted_client(Box::new(BlockingRestoreTransport {
            opcode: AtomicU8::new(0),
            sent: sent.clone(),
            writing: writing.clone(),
            release: release.clone(),
        })));
        *client.active_preset.lock().await = Some(0);
        let old_token = client.queue.snapshot_token().unwrap();
        let addr = client.profile.dsp_table.unwrap().inputs[0].peq[0].addr;
        let preset = crate::snapshot::PresetSnapshot {
            slot: 0,
            token: None,
            complete: false,
            provenance: Vec::new(),
            completeness: crate::snapshot::SnapshotCompleteness::default(),
            shadow: Some(PresetShadowState {
                peq: HashMap::from([(
                    addr,
                    CachedPeqShadow {
                        coeffs: [1.0; 5],
                        bypass: false,
                    },
                )]),
                ..PresetShadowState::default()
            }),
            fir_blocks: Vec::new(),
        };
        let worker = client.clone();
        let restore =
            tokio::spawn(async move { worker.restore_preset_snapshot(&preset, None).await });
        tokio::time::timeout(Duration::from_secs(1), writing.notified())
            .await
            .unwrap();
        assert!(client.state_transition_lock.try_lock().is_err());
        assert_ne!(
            client.queue.snapshot_token().as_deref(),
            Some(old_token.as_str())
        );
        assert!(
            !sent
                .lock()
                .await
                .iter()
                .any(|payload| payload[0] == commands::CMD_SET_CONFIG)
        );
        release.notify_one();
        assert!(restore.await.unwrap().is_err());
        assert!(client.state_transition_lock.try_lock().is_ok());
        let before = sent.lock().await.len();
        let error = client
            .restore_output_mutes_if_snapshot(&[(0, false)], &old_token)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ClientError::Transport(TransportError::PreconditionFailed)
        ));
        assert!(
            sent.lock().await[before..]
                .iter()
                .all(|payload| !commands::is_write_command(payload[0]))
        );
    }

    #[tokio::test]
    async fn token_refresh_invalidates_changed_trusted_presets() {
        let transport = ScriptTransport {
            responses: Arc::new(Mutex::new(std::collections::VecDeque::from([vec![
                commands::CMD_READ_FLASH,
                2,
                2,
                2,
                2,
                1,
                1,
                1,
                1,
                1,
                1,
                1,
                1,
                1,
                1,
                1,
                1,
            ]]))),
            disconnect_data_once: None,
            sent: Arc::new(Mutex::new(Vec::new())),
        };
        let client = scripted_client(Box::new(transport));
        for preset in 0..4 {
            client.set_preset_token(preset, [1; 4]).await;
            client
                .store_preset_shadow_snapshot(preset, PresetShadowState::default())
                .await;
            client.mark_snapshot_trusted(preset).await;
        }
        client
            .scalar_shadow
            .lock()
            .await
            .insert(42, 9.5f32.to_bits());
        client.refresh_modification_tokens().await.unwrap();
        let state = client.export_shadow_state().await;
        assert!(!state.trusted_snapshot_presets.contains(&0));
        assert!(state.trusted_snapshot_presets.contains(&1));
        assert!(!state.scalar.contains_key(&42));
        client.update_active_preset(1).await;
        assert!(!client.preset_shadows.lock().await.contains_key(&0));
    }

    #[tokio::test]
    async fn ambiguous_fir_data_restarts_from_start_without_replaying_chunk() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let client = scripted_client(Box::new(ScriptTransport {
            responses: Arc::new(Mutex::new(std::collections::VecDeque::from([
                vec![commands::CMD_READ_FLASH, 0, 0, 0, 0],
                vec![commands::CMD_FIR_LOAD_START, 0, 32],
                vec![commands::RESP_OK],
                vec![commands::CMD_FIR_LOAD_START, 0, 32],
                vec![commands::RESP_OK],
                vec![commands::RESP_OK],
                vec![commands::RESP_OK],
                vec![commands::CMD_READ_FLASH, 0, 0, 0, 0],
            ]))),
            sent: sent.clone(),
            disconnect_data_once: Some(Arc::new(AtomicBool::new(true))),
        }));
        let fir = client
            .profile
            .dsp_table
            .unwrap()
            .outputs
            .iter()
            .find_map(|output| output.fir)
            .unwrap();
        client
            .load_fir_coefficients(fir, &[0.0, 1.0])
            .await
            .unwrap();
        let opcodes: Vec<u8> = sent.lock().await.iter().map(|payload| payload[0]).collect();
        assert_eq!(
            &opcodes[1..8],
            &[
                commands::CMD_FIR_LOAD_START,
                commands::CMD_WRITE_DSP,
                commands::CMD_FIR_LOAD_DATA,
                commands::CMD_FIR_LOAD_START,
                commands::CMD_WRITE_DSP,
                commands::CMD_FIR_LOAD_DATA,
                commands::CMD_RELOAD_DSP_PARAM,
            ]
        );
    }

    #[tokio::test]
    async fn successful_fir_transfer_keeps_snapshot_trust_when_only_the_master_unmute_fails() {
        let client = scripted_client(Box::new(ScriptTransport {
            responses: Arc::new(Mutex::new(std::collections::VecDeque::from([
                vec![commands::CMD_READ_FLASH, 0, 0, 0, 0],
                vec![commands::CMD_FIR_LOAD_START, 0, 32],
                vec![commands::RESP_OK],
                vec![commands::RESP_OK],
                vec![commands::RESP_OK],
                vec![commands::CMD_READ_FLASH, 0, 0, 0, 1],
                vec![commands::RESP_ERROR, 1],
            ]))),
            sent: Arc::new(Mutex::new(Vec::new())),
            disconnect_data_once: None,
        }));
        let fir = client
            .profile
            .dsp_table
            .unwrap()
            .outputs
            .iter()
            .find_map(|output| output.fir)
            .unwrap();
        *client.active_preset.lock().await = Some(0);
        client
            .store_preset_shadow_snapshot(
                0,
                PresetShadowState {
                    selector: HashMap::from([(fir.taps_count, 1)]),
                    fir_blocks: HashMap::from([(fir.data_start, vec![0.5])]),
                    ..PresetShadowState::default()
                },
            )
            .await;
        client.mark_snapshot_trusted(0).await;
        let err = client
            .load_fir_coefficients(fir, &[0.0, 1.0])
            .await
            .unwrap_err();
        assert!(matches!(err, ClientError::Protocol(message) if message.contains("set mute")));
        assert_eq!(client.selector_shadow.lock().await[&fir.taps_count], 2);
        assert_eq!(
            client.fir_shadow.lock().await[&fir.data_start],
            vec![0.0, 1.0]
        );
        client.update_active_preset(1).await;
        client.update_active_preset(0).await;
        let state = client.export_shadow_state().await;
        // Every FIR write was acknowledged; master mute is not preset content.
        assert!(state.trusted_snapshot_presets.contains(&0));
        assert!(
            !client
                .invalidated_snapshot_presets
                .lock()
                .await
                .contains(&0)
        );
        assert_eq!(state.selector[&fir.taps_count], 2);
        assert_eq!(
            state.presets[&0].fir_blocks[&fir.data_start],
            vec![0.0, 1.0]
        );
    }

    #[tokio::test]
    async fn fir_data_and_end_errors_clean_up_and_restore_mute() {
        for fail_at_end in [false, true] {
            let mut responses = vec![
                vec![commands::CMD_READ_FLASH, 0, 0, 0, 0],
                vec![commands::CMD_FIR_LOAD_START, 0, 32],
                vec![commands::RESP_OK],
            ];
            if fail_at_end {
                responses.push(vec![commands::RESP_OK]);
            }
            responses.extend([
                vec![commands::RESP_ERROR, 1],
                vec![commands::RESP_OK],
                vec![commands::CMD_READ_FLASH, 0, 0, 0, 1],
                vec![commands::RESP_OK],
                vec![commands::CMD_READ_FLASH, 0, 0, 0, 0],
            ]);
            let sent = Arc::new(Mutex::new(Vec::new()));
            let client = scripted_client(Box::new(ScriptTransport {
                responses: Arc::new(Mutex::new(responses.into())),
                sent: sent.clone(),
                disconnect_data_once: None,
            }));
            let fir = client
                .profile
                .dsp_table
                .unwrap()
                .outputs
                .iter()
                .find_map(|output| output.fir)
                .unwrap();
            assert!(matches!(
                client.load_fir_coefficients(fir, &[1.0]).await,
                Err(ClientError::Protocol(_))
            ));
            let sent = sent.lock().await;
            assert_eq!(
                sent.iter()
                    .filter(|payload| *payload == &commands::fir_load_end())
                    .count(),
                if fail_at_end { 2 } else { 1 }
            );
            assert!(
                sent.iter()
                    .any(|payload| payload == &commands::set_mute(false))
            );
            assert!(!client.fir_shadow.lock().await.contains_key(&fir.data_start));
        }
    }

    #[tokio::test]
    async fn fir_runtime_capacity_failure_ends_transfer_and_restores_mute() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let transport = ScriptTransport {
            disconnect_data_once: None,
            responses: Arc::new(Mutex::new(std::collections::VecDeque::from([
                vec![commands::CMD_READ_FLASH, 0, 0, 0, 0],
                vec![commands::CMD_FIR_LOAD_START, 0, 1],
                vec![commands::RESP_OK],
                vec![commands::CMD_READ_FLASH, 0, 0, 0, 1],
                vec![commands::RESP_OK],
                vec![commands::CMD_READ_FLASH, 0, 0, 0, 0],
            ]))),
            sent: sent.clone(),
        };
        let client = scripted_client(Box::new(transport));
        let fir = client
            .profile
            .dsp_table
            .unwrap()
            .outputs
            .iter()
            .find_map(|output| output.fir)
            .unwrap();
        assert!(
            client
                .load_fir_coefficients(fir, &[0.0, 1.0])
                .await
                .is_err()
        );
        let sent = sent.lock().await;
        assert!(
            sent.iter()
                .any(|payload| payload == &commands::fir_load_end())
        );
        assert!(
            sent.iter()
                .any(|payload| payload == &commands::set_mute(false))
        );
    }

    #[tokio::test]
    async fn trusted_peq_edits_survive_switch_away_and_back() {
        let client = scripted_client(Box::new(ScriptTransport {
            responses: Arc::new(Mutex::new(std::collections::VecDeque::from([
                vec![commands::RESP_OK],
                vec![commands::RESP_OK],
            ]))),
            sent: Arc::new(Mutex::new(Vec::new())),
            disconnect_data_once: None,
        }));
        let original = PresetShadowState {
            peq: HashMap::from([(
                8480,
                CachedPeqShadow {
                    coeffs: [1.0; 5],
                    bypass: false,
                },
            )]),
            ..PresetShadowState::default()
        };
        client.store_preset_shadow_snapshot(0, original).await;
        client.mark_snapshot_trusted(0).await;
        client.write_biquad(8480, &[2.0; 5]).await.unwrap();
        client.bypass_filter(8480, true).await.unwrap();
        client.update_active_preset(1).await;
        client.update_active_preset(0).await;
        let exported = client.export_shadow_state().await;
        assert_eq!(exported.peq[&8480].coeffs, [2.0; 5]);
        assert!(exported.peq[&8480].bypass);
    }

    #[tokio::test]
    async fn mirror_raw_biquad_write_updates_shadow() {
        let peq_shadow = Arc::new(Mutex::new(HashMap::from([(
            8480u16,
            PeqShadow {
                coeffs: minidsp_protocol::biquad::BYPASS,
                bypass: false,
            },
        )])));
        let selector_shadow = Arc::new(Mutex::new(HashMap::new()));
        let scalar_shadow = Arc::new(Mutex::new(HashMap::new()));
        let coeffs = [0.1, 0.2, 0.3, 0.4, 0.5];
        let payload = commands::write_biquad(8480, &coeffs, &Dialect::NEW);

        mirror_raw_command_to_shadow(
            &payload,
            &Dialect::NEW,
            &peq_shadow,
            &scalar_shadow,
            &selector_shadow,
            |_| false,
        )
        .await;

        let shadow = peq_shadow.lock().await;
        assert_eq!(shadow.get(&8480).unwrap().coeffs, coeffs);
    }

    #[tokio::test]
    async fn mirror_raw_bypass_write_updates_shadow() {
        let peq_shadow = Arc::new(Mutex::new(HashMap::from([(
            8480u16,
            PeqShadow {
                coeffs: minidsp_protocol::biquad::BYPASS,
                bypass: false,
            },
        )])));
        let selector_shadow = Arc::new(Mutex::new(HashMap::new()));
        let scalar_shadow = Arc::new(Mutex::new(HashMap::new()));
        let payload = commands::bypass_filter(8480, true, &Dialect::NEW);

        mirror_raw_command_to_shadow(
            &payload,
            &Dialect::NEW,
            &peq_shadow,
            &scalar_shadow,
            &selector_shadow,
            |_| false,
        )
        .await;

        let shadow = peq_shadow.lock().await;
        assert!(shadow.get(&8480).unwrap().bypass);
    }

    #[tokio::test]
    async fn apply_peq_shadow_bypass_updates_all_bands() {
        let peq_shadow = Arc::new(Mutex::new(HashMap::from([
            (
                8480u16,
                PeqShadow {
                    coeffs: minidsp_protocol::biquad::BYPASS,
                    bypass: false,
                },
            ),
            (
                8485u16,
                PeqShadow {
                    coeffs: minidsp_protocol::biquad::BYPASS,
                    bypass: false,
                },
            ),
        ])));
        let bands = vec![
            (8480u16, minidsp_protocol::biquad::BYPASS, false),
            (8485u16, minidsp_protocol::biquad::BYPASS, false),
        ];

        apply_peq_shadow_bypass(&peq_shadow, &bands, true).await;

        let shadow = peq_shadow.lock().await;
        assert!(shadow.get(&8480).unwrap().bypass);
        assert!(shadow.get(&8485).unwrap().bypass);
    }

    #[test]
    fn measurement_restore_batches_skip_unchanged_coefficients() {
        let current_shadow = HashMap::from([
            (
                0x2000u16,
                PeqShadow {
                    coeffs: minidsp_protocol::biquad::BYPASS,
                    bypass: true,
                },
            ),
            (
                0x2005u16,
                PeqShadow {
                    coeffs: minidsp_protocol::biquad::BYPASS,
                    bypass: true,
                },
            ),
        ]);
        let saved = vec![
            (0x2000u16, minidsp_protocol::biquad::BYPASS, false),
            (0x2005u16, minidsp_protocol::biquad::BYPASS, true),
        ];

        let (phase1, phase2, phase3) =
            build_measurement_restore_batches(&current_shadow, &saved, &Dialect::NEW);

        assert!(phase1.is_empty());
        assert!(phase2.is_empty());
        assert_eq!(
            phase3,
            vec![commands::bypass_filter(0x2000, false, &Dialect::NEW)]
        );
    }

    #[test]
    fn measurement_restore_batches_rewrite_changed_coefficients() {
        let current_shadow = HashMap::from([(
            0x2000u16,
            PeqShadow {
                coeffs: minidsp_protocol::biquad::BYPASS,
                bypass: false,
            },
        )]);
        let coeffs = [1.0, 0.5, 0.25, -0.5, 0.125];
        let saved = vec![(0x2000u16, coeffs, false)];

        let (phase1, phase2, phase3) =
            build_measurement_restore_batches(&current_shadow, &saved, &Dialect::NEW);

        assert_eq!(
            phase1,
            vec![commands::bypass_filter(0x2000, true, &Dialect::NEW)]
        );
        assert_eq!(
            phase2,
            vec![commands::write_biquad(0x2000, &coeffs, &Dialect::NEW)]
        );
        assert_eq!(
            phase3,
            vec![commands::bypass_filter(0x2000, false, &Dialect::NEW)]
        );
    }

    #[tokio::test]
    async fn mirror_raw_selector_write_updates_shadow() {
        let peq_shadow = Arc::new(Mutex::new(HashMap::new()));
        let selector_shadow = Arc::new(Mutex::new(HashMap::new()));
        let scalar_shadow = Arc::new(Mutex::new(HashMap::new()));
        let payload = commands::write_dsp_int(24, 3);

        mirror_raw_command_to_shadow(
            &payload,
            &Dialect::NEW,
            &peq_shadow,
            &scalar_shadow,
            &selector_shadow,
            |_| false,
        )
        .await;

        let shadow = selector_shadow.lock().await;
        assert_eq!(shadow.get(&24), Some(&3));
    }

    #[tokio::test]
    async fn mirror_raw_linked_selector_write_updates_shadow() {
        let peq_shadow = Arc::new(Mutex::new(HashMap::new()));
        let selector_shadow = Arc::new(Mutex::new(HashMap::new()));
        let scalar_shadow = Arc::new(Mutex::new(HashMap::new()));
        let payload = commands::write_dsp_linked_u32(22, 2, &[]);

        mirror_raw_command_to_shadow(
            &payload,
            &Dialect::NEW,
            &peq_shadow,
            &scalar_shadow,
            &selector_shadow,
            |_| false,
        )
        .await;

        let shadow = selector_shadow.lock().await;
        assert_eq!(shadow.get(&22), Some(&2));
    }

    #[tokio::test]
    async fn mirror_raw_scalar_write_updates_shadow() {
        let peq_shadow = Arc::new(Mutex::new(HashMap::new()));
        let selector_shadow = Arc::new(Mutex::new(HashMap::new()));
        let scalar_shadow = Arc::new(Mutex::new(HashMap::new()));
        let payload = commands::write_dsp(42, 9.9, true, &Dialect::NEW);

        mirror_raw_command_to_shadow(
            &payload,
            &Dialect::NEW,
            &peq_shadow,
            &scalar_shadow,
            &selector_shadow,
            |_| false,
        )
        .await;

        let shadow = scalar_shadow.lock().await;
        assert_eq!(shadow.get(&42), Some(&9.9f32.to_bits()));
    }

    #[tokio::test]
    async fn mirror_raw_zero_float_write_stays_scalar_not_selector() {
        let peq_shadow = Arc::new(Mutex::new(HashMap::new()));
        let selector_shadow = Arc::new(Mutex::new(HashMap::new()));
        let scalar_shadow = Arc::new(Mutex::new(HashMap::new()));
        let payload = commands::write_dsp(28, 0.0, false, &Dialect::NEW);

        mirror_raw_command_to_shadow(
            &payload,
            &Dialect::NEW,
            &peq_shadow,
            &scalar_shadow,
            &selector_shadow,
            |_| false,
        )
        .await;

        assert_eq!(scalar_shadow.lock().await.get(&28), Some(&0.0f32.to_bits()));
        assert!(selector_shadow.lock().await.get(&28).is_none());
    }

    #[tokio::test]
    async fn mirror_raw_persisted_zero_float_write_can_prefer_scalar_shadow() {
        let peq_shadow = Arc::new(Mutex::new(HashMap::new()));
        let selector_shadow = Arc::new(Mutex::new(HashMap::new()));
        let scalar_shadow = Arc::new(Mutex::new(HashMap::new()));
        let payload = commands::write_dsp(28, 0.0, true, &Dialect::NEW);

        mirror_raw_command_to_shadow(
            &payload,
            &Dialect::NEW,
            &peq_shadow,
            &scalar_shadow,
            &selector_shadow,
            |addr| addr == 28,
        )
        .await;

        assert_eq!(scalar_shadow.lock().await.get(&28), Some(&0.0f32.to_bits()));
        assert!(selector_shadow.lock().await.get(&28).is_none());
    }

    #[tokio::test]
    async fn mirror_raw_persisted_delay_write_can_prefer_scalar_shadow() {
        let peq_shadow = Arc::new(Mutex::new(HashMap::new()));
        let selector_shadow = Arc::new(Mutex::new(HashMap::new()));
        let scalar_shadow = Arc::new(Mutex::new(HashMap::new()));
        let payload = commands::write_dsp_delay(66, 60, true, &Dialect::NEW);

        mirror_raw_command_to_shadow(
            &payload,
            &Dialect::NEW,
            &peq_shadow,
            &scalar_shadow,
            &selector_shadow,
            |addr| addr == 66,
        )
        .await;

        assert_eq!(scalar_shadow.lock().await.get(&66), Some(&60));
        assert!(selector_shadow.lock().await.get(&66).is_none());
    }

    #[tokio::test]
    async fn cached_shadow_state_roundtrips() {
        let client = MiniDspClient {
            queue: Arc::new(CommandQueue::new(Box::new(DummyTransport))),
            transport_url: "mock://shadow".into(),
            profile: probe::identify(10, 100),
            hw_id: HardwareId {
                fw_major: 1,
                fw_minor: 0,
                hw_id: 10,
            },
            dsp_version: 100,
            serial: 912345,
            saved_peq: Mutex::new(None),
            volume_settings: Default::default(),
            pending_write: Arc::new(Mutex::new(None)),
            peq_shadow: Arc::new(Mutex::new(HashMap::new())),
            peq_readback_known: Mutex::new(HashMap::new()),
            scalar_readback_known: Mutex::new(HashSet::new()),
            selector_readback_known: Mutex::new(HashSet::new()),
            invalidated_snapshot_presets: Mutex::new(HashSet::new()),
            readback_epoch: Mutex::new(0),
            selector_shadow: Arc::new(Mutex::new(HashMap::new())),
            scalar_shadow: Arc::new(Mutex::new(HashMap::new())),
            fir_shadow: Arc::new(Mutex::new(HashMap::new())),
            preset_shadows: Arc::new(Mutex::new(HashMap::new())),
            preset_tokens: Arc::new(Mutex::new(HashMap::new())),
            trusted_snapshot_presets: Arc::new(Mutex::new(HashSet::new())),
            skip_trusted_shadow_capture_once: Arc::new(Mutex::new(HashSet::new())),
            active_preset: Mutex::new(Some(0)),
            state_transition_lock: Mutex::new(()),
        };

        let state = DeviceShadowState {
            presets: HashMap::from([(
                0,
                PresetShadowState {
                    peq: HashMap::from([(
                        8480,
                        CachedPeqShadow {
                            coeffs: [0.1, 0.2, 0.3, 0.4, 0.5],
                            bypass: true,
                        },
                    )]),
                    selector: HashMap::from([(8, 1), (4182, 6)]),
                    scalar: HashMap::from([(42, 9.9f32.to_bits())]),
                    fir_blocks: HashMap::new(),
                },
            )]),
            peq: HashMap::from([(
                8480,
                CachedPeqShadow {
                    coeffs: [0.1, 0.2, 0.3, 0.4, 0.5],
                    bypass: true,
                },
            )]),
            selector: HashMap::from([(8, 1), (4182, 6)]),
            scalar: HashMap::from([(42, 9.9f32.to_bits())]),
            preset_tokens: HashMap::from([(0, [0x80, 0x97, 0x81, 0xc9])]),
            trusted_snapshot_presets: HashSet::new(),
        };

        client.apply_shadow_state(&state).await;
        let exported = client.export_shadow_state().await;

        assert_eq!(
            exported.peq.get(&8480).unwrap().coeffs,
            [0.1, 0.2, 0.3, 0.4, 0.5]
        );
        assert!(exported.peq.get(&8480).unwrap().bypass);
        assert_eq!(exported.selector.get(&8), Some(&1));
        assert_eq!(exported.selector.get(&4182), Some(&6));
        assert_eq!(exported.scalar.get(&42), Some(&9.9f32.to_bits()));
        assert_eq!(
            exported.preset_tokens.get(&0),
            Some(&[0x80, 0x97, 0x81, 0xc9])
        );
        assert_eq!(
            exported
                .presets
                .get(&0)
                .and_then(|preset| preset.scalar.get(&42)),
            Some(&9.9f32.to_bits())
        );
    }

    #[tokio::test]
    async fn preset_shadow_switching_keeps_non_authoritative_values_scoped() {
        let client = MiniDspClient {
            queue: Arc::new(CommandQueue::new(Box::new(DummyTransport))),
            transport_url: "mock://shadow".into(),
            profile: probe::identify(10, 100),
            hw_id: HardwareId {
                fw_major: 1,
                fw_minor: 0,
                hw_id: 10,
            },
            dsp_version: 100,
            serial: 912345,
            saved_peq: Mutex::new(None),
            volume_settings: Default::default(),
            pending_write: Arc::new(Mutex::new(None)),
            peq_shadow: Arc::new(Mutex::new(build_peq_shadow(&probe::identify(10, 100)))),
            peq_readback_known: Mutex::new(HashMap::new()),
            scalar_readback_known: Mutex::new(HashSet::new()),
            selector_readback_known: Mutex::new(HashSet::new()),
            invalidated_snapshot_presets: Mutex::new(HashSet::new()),
            readback_epoch: Mutex::new(0),
            selector_shadow: Arc::new(Mutex::new(HashMap::new())),
            scalar_shadow: Arc::new(Mutex::new(HashMap::new())),
            fir_shadow: Arc::new(Mutex::new(HashMap::new())),
            preset_shadows: Arc::new(Mutex::new(HashMap::new())),
            preset_tokens: Arc::new(Mutex::new(HashMap::new())),
            trusted_snapshot_presets: Arc::new(Mutex::new(HashSet::new())),
            skip_trusted_shadow_capture_once: Arc::new(Mutex::new(HashSet::new())),
            active_preset: Mutex::new(Some(0)),
            state_transition_lock: Mutex::new(()),
        };

        client
            .scalar_shadow
            .lock()
            .await
            .insert(42, 9.5f32.to_bits());
        client
            .scalar_shadow
            .lock()
            .await
            .insert(44, 99.0f32.to_bits());
        client
            .scalar_shadow
            .lock()
            .await
            .insert(45, 999.0f32.to_bits());
        client.selector_shadow.lock().await.insert(6, 1);

        client.update_active_preset(1).await;

        assert_eq!(client.scalar_shadow.lock().await.get(&42), None);
        assert_eq!(client.scalar_shadow.lock().await.get(&44), None);
        assert_eq!(client.scalar_shadow.lock().await.get(&45), None);
        assert_eq!(client.selector_shadow.lock().await.get(&6), None);

        client
            .scalar_shadow
            .lock()
            .await
            .insert(42, 3.5f32.to_bits());
        client
            .scalar_shadow
            .lock()
            .await
            .insert(44, 12.0f32.to_bits());
        client
            .scalar_shadow
            .lock()
            .await
            .insert(45, 240.0f32.to_bits());
        client.selector_shadow.lock().await.insert(6, 2);

        client.update_active_preset(0).await;

        let scalar_shadow = client.scalar_shadow.lock().await;
        assert_eq!(scalar_shadow.get(&42), Some(&9.5f32.to_bits()));
        assert_eq!(scalar_shadow.get(&44), Some(&99.0f32.to_bits()));
        assert_eq!(scalar_shadow.get(&45), Some(&999.0f32.to_bits()));
        drop(scalar_shadow);
        assert_eq!(client.selector_shadow.lock().await.get(&6), Some(&1));
    }

    #[tokio::test]
    async fn trusted_snapshot_preserves_exact_restore_on_next_switch_only() {
        let client = MiniDspClient {
            queue: Arc::new(CommandQueue::new(Box::new(DummyTransport))),
            transport_url: "mock://trusted-snapshot".into(),
            profile: probe::identify(10, 100),
            hw_id: HardwareId {
                fw_major: 1,
                fw_minor: 0,
                hw_id: 10,
            },
            dsp_version: 100,
            serial: 912345,
            saved_peq: Mutex::new(None),
            volume_settings: Default::default(),
            pending_write: Arc::new(Mutex::new(None)),
            peq_shadow: Arc::new(Mutex::new(HashMap::new())),
            peq_readback_known: Mutex::new(HashMap::new()),
            scalar_readback_known: Mutex::new(HashSet::new()),
            selector_readback_known: Mutex::new(HashSet::new()),
            invalidated_snapshot_presets: Mutex::new(HashSet::new()),
            readback_epoch: Mutex::new(0),
            selector_shadow: Arc::new(Mutex::new(HashMap::new())),
            scalar_shadow: Arc::new(Mutex::new(HashMap::new())),
            fir_shadow: Arc::new(Mutex::new(HashMap::new())),
            preset_shadows: Arc::new(Mutex::new(HashMap::new())),
            preset_tokens: Arc::new(Mutex::new(HashMap::new())),
            trusted_snapshot_presets: Arc::new(Mutex::new(HashSet::new())),
            skip_trusted_shadow_capture_once: Arc::new(Mutex::new(HashSet::new())),
            active_preset: Mutex::new(Some(0)),
            state_transition_lock: Mutex::new(()),
        };

        let exact_snapshot = PresetShadowState {
            selector: HashMap::from([(6, 1)]),
            scalar: HashMap::from([(42, 9.5f32.to_bits())]),
            ..PresetShadowState::default()
        };
        client
            .store_preset_shadow_snapshot(0, exact_snapshot.clone())
            .await;
        client.mark_snapshot_trusted(0).await;

        client
            .scalar_shadow
            .lock()
            .await
            .insert(42, 3.5f32.to_bits());
        client.selector_shadow.lock().await.insert(6, 2);

        client.update_active_preset(1).await;
        assert_eq!(
            client.preset_shadows.lock().await.get(&0),
            Some(&exact_snapshot)
        );

        client.update_active_preset(0).await;
        client
            .scalar_shadow
            .lock()
            .await
            .insert(42, 4.5f32.to_bits());
        client.selector_shadow.lock().await.insert(6, 3);

        client.update_active_preset(1).await;
        let stored = client
            .preset_shadows
            .lock()
            .await
            .get(&0)
            .cloned()
            .expect("preset 0 snapshot should still exist");
        assert_eq!(stored.scalar.get(&42), Some(&4.5f32.to_bits()));
        assert_eq!(stored.selector.get(&6), Some(&3));
    }

    #[test]
    fn prefers_scalar_shadow_for_non_authoritative_dsp100_scalar_fields() {
        let client = MiniDspClient {
            queue: Arc::new(CommandQueue::new(Box::new(DummyTransport))),
            transport_url: "mock://shadow".into(),
            profile: probe::identify(10, 100),
            hw_id: HardwareId {
                fw_major: 1,
                fw_minor: 0,
                hw_id: 10,
            },
            dsp_version: 100,
            serial: 912345,
            saved_peq: Mutex::new(None),
            volume_settings: Default::default(),
            pending_write: Arc::new(Mutex::new(None)),
            peq_shadow: Arc::new(Mutex::new(HashMap::new())),
            peq_readback_known: Mutex::new(HashMap::new()),
            scalar_readback_known: Mutex::new(HashSet::new()),
            selector_readback_known: Mutex::new(HashSet::new()),
            invalidated_snapshot_presets: Mutex::new(HashSet::new()),
            readback_epoch: Mutex::new(0),
            selector_shadow: Arc::new(Mutex::new(HashMap::new())),
            scalar_shadow: Arc::new(Mutex::new(HashMap::new())),
            fir_shadow: Arc::new(Mutex::new(HashMap::new())),
            preset_shadows: Arc::new(Mutex::new(HashMap::new())),
            preset_tokens: Arc::new(Mutex::new(HashMap::new())),
            trusted_snapshot_presets: Arc::new(Mutex::new(HashSet::new())),
            skip_trusted_shadow_capture_once: Arc::new(Mutex::new(HashSet::new())),
            active_preset: Mutex::new(Some(0)),
            state_transition_lock: Mutex::new(()),
        };

        let table = client.profile.dsp_table.unwrap();
        assert!(
            client.prefers_scalar_shadow_for_addr(table.inputs[0].gain.expect("input gain addr"))
        );
        assert!(client.prefers_scalar_shadow_for_addr(table.outputs[0].gain));
        assert!(
            client
                .prefers_scalar_shadow_for_addr(table.outputs[0].delay.expect("output delay addr"))
        );
        assert!(client.prefers_scalar_shadow_for_addr(table.routing.gain_base));
        assert!(client.prefers_scalar_shadow_for_addr(40));
        assert!(client.prefers_scalar_shadow_for_addr(41));
        assert!(client.prefers_scalar_shadow_for_addr(42));
        assert!(client.prefers_scalar_shadow_for_addr(43));
        assert!(client.prefers_scalar_shadow_for_addr(44));
        assert!(client.prefers_scalar_shadow_for_addr(45));
    }

    #[tokio::test]
    async fn concurrent_preset_switches_are_serialized() {
        let transport = DelayedPresetTransport::new(3);
        let current_preset = transport.current_preset.clone();
        let set_config_targets = transport.set_config_targets.clone();
        let client = MiniDspClient {
            queue: Arc::new(CommandQueue::new(Box::new(transport))),
            transport_url: "mock://preset-race".into(),
            profile: probe::identify(10, 100),
            hw_id: HardwareId {
                fw_major: 1,
                fw_minor: 0,
                hw_id: 10,
            },
            dsp_version: 100,
            serial: 912345,
            saved_peq: Mutex::new(None),
            volume_settings: Default::default(),
            pending_write: Arc::new(Mutex::new(None)),
            peq_shadow: Arc::new(Mutex::new(build_peq_shadow(&probe::identify(10, 100)))),
            peq_readback_known: Mutex::new(HashMap::new()),
            scalar_readback_known: Mutex::new(HashSet::new()),
            selector_readback_known: Mutex::new(HashSet::new()),
            invalidated_snapshot_presets: Mutex::new(HashSet::new()),
            readback_epoch: Mutex::new(0),
            selector_shadow: Arc::new(Mutex::new(HashMap::new())),
            scalar_shadow: Arc::new(Mutex::new(HashMap::new())),
            fir_shadow: Arc::new(Mutex::new(HashMap::new())),
            preset_shadows: Arc::new(Mutex::new(HashMap::new())),
            preset_tokens: Arc::new(Mutex::new(HashMap::new())),
            trusted_snapshot_presets: Arc::new(Mutex::new(HashSet::new())),
            skip_trusted_shadow_capture_once: Arc::new(Mutex::new(HashSet::new())),
            active_preset: Mutex::new(Some(3)),
            state_transition_lock: Mutex::new(()),
        };

        let (first, second) = tokio::join!(async { client.set_config(2).await }, async {
            tokio::time::sleep(Duration::from_millis(5)).await;
            client.set_config(3).await
        });

        first.expect("first preset switch should succeed");
        second.expect("second preset switch should succeed");

        assert_eq!(current_preset.load(Ordering::SeqCst), 3);
        assert_eq!(set_config_targets.lock().await.as_slice(), &[2, 3]);
    }

    #[tokio::test]
    async fn set_config_succeeds_when_transport_times_out_but_status_verifies() {
        let transport = TimeoutingPresetTransport::new(0);
        let current_preset = transport.current_preset.clone();
        let client = Arc::new(MiniDspClient {
            queue: Arc::new(CommandQueue::new(Box::new(transport))),
            transport_url: "mock://preset-timeout".into(),
            profile: probe::identify(10, 100),
            hw_id: HardwareId {
                fw_major: 1,
                fw_minor: 0,
                hw_id: 10,
            },
            dsp_version: 100,
            serial: 912345,
            saved_peq: Mutex::new(None),
            volume_settings: Default::default(),
            pending_write: Arc::new(Mutex::new(None)),
            peq_shadow: Arc::new(Mutex::new(build_peq_shadow(&probe::identify(10, 100)))),
            peq_readback_known: Mutex::new(HashMap::new()),
            scalar_readback_known: Mutex::new(HashSet::new()),
            selector_readback_known: Mutex::new(HashSet::new()),
            invalidated_snapshot_presets: Mutex::new(HashSet::new()),
            readback_epoch: Mutex::new(0),
            selector_shadow: Arc::new(Mutex::new(HashMap::new())),
            scalar_shadow: Arc::new(Mutex::new(HashMap::new())),
            fir_shadow: Arc::new(Mutex::new(HashMap::new())),
            preset_shadows: Arc::new(Mutex::new(HashMap::new())),
            preset_tokens: Arc::new(Mutex::new(HashMap::new())),
            trusted_snapshot_presets: Arc::new(Mutex::new(HashSet::new())),
            skip_trusted_shadow_capture_once: Arc::new(Mutex::new(HashSet::new())),
            active_preset: Mutex::new(Some(0)),
            state_transition_lock: Mutex::new(()),
        });

        let status = client
            .set_config(2)
            .await
            .expect("master-status verification should recover from timeout");
        assert_eq!(status.preset, 2);
        assert_eq!(current_preset.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn wait_for_reset_recovery_rejects_single_status_blip() {
        let mut responses = std::collections::VecDeque::from([vec![commands::RESP_ERROR, 1]]);
        responses.extend((0..16).map(|_| vec![commands::CMD_READ_FLASH, 0, 0, 0, 0]));
        let client = scripted_client(Box::new(ScriptTransport {
            responses: Arc::new(Mutex::new(responses)),
            disconnect_data_once: None,
            sent: Arc::new(Mutex::new(Vec::new())),
        }));

        let err = client
            .wait_for_reset_recovery_until(
                Duration::from_millis(700),
                client.queue.reconnect_epoch(),
            )
            .await
            .expect_err("single transient read failure should not verify reset");
        assert!(matches!(err, ClientError::Protocol(_)));
    }

    #[tokio::test]
    async fn wait_for_reset_recovery_accepts_timeout_reconnect() {
        let client = scripted_client(Box::new(RecoveringResetTransport::new(1)));
        let initial_epoch = client.queue.disconnect_epoch();
        client
            .wait_for_reset_recovery_until(Duration::from_millis(700), initial_epoch)
            .await
            .expect(
                "timeout recovery must record an outage before the first successful status read",
            );
        assert!(client.queue.disconnect_epoch() > initial_epoch);
    }

    #[tokio::test]
    async fn reboot_and_reset_recover_after_hid_error_and_failed_reopen() {
        for reboot in [false, true] {
            let transport = RebootCommandDropTransport::new();
            transport
                .reopen_failures_remaining
                .store(1, Ordering::SeqCst);
            let client = scripted_client(Box::new(transport));
            let result = tokio::time::timeout(Duration::from_secs(2), async {
                if reboot {
                    client.reboot_device().await
                } else {
                    client.reset_with_verification().await
                }
            })
            .await
            .expect("recovery should finish on the first successful status read");
            result.unwrap();
            assert!(client.queue.disconnect_epoch() > 0);
        }
    }

    #[tokio::test]
    async fn reset_succeeds_when_transport_times_out_but_status_recovers() {
        let client = MiniDspClient {
            queue: Arc::new(CommandQueue::new(Box::new(RecoveringResetTransport::new(
                2,
            )))),
            transport_url: "mock://reset-timeout".into(),
            profile: probe::identify(10, 100),
            hw_id: HardwareId {
                fw_major: 1,
                fw_minor: 0,
                hw_id: 10,
            },
            dsp_version: 100,
            serial: 912345,
            saved_peq: Mutex::new(None),
            volume_settings: Default::default(),
            pending_write: Arc::new(Mutex::new(None)),
            peq_shadow: Arc::new(Mutex::new(build_peq_shadow(&probe::identify(10, 100)))),
            peq_readback_known: Mutex::new(HashMap::new()),
            scalar_readback_known: Mutex::new(HashSet::new()),
            selector_readback_known: Mutex::new(HashSet::new()),
            invalidated_snapshot_presets: Mutex::new(HashSet::new()),
            readback_epoch: Mutex::new(0),
            selector_shadow: Arc::new(Mutex::new(HashMap::new())),
            scalar_shadow: Arc::new(Mutex::new(HashMap::new())),
            fir_shadow: Arc::new(Mutex::new(HashMap::new())),
            preset_shadows: Arc::new(Mutex::new(HashMap::new())),
            preset_tokens: Arc::new(Mutex::new(HashMap::new())),
            trusted_snapshot_presets: Arc::new(Mutex::new(HashSet::new())),
            skip_trusted_shadow_capture_once: Arc::new(Mutex::new(HashSet::new())),
            active_preset: Mutex::new(Some(0)),
            state_transition_lock: Mutex::new(()),
        };

        client
            .reset_with_verification()
            .await
            .expect("reset recovery verification should accept sustained outage");
    }

    #[tokio::test]
    async fn reboot_succeeds_when_command_stage_hid_drop_is_followed_by_reconnect() {
        let client = Arc::new(MiniDspClient {
            queue: Arc::new(CommandQueue::new(Box::new(
                RebootCommandDropTransport::new(),
            ))),
            transport_url: "mock://reboot-command-drop".into(),
            profile: probe::identify(10, 100),
            hw_id: HardwareId {
                fw_major: 1,
                fw_minor: 0,
                hw_id: 10,
            },
            dsp_version: 100,
            serial: 912345,
            saved_peq: Mutex::new(None),
            volume_settings: Default::default(),
            pending_write: Arc::new(Mutex::new(None)),
            peq_shadow: Arc::new(Mutex::new(build_peq_shadow(&probe::identify(10, 100)))),
            peq_readback_known: Mutex::new(HashMap::new()),
            scalar_readback_known: Mutex::new(HashSet::new()),
            selector_readback_known: Mutex::new(HashSet::new()),
            invalidated_snapshot_presets: Mutex::new(HashSet::new()),
            readback_epoch: Mutex::new(0),
            selector_shadow: Arc::new(Mutex::new(HashMap::new())),
            scalar_shadow: Arc::new(Mutex::new(HashMap::new())),
            fir_shadow: Arc::new(Mutex::new(HashMap::new())),
            preset_shadows: Arc::new(Mutex::new(HashMap::new())),
            preset_tokens: Arc::new(Mutex::new(HashMap::new())),
            trusted_snapshot_presets: Arc::new(Mutex::new(HashSet::new())),
            skip_trusted_shadow_capture_once: Arc::new(Mutex::new(HashSet::new())),
            active_preset: Mutex::new(Some(0)),
            state_transition_lock: Mutex::new(()),
        });

        client.reboot_device().await.expect(
            "reboot recovery should accept a command-stage HID drop once the device recovers",
        );
    }

    #[test]
    fn sanitize_meter_values_clamps_invalid_and_tiny_values() {
        let values = sanitize_meter_values(&[f32::NAN, -200.0, 0.0, 1e-20, -87.5, 3.2]);
        assert_eq!(
            values,
            vec![
                SILENT_METER_DB,
                SILENT_METER_DB,
                SILENT_METER_DB,
                SILENT_METER_DB,
                -87.5,
                3.2,
            ]
        );
    }

    #[test]
    fn sanitize_shadow_preferred_scalar_clamps_placeholder_and_absurd_values() {
        assert_eq!(sanitize_shadow_preferred_scalar(0), Some(0.0));
        assert_eq!(sanitize_shadow_preferred_scalar(2), Some(0.0));
        assert_eq!(sanitize_shadow_preferred_scalar(0x4A03_9901), Some(0.0));
        assert_eq!(sanitize_shadow_preferred_scalar(9.5f32.to_bits()), None);
    }
    #[tokio::test]
    async fn config_readback_distinguishes_seeded_peq_defaults_from_acknowledged_writes() {
        let client = scripted_client(Box::new(ScriptTransport {
            responses: Arc::new(Mutex::new(std::collections::VecDeque::from([
                vec![commands::RESP_OK],
                vec![commands::RESP_OK],
            ]))),
            disconnect_data_once: None,
            sent: Arc::new(Mutex::new(Vec::new())),
        }));
        *client.peq_shadow.lock().await = build_peq_shadow(client.profile());
        let mut before = ConfigReadback::default();
        assert_eq!(
            client
                .config_peq_readback(8480, &mut before, "inputs.0.peq.0.coeff".into())
                .await,
            minidsp_protocol::biquad::BYPASS
        );
        assert!(
            !client
                .config_peq_bypass_readback(8480, &mut before, "inputs.0.peq.0.bypass".into())
                .await
        );
        assert!(
            before
                .fields
                .iter()
                .all(|field| field.source == ConfigReadbackSource::Default)
        );
        let coeffs = [1.0, 0.1, 0.0, 0.2, 0.0];
        client.write_biquad(8480, &coeffs).await.unwrap();
        client.bypass_filter(8480, true).await.unwrap();
        let mut after = ConfigReadback::default();
        assert_eq!(
            client
                .config_peq_readback(8480, &mut after, "inputs.0.peq.0.coeff".into())
                .await,
            coeffs
        );
        assert!(
            client
                .config_peq_bypass_readback(8480, &mut after, "inputs.0.peq.0.bypass".into())
                .await
        );
        assert!(after.complete);
        assert!(
            after
                .fields
                .iter()
                .all(|field| field.source == ConfigReadbackSource::Shadow)
        );
        // Old persisted data has no provenance, even when its values match.
        let saved = client.export_shadow_state().await;
        client.apply_shadow_state(&saved).await;
        let mut restored = ConfigReadback::default();
        assert_eq!(
            client
                .config_peq_readback(8480, &mut restored, "inputs.0.peq.0.coeff".into())
                .await,
            coeffs
        );
        assert!(!restored.complete);
    }

    #[tokio::test]
    async fn config_readback_retains_legacy_mute_and_route_fallbacks_but_exposes_failure() {
        let client = scripted_client(Box::new(ScriptTransport {
            responses: Arc::new(Mutex::new(std::collections::VecDeque::from([
                vec![commands::RESP_ERROR, 1],
                vec![commands::RESP_ERROR, 1],
            ]))),
            disconnect_data_once: None,
            sent: Arc::new(Mutex::new(Vec::new())),
        }));
        let mut report = ConfigReadback::default();
        assert!(
            !client
                .config_selector_readback(2, false, &mut report, "outputs.0.mute".into())
                .await
        );
        assert!(
            !client
                .config_routing_readback(2, &mut report, "inputs.0.routing.0.enabled".into())
                .await
        );
        assert!(!report.complete);
        assert_eq!(report.fields.len(), 2);
        assert!(report.fields.iter().all(|field| field.source == ConfigReadbackSource::Default && field.error.is_some()));
    }

    #[tokio::test]
    async fn config_readback_does_not_call_collapsed_selectors_device_proof() {
        let client = scripted_client(Box::new(ScriptTransport {
            responses: Arc::new(Mutex::new(std::collections::VecDeque::from([
                vec![0x14, 0, 2, 1, 0, 0, 0],
                vec![0x14, 0, 2, 1, 0, 0, 0],
            ]))),
            disconnect_data_once: None,
            sent: Arc::new(Mutex::new(Vec::new())),
        }));
        let mut report = ConfigReadback::default();
        assert!(
            client
                .config_selector_readback(2, false, &mut report, "outputs.0.mute".into())
                .await
        );
        assert!(
            client
                .config_routing_readback(2, &mut report, "inputs.0.routing.0.enabled".into())
                .await
        );
        assert!(
            report
                .fields
                .iter()
                .all(|field| field.source == ConfigReadbackSource::Default)
        );
        // Presence in a cached map alone is not proof of an acknowledged write.
        client.selector_shadow.lock().await.insert(2, 2);
        let mut cached = ConfigReadback::default();
        assert!(
            client
                .config_routing_readback(2, &mut cached, "inputs.0.routing.0.enabled".into())
                .await
        );
        assert!(!cached.complete);
        client.selector_readback_known.lock().await.insert(2);
        let mut known = ConfigReadback::default();
        assert!(
            client
                .config_routing_readback(2, &mut known, "inputs.0.routing.0.enabled".into())
                .await
        );
        assert!(known.complete);
        assert_eq!(known.fields[0].source, ConfigReadbackSource::Shadow);
    }

    #[tokio::test]
    async fn config_readback_marks_nonfinite_gain_as_failed_even_when_transport_succeeds() {
        let mut response = vec![0x14, 0, 2];
        response.extend(f32::NAN.to_bits().to_le_bytes());
        let client = scripted_client(Box::new(ScriptTransport {
            responses: Arc::new(Mutex::new(std::collections::VecDeque::from([response]))),
            disconnect_data_once: None,
            sent: Arc::new(Mutex::new(Vec::new())),
        }));
        let mut report = ConfigReadback::default();
        assert!(
            client
                .config_gain_readback(2, &mut report, "outputs.0.gain".into())
                .await
                .is_nan()
        );
        assert!(!report.complete);
        assert!(
            report.fields[0]
                .error
                .as_ref()
                .unwrap()
                .contains("non-finite")
        );
    }
    #[tokio::test]
    async fn readback_knowledge_is_invalidated_by_raw_same_slot_reload_and_failed_reset() {
        let client = scripted_client(Box::new(DummyTransport));
        for (request, response) in [
            (
                commands::set_config(0, commands::PRESET_CHANGE_RELOAD_SAME_SLOT),
                vec![commands::RESP_OK],
            ),
            (commands::reset(), vec![commands::RESP_ERROR, 1]),
            (commands::enter_bootloader(), vec![commands::RESP_OK]),
        ] {
            client.trusted_snapshot_presets.lock().await.insert(0);
            client
                .peq_readback_known
                .lock()
                .await
                .insert(8480, (true, true));
            client.scalar_readback_known.lock().await.insert(40);
            client.selector_readback_known.lock().await.insert(2);
            client
                .mirror_raw_command_if_success(&request, &response)
                .await;
            // None of these change a stored preset, so its snapshot trust stays.
            assert!(client.trusted_snapshot_presets.lock().await.contains(&0));
            assert!(client.invalidated_snapshot_presets.lock().await.is_empty());
            assert!(client.peq_readback_known.lock().await.is_empty());
            assert!(client.scalar_readback_known.lock().await.is_empty());
            assert!(client.selector_readback_known.lock().await.is_empty());
        }
    }
    #[tokio::test]
    async fn attempted_word_write_invalidates_only_affected_coefficient_knowledge() {
        let client = scripted_client(Box::new(DummyTransport));
        client
            .peq_readback_known
            .lock()
            .await
            .insert(100, (true, true));
        client
            .peq_readback_known
            .lock()
            .await
            .insert(200, (true, true));
        client.scalar_readback_known.lock().await.extend([101, 500]);
        *client.active_preset.lock().await = Some(0);
        client.trusted_snapshot_presets.lock().await.insert(0);
        // No ACK is needed to revoke knowledge: the device may apply a timed-out write.
        let write = commands::write_dsp_u32(101, 123, true);
        client.invalidate_config_readback_for_write(&write).await;
        {
            let known = client.peq_readback_known.lock().await;
            assert_eq!(known.get(&100), Some(&(false, true)));
            assert_eq!(known.get(&200), Some(&(true, true)));
        }
        assert!(!client.scalar_readback_known.lock().await.contains(&101));
        assert!(client.scalar_readback_known.lock().await.contains(&500));
        // Preset trust survives the attempt and is lost only if the write fails.
        assert!(client.trusted_snapshot_presets.lock().await.contains(&0));
        client.revoke_trust_after_failed_write(&write).await;
        assert!(!client.trusted_snapshot_presets.lock().await.contains(&0));
        assert!(
            client
                .invalidated_snapshot_presets
                .lock()
                .await
                .contains(&0)
        );
    }

    #[tokio::test]
    async fn copy_preset_revokes_trust_in_every_stored_preset() {
        let client = scripted_client(Box::new(DummyTransport));
        client.trusted_snapshot_presets.lock().await.extend([0, 1]);
        client
            .invalidate_config_readback_for_write(&commands::copy_preset(true))
            .await;
        assert!(client.trusted_snapshot_presets.lock().await.is_empty());
        assert!(
            client
                .invalidated_snapshot_presets
                .lock()
                .await
                .contains(&1)
        );
    }

    #[tokio::test]
    async fn ram_only_raw_write_revokes_selector_knowledge() {
        let client = scripted_client(Box::new(DummyTransport));
        client.selector_readback_known.lock().await.insert(300);
        // Control 0xA0 is not parsed as a selector write; it still changes the value.
        let request = commands::write_dsp_u32_le_with_ctrl(300, 1, 0xA0);
        client
            .mirror_raw_command_if_success(&request, &[commands::RESP_OK])
            .await;
        assert!(!client.selector_readback_known.lock().await.contains(&300));
    }

    #[tokio::test]
    async fn attempted_linked_write_invalidates_every_destination_before_ack() {
        let client = scripted_client(Box::new(DummyTransport));
        client
            .selector_readback_known
            .lock()
            .await
            .extend([100, 200, 300]);
        let request = [commands::CMD_WRITE_BIQUAD, 0x81, 0, 100, 0, 200, 2, 0, 0, 0];
        client.invalidate_config_readback_for_write(&request).await;
        let known = client.selector_readback_known.lock().await;
        assert!(!known.contains(&100));
        assert!(!known.contains(&200));
        assert!(known.contains(&300));
    }
    #[tokio::test]
    async fn changed_reconnect_epoch_revokes_acknowledged_fields_but_keeps_snapshot_trust() {
        let client = scripted_client(Box::new(DummyTransport));
        client
            .peq_readback_known
            .lock()
            .await
            .insert(100, (true, true));
        client.scalar_readback_known.lock().await.insert(200);
        client.selector_readback_known.lock().await.insert(300);
        client.trusted_snapshot_presets.lock().await.insert(0);
        // A differing remembered epoch exercises the same transition as queue reconnect.
        *client.readback_epoch.lock().await = u64::MAX;
        assert_eq!(
            client.synchronize_readback_epoch().await,
            client.queue.reconnect_epoch()
        );
        assert!(client.peq_readback_known.lock().await.is_empty());
        assert!(client.scalar_readback_known.lock().await.is_empty());
        assert!(client.selector_readback_known.lock().await.is_empty());
        // Stored presets survive a reconnect.
        assert!(client.trusted_snapshot_presets.lock().await.contains(&0));
        assert!(client.invalidated_snapshot_presets.lock().await.is_empty());
    }
}
