//! Controller settings and pure master-volume safety policy.

use crate::commands::CMD_SET_VOLUME;
use alloc::{format, string::String};

/// Additive controller settings. An absent ceiling preserves existing behavior.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "std", serde(rename_all = "camelCase", deny_unknown_fields))]
pub struct Settings {
    #[cfg_attr(feature = "std", serde(deserialize_with = "deserialize_max_volume"))]
    max_volume_db: Option<f32>,
}

impl Settings {
    pub const OFF: Self = Self {
        max_volume_db: None,
    };

    pub fn new(max_volume_db: Option<f64>) -> Result<Self, String> {
        if let Some(db) = max_volume_db
            && (!db.is_finite() || !(-127.0..=0.0).contains(&db) || (db * 2.0).fract() != 0.0)
        {
            return Err("maxVolumeDb must be null or -127..=0 dB in multiples of 0.5 dB".into());
        }
        Ok(Self {
            max_volume_db: max_volume_db.map(|db| db as f32),
        })
    }

    pub fn max_volume_db(self) -> Option<f32> {
        self.max_volume_db
    }

    pub fn clamp_db(self, db: f32) -> f32 {
        self.max_volume_db.map_or(db, |max| db.min(max))
    }

    /// Raw master volume is attenuation: 0 = 0 dB, 255 = -127.5 dB.
    pub fn clamp_raw(self, attenuation: u8) -> u8 {
        self.max_volume_db
            .map_or(attenuation, |db| attenuation.max((-2.0 * db) as u8))
    }

    /// Positive steps raise volume. Saturate at the hardware's full raw range.
    pub fn relative_steps(self, current: u8, steps: i16) -> u8 {
        self.clamp_raw((i32::from(current) - i32::from(steps)).clamp(0, 255) as u8)
    }

    pub fn validate_rattle_volumes(self, volumes: &[f32]) -> Result<(), String> {
        if let Some(max) = self.max_volume_db
            && volumes.iter().any(|&db| db > max)
        {
            return Err(format!(
                "volumesDb exceeds maximum master volume of {max} dB"
            ));
        }
        Ok(())
    }

    /// Both OLD and NEW dialects use SetVolume (0x42), one attenuation byte.
    /// Source: commands::set_volume; DSP address/value dialects do not apply.
    pub fn clamp_payload(self, payload: &mut [u8]) {
        if payload.first() == Some(&CMD_SET_VOLUME) && payload.len() >= 2 {
            payload[1] = self.clamp_raw(payload[1]);
        }
    }

    /// Clamp a checksummed request frame in place, preserving trailing padding.
    /// Other commands and invalid/incomplete frames remain byte-identical.
    pub fn clamp_frame(self, frame: &mut [u8]) {
        if self.max_volume_db.is_none() || frame.len() < 4 {
            return;
        }
        let len = usize::from(frame[0]);
        if len < 3 || frame.len() <= len || frame[1] != CMD_SET_VOLUME {
            return;
        }
        let checksum = frame[..len].iter().fold(0u8, |sum, &b| sum.wrapping_add(b));
        if frame[len] != checksum {
            return;
        }
        self.clamp_payload(&mut frame[1..len]);
        frame[0] = len as u8;
        frame[len] = frame[..len].iter().fold(0u8, |sum, &b| sum.wrapping_add(b));
    }
}

#[cfg(feature = "std")]
fn deserialize_max_volume<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<f32>, D::Error> {
    let db = <Option<f64> as serde::Deserialize>::deserialize(deserializer)?;
    Settings::new(db)
        .map(|settings| settings.max_volume_db)
        .map_err(serde::de::Error::custom)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{commands, dialect::Dialect, packet};

    #[test]
    fn validates_exact_half_db_settings() {
        for db in [None, Some(-127.0), Some(-12.5), Some(0.0)] {
            assert!(Settings::new(db).is_ok());
        }
        for db in [f64::NAN, f64::INFINITY, -127.5, 0.5, -12.25, -12.50000001] {
            assert!(Settings::new(Some(db)).is_err());
        }
        for json in [
            r#"{}"#,
            r#"{"maxVolumeDb":-12.50000001}"#,
            r#"{"maxVolumeDb":true}"#,
        ] {
            assert!(serde_json::from_str::<Settings>(json).is_err());
        }
        assert_eq!(
            serde_json::from_str::<Settings>(r#"{"maxVolumeDb":null}"#).unwrap(),
            Settings::OFF
        );
    }

    #[test]
    fn clamps_absolute_relative_and_rattle() {
        let settings = Settings::new(Some(-20.5)).unwrap();
        assert_eq!(settings.clamp_db(0.0), -20.5);
        assert_eq!(settings.clamp_raw(0), 41);
        assert_eq!(settings.clamp_raw(255), 255);
        assert_eq!(settings.relative_steps(41, 1), 41);
        assert_eq!(settings.relative_steps(41, -1), 42);
        assert_eq!(settings.relative_steps(255, -1), 255);
        assert_eq!(Settings::OFF.relative_steps(0, 1), 0);
        assert!(settings.validate_rattle_volumes(&[-40.0, -20.5]).is_ok());
        assert!(
            settings
                .validate_rattle_volumes(&[-20.0])
                .unwrap_err()
                .contains("-20.5")
        );
    }

    #[test]
    fn clamps_both_dialects_and_recomputes_checksum_preserving_padding() {
        for _dialect in [Dialect::OLD, Dialect::NEW] {
            let mut frame = packet::encode_frame(&commands::set_volume(0));
            frame.resize(64, 0xff);
            Settings::new(Some(-20.5)).unwrap().clamp_frame(&mut frame);
            assert_eq!(&frame[..4], &[3, 0x42, 41, 110]);
            assert_eq!(packet::decode_frame(&frame).unwrap(), [0x42, 41]);
            assert!(frame[4..].iter().all(|&b| b == 0xff));
        }
    }

    #[test]
    fn leaves_disabled_quiet_other_and_malformed_frames_unchanged() {
        let settings = Settings::new(Some(-20.0)).unwrap();
        for payload in [
            commands::set_volume(100),
            commands::set_mute(false),
            commands::read_hardware_id(),
        ] {
            let mut frame = packet::encode_frame(&payload);
            let original = frame.clone();
            settings.clamp_frame(&mut frame);
            assert_eq!(frame, original);
        }
        let mut frame = packet::encode_frame(&commands::set_volume(0));
        let original = frame.clone();
        Settings::OFF.clamp_frame(&mut frame);
        assert_eq!(frame, original);
        for mut frame in [vec![], vec![3, 0x42, 0], vec![3, 0x42, 0, 0]] {
            let original = frame.clone();
            settings.clamp_frame(&mut frame);
            assert_eq!(frame, original);
        }
    }
}
