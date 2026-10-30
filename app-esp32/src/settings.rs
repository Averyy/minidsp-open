//! Persistent controller settings, independent of the connected MiniDSP.

use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use minidsp_protocol::settings::Settings;
use std::sync::Mutex;

pub static SETTINGS: Mutex<Settings> = Mutex::new(Settings::OFF);

pub fn load(partition: &EspDefaultNvsPartition) -> Result<(), Box<dyn std::error::Error>> {
    let nvs = EspNvs::<NvsDefault>::new(partition.clone(), "settings", true)?;
    let settings = match nvs.get_u16("max_vol")? {
        Some(value) if value <= 254 => Settings::new(Some(-0.5 * f64::from(value)))?,
        Some(255) | None => Settings::OFF,
        Some(_) => return Err("invalid stored maximum volume".into()),
    };
    *SETTINGS.lock().unwrap_or_else(|err| err.into_inner()) = settings;
    Ok(())
}

/// A sentinel clears the ceiling; new flags can join this NVS namespace.
pub fn save(
    partition: &EspDefaultNvsPartition,
    settings: Settings,
) -> Result<(), esp_idf_svc::sys::EspError> {
    let nvs = EspNvs::<NvsDefault>::new(partition.clone(), "settings", true)?;
    let raw = settings
        .max_volume_db()
        .map_or(255, |db| (-2.0 * db) as u16);
    nvs.set_u16("max_vol", raw)?;
    Ok(())
}
