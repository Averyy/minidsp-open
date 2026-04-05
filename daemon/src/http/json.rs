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

/// Status summary for GET /devices/{id} — omits null fields.
#[derive(Debug, Serialize, Deserialize)]
pub struct StatusSummaryJson {
    pub master: MasterStatusGetJson,
    pub input_levels: Vec<f32>,
    pub output_levels: Vec<f32>,
}

/// Master status for GET responses — null fields omitted.
#[derive(Debug, Serialize, Deserialize)]
pub struct MasterStatusGetJson {
    pub preset: u8,
    pub source: String,
    pub volume: f32,
    pub mute: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dirac: Option<bool>,
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
    pub outputs: Option<Vec<InputConfigJson>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct InputConfigJson {
    pub index: u8,
    pub peq: Option<Vec<PeqConfigJson>>,
    pub gain: Option<f32>,
    pub mute: Option<bool>,
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
