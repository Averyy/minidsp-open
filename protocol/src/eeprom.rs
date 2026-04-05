//! Flash/EEPROM memory map helpers.
//!
//! MiniDSP devices store persistent state in flash at page 0xFF (addr_hi = 0xFF).
//! Master status is at 0xFFD8, modification tokens at 0xFFC8.

/// Flash page for device settings.
pub const FLASH_PAGE: u8 = 0xFF;

/// Master status EEPROM address (0xFFD8).
pub const MASTER_STATUS_ADDR: (u8, u8) = (0xFF, 0xD8);

/// DSP version EEPROM address (0xFFA1).
pub const DSP_VERSION_ADDR: (u8, u8) = (0xFF, 0xA1);

/// Serial number EEPROM address (0xFFFE), big-endian u16.
pub const SERIAL_ADDR: (u8, u8) = (0xFF, 0xFE);

/// Modification tokens EEPROM address (0xFFC8).
pub const TOKENS_ADDR: (u8, u8) = (0xFF, 0xC8);

/// Volume offset per source EEPROM base address (0xFFCA).
pub const VOLUME_OFFSETS_ADDR: (u8, u8) = (0xFF, 0xCA);

/// Master status byte layout (from EEPROM 0xFFD8 read of 4 bytes).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MasterStatusRaw {
    /// Current volume as raw byte (gain = -0.5 * value).
    pub volume: u8,
    /// Mute state: 0x00 = unmuted, 0x01 = muted.
    pub mute: u8,
    /// Active preset (0-3).
    pub preset: u8,
    /// Active source index.
    pub source: u8,
}

impl MasterStatusRaw {
    /// Parse from 4 bytes read from EEPROM 0xFFD8.
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 4 {
            return None;
        }
        Some(Self {
            volume: data[0],
            mute: data[1],
            preset: data[2],
            source: data[3],
        })
    }

    /// Volume in dB.
    pub fn volume_db(&self) -> f32 {
        -0.5 * self.volume as f32
    }

    /// Whether the device is muted.
    pub fn is_muted(&self) -> bool {
        self.mute != 0
    }
}

/// Extended status includes Dirac state and other fields from additional EEPROM reads.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExtendedStatus {
    pub master: MasterStatusRaw,
    /// Dirac bypass state: Some(false) = Dirac active, Some(true) = bypassed, None = not supported.
    pub dirac: Option<bool>,
}

/// Parse DSP version from EEPROM read at 0xFFA1.
pub fn parse_dsp_version(data: &[u8]) -> Option<u8> {
    data.first().copied()
}

/// Parse serial number from EEPROM read at 0xFFFE (big-endian u16).
pub fn parse_serial(data: &[u8]) -> Option<u32> {
    if data.len() < 2 {
        return None;
    }
    Some(u16::from_be_bytes([data[0], data[1]]) as u32 + 900000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn master_status_parse() {
        let data = [20, 0x00, 0x01, 0x02]; // volume=20 (-10dB), unmuted, preset 1, source 2
        let status = MasterStatusRaw::from_bytes(&data).unwrap();
        assert_eq!(status.volume, 20);
        assert!(!status.is_muted());
        assert_eq!(status.preset, 1);
        assert_eq!(status.source, 2);
        assert!((status.volume_db() - (-10.0)).abs() < 0.01);
    }

    #[test]
    fn serial_parse() {
        // Serial 912345: encoded as (912345 - 900000) = 12345 = 0x3039
        let data = [0x30, 0x39];
        assert_eq!(parse_serial(&data), Some(912345));
    }
}
