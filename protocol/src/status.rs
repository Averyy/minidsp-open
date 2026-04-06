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
        Self { b0: c[0], b1: c[1], b2: c[2], a1: c[3], a2: c[4] }
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

/// Input channel config.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
pub struct InputChannelConfig {
    pub index: u8,
    pub gain: Option<f32>,
    pub mute: Option<bool>,
    pub peq: Vec<PeqBandConfig>,
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
    #[cfg_attr(feature = "std", serde(skip_serializing_if = "Option::is_none"))]
    pub compressor: Option<CompressorConfig>,
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
