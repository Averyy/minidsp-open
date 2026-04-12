//! Flash/EEPROM memory map helpers.
//!
//! MiniDSP devices store persistent state in flash at page 0xFF (addr_hi = 0xFF).
//! The full modern layout, confirmed against the official Device Console
//! `dspMessage.js` sources (2026-04-11 RE session):
//!
//! | Addr    | Size | Meaning                                     |
//! |---------|------|---------------------------------------------|
//! | 0xFFA1  | 1    | DSP id                                      |
//! | 0xFFA3  | 1-9  | Verify-program status + destroy-protect byte|
//! | 0xFFA9  | 1    | AudioSrcChanged unsolicited notification    |
//! | 0xFFC8  | 16   | Modification tokens (4 preset slots × 4 B)  |
//! | 0xFFCA  | 6    | Per-source volume offset base               |
//! | 0xFFD8  | 4    | [active_preset, source, volume, mute]       |
//! | 0xFFD9  | 1    | Current source (also covered by 0xFFD8[+1]) |
//! | 0xFFDA  | 2    | Master volume + mute (also via 0xFFD8[+2])  |
//! | 0xFFE0  | 1    | Master FIR bypass                           |
//! | 0xFFE8  | 5    | Display idle / brightness settings          |
//! | 0xFFEA  | ≤32  | Selectable audio source descriptor (1 B/src)|
//! | 0xFFED  | 1    | DRE (Dynamic Range Enhancement) status      |
//! | 0xFFFE  | 2    | Serial number (big-endian u16, + 900000)    |
//! | 0xFE00  | var  | Per-source volume offset values (legacy)    |

/// Flash page for device settings.
pub const FLASH_PAGE: u8 = 0xFF;

/// DSP version EEPROM address (0xFFA1).
pub const DSP_VERSION_ADDR: (u8, u8) = (0xFF, 0xA1);

/// Verify-program / destroy-protection status byte (0xFFA3).
///
/// After a preset save the device writes a status code here; Device Console
/// checks `byte <= 0x0D` as the success condition (`verifyProgram()` in
/// `app-extracted/build/src/userPreset.js`). Values > 0x0D indicate the
/// block write failed.
pub const VERIFY_PROGRAM_ADDR: (u8, u8) = (0xFF, 0xA3);

/// `AudioSrcChanged` unsolicited-notification address (0xFFA9, 1 byte).
///
/// Despite the historical "selectable audio sources" label, this address is
/// **not** a source list. The device emits a 1-byte `ReadFlash`-shaped frame
/// at this address whenever the active source changes (front-panel knob,
/// remote, etc.). Device Console's `dspMessage.js` `AudioSrcChangedMsg`
/// asserts the payload is exactly 1 byte (the new source id) and we observed
/// the same on the physical 2x4 HD (2026-04-11 RE session).
///
/// The actual selectable-source descriptor lives at
/// [`AUDIO_SRC_SELECT_ADDR`] (0xFFEA).
pub const AUDIO_SRC_CHANGED_NOTIFICATION_ADDR: (u8, u8) = (0xFF, 0xA9);

/// Selectable audio source descriptor base (0xFFEA, up to 32 bytes).
///
/// Variable-length list of source IDs (1 byte each) that the device exposes
/// as user-selectable. Sourced from `dspMessage.js` `AudioSrcSelectMsg`
/// (`flashAddrLB=0xea`, `maxNumSrcs=0x20`). On devices with a fixed source
/// set (2x4 HD: Analog/Toslink/USB) this region is uninitialized; it is
/// populated on HT/HTx-class devices where the physical input set varies.
/// No client API is exposed yet — pending live verification on an HT device.
pub const AUDIO_SRC_SELECT_ADDR: (u8, u8) = (0xFF, 0xEA);

/// Maximum number of source IDs returned by a read at
/// [`AUDIO_SRC_SELECT_ADDR`]. Matches Device Console's `maxNumSrcs = 0x20`.
pub const AUDIO_SRC_SELECT_MAX_SOURCES: u8 = 32;

/// Modification tokens EEPROM address (0xFFC8).
///
/// Four 4-byte big-endian tokens, one per preset slot. Device Console's
/// drift-detection flow compares the device's live token for the active slot
/// against the `<timestamp>` field in its cached `setting<N>.xml`. A
/// mismatch triggers the "Revert / Initialize / Disconnect" dialog.
pub const TOKENS_ADDR: (u8, u8) = (0xFF, 0xC8);

/// Size of one preset modification token in bytes (big-endian u32).
pub const TOKEN_SIZE_BYTES: u8 = 4;

/// Compute the flash address of the modification token for a preset slot.
/// `slot` is 0-based; valid range is `0..=3`.
pub const fn token_addr_for_slot(slot: u8) -> (u8, u8) {
    let (hi, lo) = TOKENS_ADDR;
    (hi, lo + slot * TOKEN_SIZE_BYTES)
}

/// Volume offset per source EEPROM base address (0xFFCA).
pub const VOLUME_OFFSETS_ADDR: (u8, u8) = (0xFF, 0xCA);

/// Master status EEPROM address (0xFFD8). 4 bytes: preset, source, vol, mute.
pub const MASTER_STATUS_ADDR: (u8, u8) = (0xFF, 0xD8);

/// Active preset slot byte (0xFFD8, 1 byte). Equivalent to the first byte of
/// [`MASTER_STATUS_ADDR`] — exposed separately for callers that only want the
/// preset without triggering a 4-byte read.
pub const ACTIVE_PRESET_ADDR: (u8, u8) = (0xFF, 0xD8);

/// Current audio source byte (0xFFD9, 1 byte). Equivalent to
/// [`MASTER_STATUS_ADDR`] + 1.
pub const CURRENT_SOURCE_ADDR: (u8, u8) = (0xFF, 0xD9);

/// Master volume + mute (0xFFDA, 2 bytes). Equivalent to
/// [`MASTER_STATUS_ADDR`] + 2.
pub const MASTER_VOL_MUTE_ADDR: (u8, u8) = (0xFF, 0xDA);

/// Master FIR bypass state (0xFFE0, 1 byte).
pub const MASTER_FIR_BYPASS_ADDR: (u8, u8) = (0xFF, 0xE0);

/// Display idle / brightness settings (0xFFE8, 5 bytes).
pub const DISPLAY_IDLE_ADDR: (u8, u8) = (0xFF, 0xE8);

/// DRE (Dynamic Range Enhancement) status byte (0xFFED).
pub const DRE_STATUS_ADDR: (u8, u8) = (0xFF, 0xED);

/// Serial number EEPROM address (0xFFFE), big-endian u16.
pub const SERIAL_ADDR: (u8, u8) = (0xFF, 0xFE);

/// Device Console's `verifyProgram()` threshold: status bytes in `0..=0x0D`
/// indicate a successful preset save. Anything above is a failure.
pub const VERIFY_PROGRAM_OK_MAX: u8 = 0x0D;

/// True if `byte` (read from [`VERIFY_PROGRAM_ADDR`]) indicates a successful
/// preset save. Mirrors Device Console's `userPreset.js` `verifyProgram()`
/// check (`status <= 0x0D`). Anything above the threshold means the flash
/// block write failed and the persisted preset is corrupt.
pub const fn is_verify_program_ok(byte: u8) -> bool {
    byte <= VERIFY_PROGRAM_OK_MAX
}

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
    /// Byte order: [preset, source, volume, mute] — verified against miniDSP 2x4 HD simulator.
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 4 {
            return None;
        }
        Some(Self {
            preset: data[0],
            source: data[1],
            volume: data[2],
            mute: data[3],
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
        // Byte order at 0xFFD8: [preset, source, volume, mute]
        // Verified against miniDSP 2x4 HD simulator
        let data = [0x01, 0x02, 20, 0x00]; // preset 1, source 2, volume=20 (-10dB), unmuted
        let status = MasterStatusRaw::from_bytes(&data).unwrap();
        assert_eq!(status.preset, 1);
        assert_eq!(status.source, 2);
        assert_eq!(status.volume, 20);
        assert!(!status.is_muted());
        assert!((status.volume_db() - (-10.0)).abs() < 0.01);
    }

    #[test]
    fn master_status_simulator_defaults() {
        // Simulator defaults: preset=0, source=2 (USB), volume=0 (0dB), mute=0
        let data = [0x00, 0x02, 0x00, 0x00];
        let status = MasterStatusRaw::from_bytes(&data).unwrap();
        assert_eq!(status.preset, 0);
        assert_eq!(status.source, 2);
        assert_eq!(status.volume, 0);
        assert!(!status.is_muted());
        assert!((status.volume_db() - 0.0).abs() < 0.01);
    }

    #[test]
    fn serial_parse() {
        // Serial 912345: encoded as (912345 - 900000) = 12345 = 0x3039
        let data = [0x30, 0x39];
        assert_eq!(parse_serial(&data), Some(912345));
    }

    #[test]
    fn token_addr_for_slot_strides_by_four_from_ffc8() {
        assert_eq!(token_addr_for_slot(0), (0xFF, 0xC8));
        assert_eq!(token_addr_for_slot(1), (0xFF, 0xCC));
        assert_eq!(token_addr_for_slot(2), (0xFF, 0xD0));
        assert_eq!(token_addr_for_slot(3), (0xFF, 0xD4));
    }

    #[test]
    fn verify_program_ok_threshold_matches_device_console_constant() {
        // Device Console refuses any byte > 0x0D from 0xFFA3 as a failed save.
        assert_eq!(VERIFY_PROGRAM_OK_MAX, 0x0D);
    }

    #[test]
    fn is_verify_program_ok_boundary() {
        assert!(is_verify_program_ok(0x00));
        assert!(is_verify_program_ok(0x0D));
        assert!(!is_verify_program_ok(0x0E));
        assert!(!is_verify_program_ok(0xFF));
    }
}
