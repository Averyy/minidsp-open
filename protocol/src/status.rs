//! Status types for JSON serialization — compatible with minidsp-rs StatusSummary.

#[cfg(feature = "std")]
extern crate alloc;

/// Biquad coefficients for a single PEQ band.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
pub struct BiquadCoeffs {
    pub b0: f32,
    pub b1: f32,
    pub b2: f32,
    pub a1: f32,
    pub a2: f32,
}

impl BiquadCoeffs {
    pub fn from_array(c: &[f32; 5]) -> Self {
        Self {
            b0: c[0],
            b1: c[1],
            b2: c[2],
            a1: c[3],
            a2: c[4],
        }
    }

    pub fn to_array(&self) -> [f32; 5] {
        [self.b0, self.b1, self.b2, self.a1, self.a2]
    }
}

/// PEQ band state: coefficients + bypass flag.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
pub struct PeqBandConfig {
    pub index: u8,
    pub coeff: BiquadCoeffs,
    pub bypass: bool,
}

/// Indexed biquad coefficients used for grouped crossover readback.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
pub struct IndexedBiquadConfig {
    pub index: u8,
    pub coeff: BiquadCoeffs,
}

/// Crossover group state: 4 biquads plus one group-level bypass flag.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
pub struct CrossoverGroupConfig {
    pub index: u8,
    #[cfg_attr(feature = "std", serde(skip_serializing_if = "Vec::is_empty", default))]
    pub filters: Vec<IndexedBiquadConfig>,
    pub bypass: bool,
}

/// Compressor settings for an output channel.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
pub struct CompressorConfig {
    pub bypass: bool,
    pub threshold: f32,
    pub gain: f32,
    pub ratio: f32,
    pub knee: f32,
    pub attack: f32,
    pub release: f32,
}

/// FIR state for a channel.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
pub struct FirConfig {
    pub bypass: bool,
    pub active_taps: u16,
}

/// Input channel config.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
pub struct InputChannelConfig {
    pub index: u8,
    pub gain: Option<f32>,
    pub mute: Option<bool>,
    pub peq: Vec<PeqBandConfig>,
    #[cfg_attr(feature = "std", serde(skip_serializing_if = "Vec::is_empty", default))]
    pub routing: Vec<RoutingConfig>,
}

/// Output channel config.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
pub struct OutputChannelConfig {
    pub index: u8,
    pub gain: f32,
    pub mute: bool,
    pub delay: f32,
    pub peq: Vec<PeqBandConfig>,
    #[cfg_attr(feature = "std", serde(skip_serializing_if = "Vec::is_empty", default))]
    pub crossover: Vec<CrossoverGroupConfig>,
    #[cfg_attr(feature = "std", serde(skip_serializing_if = "Option::is_none"))]
    pub fir: Option<FirConfig>,
    #[cfg_attr(feature = "std", serde(skip_serializing_if = "Option::is_none"))]
    pub compressor: Option<CompressorConfig>,
}

/// Routing matrix entry for one input -> output path.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
pub struct RoutingConfig {
    pub output: u8,
    pub enabled: bool,
    pub gain_db: f32,
    /// Per-cell polarity inversion. `None` for families whose routing matrix
    /// has no `polarity_base` (legacy DSP_100/DSP_101/DSP_105/DSP_111/DSP_28).
    /// `Some(true)` = inverted, `Some(false)` = normal.
    #[cfg_attr(
        feature = "std",
        serde(skip_serializing_if = "Option::is_none", default)
    )]
    pub polarity_inverted: Option<bool>,
}

/// One channel of the bass-management gain block (8 channels on HT 113/115).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
pub struct BmChannelConfig {
    pub index: u8,
    pub muted: bool,
    pub gain_db: f32,
}

/// One cell of the 9 × 8 bass-management matrix mixer.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
pub struct BmMixerCellConfig {
    /// 0-based row index. The last row (row index `rows - 1`) is the LFE
    /// virtual channel; see `BmMatrixMixer::is_lfe_row`.
    pub row: u8,
    /// 0-based output column index.
    pub col: u8,
    pub enabled: bool,
    pub gain_db: f32,
    pub polarity_inverted: bool,
}

/// Snapshot of a device's full bass-management state. Returned from
/// `MiniDspClient::read_bass_management_state`.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
pub struct BmStateConfig {
    /// 8 per-channel bass-management channels (gain + mute).
    pub channels: Vec<BmChannelConfig>,
    /// 9 × 8 = 72 mixer cells (row-major). Row index `rows - 1` is the LFE
    /// virtual channel.
    pub mixer: Vec<BmMixerCellConfig>,
    /// Number of rows in the matrix mixer (9 for HT 113/115, including LFE).
    pub rows: u8,
    /// Number of output columns in the matrix mixer (8 for HT 113/115).
    pub cols: u8,
}

/// Patch payload for [`BmChannelConfig`] writes — only the populated fields
/// are applied.
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
pub struct BmChannelPatch {
    #[cfg_attr(
        feature = "std",
        serde(skip_serializing_if = "Option::is_none", default)
    )]
    pub muted: Option<bool>,
    #[cfg_attr(
        feature = "std",
        serde(skip_serializing_if = "Option::is_none", default)
    )]
    pub gain_db: Option<f32>,
}

/// Patch payload for [`BmMixerCellConfig`] writes — only the populated fields
/// are applied.
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
pub struct BmMixerCellPatch {
    #[cfg_attr(
        feature = "std",
        serde(skip_serializing_if = "Option::is_none", default)
    )]
    pub enabled: Option<bool>,
    #[cfg_attr(
        feature = "std",
        serde(skip_serializing_if = "Option::is_none", default)
    )]
    pub gain_db: Option<f32>,
    #[cfg_attr(
        feature = "std",
        serde(skip_serializing_if = "Option::is_none", default)
    )]
    pub polarity_inverted: Option<bool>,
}

/// Full device config — JSON-serializable representation of all device state.
///
/// Used for config readback (GET /devices/{id}/config), measurement mode
/// save/restore, and config export/import.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
pub struct DeviceConfig {
    pub master_status: MasterStatusView,
    pub inputs: Vec<InputChannelConfig>,
    pub outputs: Vec<OutputChannelConfig>,
}

/// Master status — the main device state.
///
/// When serialized for GET responses, null fields are omitted (`skip_serializing_if`).
/// When serialized for POST responses, null fields are included.
#[derive(Debug, Clone, PartialEq)]
pub struct MasterStatus {
    pub preset: Option<u8>,
    pub source: Option<String>,
    pub volume: Option<f32>,
    pub mute: Option<bool>,
    pub dirac: Option<bool>,
}

impl MasterStatus {
    pub fn new() -> Self {
        Self {
            preset: None,
            source: None,
            volume: None,
            mute: None,
            dirac: None,
        }
    }
}

impl Default for MasterStatus {
    fn default() -> Self {
        Self::new()
    }
}

/// Status summary — compatible with minidsp-rs StatusSummary JSON format.
///
/// Used by ezBEQ, Home Assistant, and other consumers.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
pub struct StatusSummary {
    pub master: MasterStatusView,
    pub input_levels: Vec<f32>,
    pub output_levels: Vec<f32>,
}

/// View of master status for GET responses — omits null fields.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
pub struct MasterStatusView {
    pub preset: u8,
    pub source: String,
    pub volume: f32,
    pub mute: bool,
    #[cfg_attr(feature = "std", serde(skip_serializing_if = "Option::is_none"))]
    pub dirac: Option<bool>,
}

/// Device info for the /devices list endpoint.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceInfo {
    pub url: String,
    pub version: DeviceVersion,
    pub product_name: String,
}

/// Device version information.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceVersion {
    pub hw_id: u8,
    pub fw_major: u8,
    pub fw_minor: u8,
    pub dsp_version: u8,
    pub serial: u32,
}

/// Assemble device identity while leaving transport URL selection to the caller.
pub fn device_info(
    url: String,
    hw_id: &crate::commands::HardwareId,
    dsp_version: u8,
    serial: u32,
) -> DeviceInfo {
    DeviceInfo {
        url,
        version: DeviceVersion {
            hw_id: hw_id.hw_id,
            fw_major: hw_id.fw_major,
            fw_minor: hw_id.fw_minor,
            dsp_version,
            serial,
        },
        product_name: crate::device::probe::product_name(hw_id.hw_id, dsp_version).to_string(),
    }
}
