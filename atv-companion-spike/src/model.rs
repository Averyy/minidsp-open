use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredDevice {
    pub id: String,
    pub name: String,
    pub host: String,
    pub connect_host: String,
    pub port: u16,
    pub addresses: Vec<String>,
    pub txt: BTreeMap<String, String>,
    pub last_seen_ms: u64,
}

impl DiscoveredDevice {
    pub fn manual(host: String, port: u16) -> Self {
        Self {
            id: format!("{host}:{port}"),
            name: host.clone(),
            host: host.clone(),
            connect_host: host.clone(),
            port,
            addresses: vec![host],
            txt: BTreeMap::new(),
            last_seen_ms: now_millis(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DevicesStore {
    pub devices: BTreeMap<String, DiscoveredDevice>,
}

impl DevicesStore {
    pub fn replace(&mut self, devices: Vec<DiscoveredDevice>) {
        self.devices = devices
            .into_iter()
            .map(|device| (device.id.clone(), device))
            .collect();
    }

    pub fn resolve(
        &self,
        selector: DeviceSelector<'_>,
    ) -> Result<&DiscoveredDevice, DeviceLookupError> {
        match selector {
            DeviceSelector::Only if self.devices.len() == 1 => self
                .devices
                .values()
                .next()
                .ok_or(DeviceLookupError::NoDevices),
            DeviceSelector::Only if self.devices.is_empty() => Err(DeviceLookupError::NoDevices),
            DeviceSelector::Only => Err(DeviceLookupError::Ambiguous(
                self.devices.keys().cloned().collect(),
            )),
            DeviceSelector::Selector(target) => {
                if let Some(device) = self.devices.get(target) {
                    return Ok(device);
                }

                let mut matches = self
                    .devices
                    .values()
                    .filter(|device| {
                        device.name.eq_ignore_ascii_case(target)
                            || device.connect_host.eq_ignore_ascii_case(target)
                            || device.id.eq_ignore_ascii_case(target)
                    })
                    .collect::<Vec<_>>();

                if matches.len() == 1 {
                    return Ok(matches.remove(0));
                }

                Err(DeviceLookupError::NotFound(target.to_owned()))
            }
        }
    }
}

#[derive(Debug, Clone)]
pub enum DeviceSelector<'a> {
    Only,
    Selector(&'a str),
}

impl<'a> DeviceSelector<'a> {
    pub fn from_optional(value: Option<&'a str>) -> Self {
        match value {
            Some(value) => Self::Selector(value),
            None => Self::Only,
        }
    }
}

#[derive(Debug, Error)]
pub enum DeviceLookupError {
    #[error("no devices available; run scan first")]
    NoDevices,
    #[error("device selector is ambiguous: {0:?}")]
    Ambiguous(Vec<String>),
    #[error("could not find device '{0}'")]
    NotFound(String),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CredentialsStore {
    pub by_device: BTreeMap<String, CredentialRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialRecord {
    pub device_id: String,
    pub client_id_hex: String,
    pub client_seed_hex: String,
    pub client_ltpk_hex: String,
    pub atv_id_hex: String,
    pub atv_ltpk_hex: String,
    pub paired_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetadataEntry {
    pub device_id: String,
    pub observed_at_ms: u64,
    pub kind: String,
    pub raw: JsonValue,
    pub normalized: NormalizedMetadata,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NormalizedMetadata {
    pub app_bundle_id: Option<String>,
    pub title: Option<String>,
    pub episode_title: Option<String>,
    pub season_number: Option<u64>,
    pub episode_number: Option<u64>,
    pub duration: Option<f64>,
    pub playback_state: Option<String>,
    pub elapsed_time: Option<f64>,
    pub timestamp_ms: Option<u64>,
    pub canonical_id: Option<String>,
    pub program_id: Option<String>,
    pub itunes_store_id: Option<String>,
    pub product_page_url: Option<String>,
    pub additional_identifier: Option<String>,
}

pub fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}
