//! JSON serialization types — compatible with minidsp-rs wire format.

use serde::{Deserialize, Serialize};

/// Device info for GET /devices response.
#[derive(Debug, Serialize, Deserialize)]
pub struct DeviceJson {
    pub url: String,
    pub version: VersionJson,
    pub product_name: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VersionJson {
    pub hw_id: u8,
    pub fw_major: u8,
    pub fw_minor: u8,
    pub dsp_version: u8,
    pub serial: u32,
}

/// Master status for POST request body.
#[derive(Debug, Serialize, Deserialize)]
pub struct MasterStatusPostJson {
    pub preset: Option<u8>,
    pub source: Option<String>,
    pub volume: Option<f32>,
    pub mute: Option<bool>,
    pub dirac: Option<bool>,
}

/// Master status for POST response — null fields INCLUDED.
#[derive(Debug, Serialize, Deserialize)]
pub struct MasterStatusResponseJson {
    pub preset: Option<u8>,
    pub source: Option<String>,
    pub volume: Option<f32>,
    pub mute: Option<bool>,
    pub dirac: Option<bool>,
}

/// Config write request body for POST /devices/{id}/config.
#[derive(Debug, Serialize, Deserialize)]
pub struct ConfigWriteJson {
    pub master_status: Option<MasterStatusPostJson>,
    pub inputs: Option<Vec<InputConfigJson>>,
    pub outputs: Option<Vec<OutputConfigJson>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct InputConfigJson {
    pub index: u8,
    pub peq: Option<Vec<PeqConfigJson>>,
    pub gain: Option<f32>,
    pub mute: Option<bool>,
}

/// Output config for POST /devices/{id}/config — includes delay and polarity.
#[derive(Debug, Serialize, Deserialize)]
pub struct OutputConfigJson {
    pub index: u8,
    pub peq: Option<Vec<PeqConfigJson>>,
    pub gain: Option<f32>,
    pub mute: Option<bool>,
    /// Delay in Duration format: {"secs": N, "nanos": N} (matches minidsp-rs wire format).
    pub delay: Option<std::time::Duration>,
    /// Polarity inversion: true = inverted, false = normal.
    pub invert: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PeqConfigJson {
    pub index: u8,
    pub coeff: Option<BiquadCoeffJson>,
    pub bypass: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BiquadCoeffJson {
    pub b0: f32,
    pub b1: f32,
    pub b2: f32,
    pub a1: f32,
    pub a2: f32,
}
