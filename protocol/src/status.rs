//! Status types for JSON serialization — compatible with minidsp-rs StatusSummary.

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
