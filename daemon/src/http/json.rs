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
/// Dedicated fail-closed endpoint: legacy /config must never be a fallback.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MuteRestoreJson {
    pub if_snapshot_token: String,
    pub outputs: Vec<MuteRestoreOutputJson>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MuteRestoreOutputJson {
    pub index: u8,
    pub mute: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ConfigWriteJson {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_snapshot_token: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routing: Option<Vec<RoutingConfigJson>>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crossover: Option<Vec<CrossoverConfigJson>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fir: Option<FirConfigJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compressor: Option<CompressorConfigJson>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PeqConfigJson {
    pub index: u8,
    pub coeff: Option<BiquadCoeffJson>,
    pub bypass: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct BiquadCoeffJson {
    pub b0: f32,
    pub b1: f32,
    pub b2: f32,
    pub a1: f32,
    pub a2: f32,
}

impl Default for BiquadCoeffJson {
    fn default() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RoutingConfigJson {
    #[serde(alias = "index")]
    pub output: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mute: Option<bool>,
    #[serde(alias = "gain", skip_serializing_if = "Option::is_none")]
    pub gain_db: Option<f32>,
    /// Per-cell polarity inversion. Only honored on families whose
    /// `RoutingMatrix.polarity_base` is populated (Flex Eight, Flex HT/HTx,
    /// DDRC-88BM). `None` leaves the cell's polarity untouched.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub polarity_inverted: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CompressorConfigJson {
    pub bypass: Option<bool>,
    pub threshold: Option<f32>,
    pub gain: Option<f32>,
    pub ratio: Option<f32>,
    pub knee: Option<f32>,
    pub attack: Option<f32>,
    pub release: Option<f32>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IndexedBiquadJson {
    pub index: u8,
    pub coeff: Option<BiquadCoeffJson>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CrossoverConfigJson {
    pub index: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(
        alias = "coeff",
        default,
        deserialize_with = "deserialize_crossover_filters"
    )]
    pub filters: Option<Vec<IndexedBiquadJson>>,
    pub bypass: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FirConfigJson {
    pub bypass: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coefficients: Option<Vec<f32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_taps: Option<u16>,
}

fn deserialize_crossover_filters<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Vec<IndexedBiquadJson>>, D::Error> {
    #[derive(Deserialize)]
    struct Flat {
        index: u8,
        #[serde(flatten)]
        coeff: BiquadCoeffJson,
    }
    let filters = Option::<Vec<serde_json::Value>>::deserialize(deserializer)?;
    filters
        .map(|filters| {
            filters
                .into_iter()
                .map(|value| {
                    if value.get("coeff").is_none() {
                        let flat: Flat =
                            serde_json::from_value(value).map_err(serde::de::Error::custom)?;
                        Ok(IndexedBiquadJson {
                            index: flat.index,
                            coeff: Some(flat.coeff),
                        })
                    } else {
                        serde_json::from_value(value).map_err(serde::de::Error::custom)
                    }
                })
                .collect()
        })
        .transpose()
}

#[cfg(test)]
mod compatibility_tests {
    use super::*;

    // minidsp-rs 8e987f9, minidsp/src/model.rs:208-250, 316-350:
    // routing uses index/mute/gain; crossover coeff entries are flat biquads.
    #[test]
    fn upstream_config_routing_and_crossover_shapes() {
        let config: ConfigWriteJson = serde_json::from_str(r#"{
            "inputs":[{"index":0,"routing":[{"index":2,"mute":true},{"index":1,"gain":-3.5}]}],
            "outputs":[{"index":0,"crossover":[{"index":1,"coeff":[{"index":2,"b0":1,"b1":0,"b2":0,"a1":0,"a2":0}]}]}]
        }"#).unwrap();
        let routes = config.inputs.unwrap().remove(0).routing.unwrap();
        assert_eq!(routes[0].output, 2);
        assert_eq!(routes[0].mute, Some(true));
        assert_eq!(routes[0].gain_db, None);
        assert_eq!(routes[1].gain_db, Some(-3.5));
        assert_eq!(routes[1].enabled, None);
        let filter = config
            .outputs
            .unwrap()
            .remove(0)
            .crossover
            .unwrap()
            .remove(0)
            .filters
            .unwrap()
            .remove(0);
        assert_eq!(filter.index, 2);
        assert_eq!(filter.coeff.unwrap().b0, 1.0);
    }

    #[test]
    fn existing_config_extensions_and_invalid_coefficients_remain_supported() {
        let config: ConfigWriteJson = serde_json::from_str(r#"{
            "inputs":[{"index":0,"routing":[{"output":2,"enabled":true,"gain_db":-2}]}],
            "outputs":[{"index":0,"crossover":[{"index":0,"filters":[{"index":1,"coeff":{"b0":1,"b1":0,"b2":0,"a1":0,"a2":0}}]}]}]
        }"#).unwrap();
        assert_eq!(
            config.inputs.unwrap()[0].routing.as_ref().unwrap()[0].enabled,
            Some(true)
        );
        assert!(
            config.outputs.unwrap()[0].crossover.as_ref().unwrap()[0]
                .filters
                .as_ref()
                .unwrap()[0]
                .coeff
                .is_some()
        );
        assert!(
            serde_json::from_str::<CrossoverConfigJson>(
                r#"{"index":0,"coeff":[{"index":0,"b0":"bad","b1":0,"b2":0,"a1":0,"a2":0}]}"#
            )
            .is_err()
        );
    }
}

#[cfg(test)]
mod mute_restore_contract_tests {
    use super::MuteRestoreJson;

    #[test]
    fn conditional_restore_requires_token_and_rejects_other_writes() {
        assert!(
            serde_json::from_str::<MuteRestoreJson>(
                r#"{"if_snapshot_token":"opaque","outputs":[{"index":0,"mute":true}]}"#
            )
            .is_ok()
        );
        for body in [
            r#"{"outputs":[{"index":0,"mute":true}]}"#,
            r#"{"if_snapshot_token":"opaque","outputs":[{"index":0,"mute":true,"gain":0}]}"#,
            r#"{"if_snapshot_token":"opaque","outputs":[{"index":0,"mute":true}],"master_status":{"preset":1}}"#,
            r#"{"if_snapshot_token":"opaque","outputs":[{"index":0}]}"#,
        ] {
            assert!(serde_json::from_str::<MuteRestoreJson>(body).is_err());
        }
    }
}
