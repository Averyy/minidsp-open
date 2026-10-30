//! Persistent controller settings, independent of the connected MiniDSP.

use crate::auth::StoredPassword;
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use minidsp_protocol::auth::PasswordRecord;
use minidsp_protocol::settings::Settings;
use std::sync::Mutex;

pub static SETTINGS: Mutex<Settings> = Mutex::new(Settings::OFF);

/// Never fails boot. An unreadable ceiling is logged and left off; an
/// unreadable password record keeps the board locked until BOOT recovery,
/// so corruption or a newer record format after a rollback can't open it.
pub fn load(partition: &EspDefaultNvsPartition) {
    let nvs = match EspNvs::<NvsDefault>::new(partition.clone(), "settings", true) {
        Ok(nvs) => nvs,
        Err(e) => {
            log::error!("settings: NVS namespace unavailable ({e}); password locked, no ceiling");
            crate::auth::load(partition, StoredPassword::Unreadable);
            return;
        }
    };
    let settings = match nvs.get_u16("max_vol") {
        Ok(Some(value)) if value <= 254 => {
            Settings::new(Some(-0.5 * f64::from(value))).unwrap_or(Settings::OFF)
        }
        Ok(Some(255) | None) => Settings::OFF,
        Ok(Some(value)) => {
            log::error!("settings: ignoring invalid stored maximum volume {value}");
            Settings::OFF
        }
        Err(e) => {
            log::error!("settings: could not read maximum volume: {e}");
            Settings::OFF
        }
    };
    let mut bytes = [0; minidsp_protocol::auth::RECORD_BYTES];
    let password = match nvs.get_blob("password", &mut bytes) {
        Ok(None) => StoredPassword::None,
        Ok(Some(bytes)) => {
            match PasswordRecord::decode(bytes) {
                Some(record) => StoredPassword::Valid(record),
                None => {
                    log::error!("settings: stored password record is unreadable; locked until BOOT recovery");
                    StoredPassword::Unreadable
                }
            }
        }
        Err(e) => {
            log::error!(
                "settings: could not read password record ({e}); locked until BOOT recovery"
            );
            StoredPassword::Unreadable
        }
    };
    crate::auth::load(partition, password);
    *SETTINGS.lock().unwrap_or_else(|err| err.into_inner()) = settings;
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

/// Save before publishing the new auth state, so a failed write retains protection.
pub fn save_password(
    partition: &EspDefaultNvsPartition,
    record: Option<&PasswordRecord>,
) -> Result<(), esp_idf_svc::sys::EspError> {
    let nvs = EspNvs::<NvsDefault>::new(partition.clone(), "settings", true)?;
    if let Some(record) = record {
        nvs.set_blob("password", &record.encode())?;
    } else {
        nvs.remove("password")?;
    }
    Ok(())
}
