//! Command builders and response parsing for the MiniDSP HID protocol.
//!
//! Each command is a byte sequence that gets wrapped in a frame (see [`crate::packet`]).
//! The first byte identifies the command type.

use crate::dialect::{self, Dialect};

// -- Command opcodes --

pub const CMD_RESET: u8 = 0x03;
pub const CMD_WRITE_FLASH: u8 = 0x04;
pub const CMD_READ_FLASH: u8 = 0x05;
pub const CMD_WRITE_FLASH_BLOCK: u8 = 0x06;
pub const CMD_FINISH_FLASH_BLOCK: u8 = 0x07;
pub const CMD_WRITE_DSP: u8 = 0x13;
pub const CMD_READ_FLOATS: u8 = 0x14;
pub const CMD_SET_MUTE: u8 = 0x17;
pub const CMD_BYPASS_FILTER: u8 = 0x19;
pub const CMD_SET_DISPLAY_BRIGHTNESS: u8 = 0x1A;
pub const CMD_SET_DISPLAY_IDLE_TIME: u8 = 0x1B;
pub const CMD_SET_DRE: u8 = 0x1E;
pub const CMD_SET_CONFIG: u8 = 0x25;
pub const CMD_COPY_PRESET: u8 = 0x27;
pub const CMD_WRITE_BIQUAD: u8 = 0x30;
pub const CMD_LOAD_DSP_PROGRAM: u8 = 0x11;
pub const CMD_BLUETOOTH_AT_CMD: u8 = 0x21;
pub const CMD_RELOAD_E2PROM_HEADER: u8 = 0x24;
pub const CMD_READ_HARDWARE_ID: u8 = 0x31;
pub const CMD_SET_SOURCE: u8 = 0x34;
/// Read the number of available FIR taps for a channel (live readback).
///
/// Wire shape: `[0x39][chan_u8]` → `[0x39][chan][count_u16_be]`.
/// Device Console uses this as a true runtime query: `getNumAvailTaps(ch)` in
/// `streamDevice.js`. Named `FIR_LOAD_START` historically because the same
/// opcode also gates the FIR upload pipeline; the GET_NUM alias makes the
/// live-read use case grep-findable.
pub const CMD_GET_NUM_FIR_TAPS: u8 = 0x39;
pub const CMD_FIR_LOAD_START: u8 = CMD_GET_NUM_FIR_TAPS;
pub const CMD_FIR_LOAD_DATA: u8 = 0x3A;
pub const CMD_RELOAD_DSP_PARAM: u8 = 0x3B;
pub const CMD_WRITE_FLASH_FULL_ADDR: u8 = 0x3C;
pub const CMD_READ_FLASH_FULL_ADDR: u8 = 0x3D;
pub const CMD_ENTER_BOOTLOADER: u8 = 0x3E;
pub const CMD_DIRAC_BYPASS: u8 = 0x3F;
pub const CMD_READ_DSP_VERSION: u8 = 0x40;
pub const CMD_SET_VOLUME: u8 = 0x42;
pub const CMD_DIRAC_DELAY_GAIN: u8 = 0x45;
pub const CMD_READ_DFLASH_ID: u8 = 0x46;
/// Write a 32-byte page into the secondary "D flash" (paged external flash).
///
/// **IMPORTANT** — previously known in this codebase as `CMD_SET_HDMI_SOURCE`
/// from a speculative minidsp-rs label that was never verified. The official
/// Device Console `DspCmdCode` enum (`app/app-extracted/build/src/dspMessage.js`)
/// defines this opcode as `WriteDFlashId`: `[0x49][page_no_u8][32 data bytes]`.
/// It is used by the HDMI/eARC flash pipeline and the preset-program data path,
/// not by audio-source switching. Audio-source switching is `CMD_SET_SOURCE`
/// (`0x34`) for SHARC devices and a sub-command of `CMD_EARC` (`0x53`) for
/// HDA/HTx. If a nanoAVR needs a different source-switch opcode, it's still
/// unknown and `CMD_SET_HDMI_SOURCE` is retained as a deprecated alias so any
/// pre-rename call sites fail over to the (incorrect but historical) 0x49
/// instead of compiling against air.
pub const CMD_WRITE_DFLASH_ID: u8 = 0x49;
#[deprecated(
    since = "0.2.0",
    note = "0x49 is WriteDFlashId per Device Console — NOT an HDMI source switch. \
            Use CMD_WRITE_DFLASH_ID or CMD_EARC as appropriate."
)]
pub const CMD_SET_HDMI_SOURCE: u8 = CMD_WRITE_DFLASH_ID;
pub const CMD_ERASE_FLASH: u8 = 0x50;
pub const CMD_COM_FW_UPGRADE: u8 = 0x52;
pub const CMD_EARC: u8 = 0x53;
pub const CMD_LOG_CONTROL: u8 = 0x54;
pub const CMD_WISA: u8 = 0xF0;

/// Official Device Console uses three distinct reset-mode bytes on
/// `ChangePreset (0x25)`:
///
/// - `0x00` (`PRESET_CHANGE_WITHOUT_RESET`): converter-flow change used only
///   during preset import to avoid double-resetting when the DSP is already
///   mid-import.
/// - `0x01` (`PRESET_CHANGE_RELOAD_SAME_SLOT`): re-select the currently active
///   slot — reload its state from flash into live RAM without the full DSP
///   reset cycle. Used after an XML import when the user re-clicks the already
///   active slot to force a re-load from the new cached file.
/// - `0x02` (`PRESET_CHANGE_NORMAL`): normal interactive change — full DSP
///   reset and post-preset verify cycle.
///
/// The official `index.js` chooses between `0x01` and `0x02` at runtime via
/// `activePresetIdx === slot ? 0x01 : 0x02`. `0x00` is only used by the
/// converter/importer path.
pub const PRESET_CHANGE_WITHOUT_RESET: u8 = 0x00;
pub const PRESET_CHANGE_RELOAD_SAME_SLOT: u8 = 0x01;
pub const PRESET_CHANGE_NORMAL: u8 = 0x02;
pub const FLASH_FULL_ADDR_CHUNK_MAX: u8 = 0x3A;

// -- Response types --

pub const RESP_ERROR: u8 = 0x00;
pub const RESP_OK: u8 = 0x01;
pub const RESP_EVENT: u8 = 0x02;
pub const RESP_RESET_COMPLETE: u8 = 0xAA;
pub const RESP_PRESET_COMPLETE: u8 = 0xAB;
pub const RESP_OVERFLOW: u8 = 0xFF;

/// Returns true if the byte is a known MiniDSP command or response opcode.
///
/// Used by the HID decoder to recognize `0xFF`-prefixed unframed packets
/// without maintaining a separate hand-coded allowlist. This set must be the
/// union of every opcode we RECOGNIZE (not necessarily every one we actively
/// implement) so the TCP bridge can forward advanced Device Console commands
/// (bootloader entry, firmware-update blocks, eARC sub-commands, WiSA
/// sub-commands, etc.) without stripping them as "unknown".
pub const fn is_known_opcode(byte: u8) -> bool {
    matches!(
        byte,
        CMD_RESET
            | CMD_WRITE_FLASH
            | CMD_READ_FLASH
            | CMD_WRITE_FLASH_BLOCK
            | CMD_FINISH_FLASH_BLOCK
            | CMD_LOAD_DSP_PROGRAM
            | CMD_WRITE_DSP
            | CMD_READ_FLOATS
            | CMD_SET_MUTE
            | CMD_BYPASS_FILTER
            | CMD_SET_DISPLAY_BRIGHTNESS
            | CMD_SET_DISPLAY_IDLE_TIME
            | CMD_SET_DRE
            | CMD_BLUETOOTH_AT_CMD
            | CMD_RELOAD_E2PROM_HEADER
            | CMD_SET_CONFIG
            | CMD_COPY_PRESET
            | CMD_WRITE_BIQUAD
            | CMD_READ_HARDWARE_ID
            | CMD_SET_SOURCE
            | CMD_GET_NUM_FIR_TAPS
            | CMD_FIR_LOAD_DATA
            | CMD_RELOAD_DSP_PARAM
            | CMD_WRITE_FLASH_FULL_ADDR
            | CMD_READ_FLASH_FULL_ADDR
            | CMD_ENTER_BOOTLOADER
            | CMD_DIRAC_BYPASS
            | CMD_READ_DSP_VERSION
            | CMD_SET_VOLUME
            | CMD_DIRAC_DELAY_GAIN
            | CMD_READ_DFLASH_ID
            | CMD_WRITE_DFLASH_ID
            | CMD_ERASE_FLASH
            | CMD_COM_FW_UPGRADE
            | CMD_EARC
            | CMD_LOG_CONTROL
            | CMD_WISA
            | RESP_OK
            | RESP_EVENT
            | RESP_RESET_COMPLETE
            | RESP_PRESET_COMPLETE
    )
}

/// Returns true if the opcode is a write/ack command that produces an unframed
/// echo response starting with `0x01` on real DSP100 / 2x4 HD hardware.
///
/// This is used to trim the garbage tail from 64-byte HID echo responses before
/// forwarding to TCP clients (official Device Console, minidsp-rs). The set
/// exactly matches the `Some(0x01)` arm in `packet::decode_hid_response`.
pub const fn is_write_command(opcode: u8) -> bool {
    matches!(
        opcode,
        CMD_WRITE_FLASH
            | CMD_WRITE_FLASH_BLOCK
            | CMD_FINISH_FLASH_BLOCK
            | CMD_WRITE_FLASH_FULL_ADDR
            | CMD_WRITE_DSP
            | CMD_WRITE_BIQUAD
            | CMD_SET_VOLUME
            | CMD_SET_MUTE
            | CMD_SET_SOURCE
            | CMD_SET_CONFIG
            | CMD_BYPASS_FILTER
            | CMD_DIRAC_BYPASS
            | CMD_SET_DISPLAY_BRIGHTNESS
            | CMD_SET_DISPLAY_IDLE_TIME
            | CMD_SET_DRE
            | CMD_COPY_PRESET
            | CMD_FIR_LOAD_DATA
            | CMD_RELOAD_DSP_PARAM
            | CMD_LOG_CONTROL
            | CMD_WRITE_DFLASH_ID
    )
}

/// Parsed hardware identity from ReadHardwareId response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HardwareId {
    pub fw_major: u8,
    pub fw_minor: u8,
    pub hw_id: u8,
}

/// Parsed response from the device.
#[derive(Debug, Clone, PartialEq)]
pub enum Response {
    /// Command acknowledged.
    Ack,
    /// Error response.
    Error(Vec<u8>),
    /// Hardware ID response.
    HardwareId(HardwareId),
    /// Float values read from DSP memory.
    Floats(Vec<f32>),
    /// Raw response bytes (for passthrough in TCP bridge without re-encoding).
    Raw(Vec<u8>),
    /// Flash data read.
    FlashData(Vec<u8>),
    /// Reset complete (0xAA).
    ResetComplete,
    /// Preset change complete (0xAB).
    PresetComplete,
    /// Buffer overflow / fatal (0xFF).
    Overflow,
    /// FIR load start response (max coefficient count).
    FirLoadInfo { max_coeffs: u16 },
    /// Unknown response (raw bytes).
    Unknown(Vec<u8>),
}

// -- Command builders --
// Each returns the payload bytes to be wrapped in a frame.

/// Build a ReadHardwareId command.
pub fn read_hardware_id() -> Vec<u8> {
    vec![CMD_READ_HARDWARE_ID]
}

/// Build a SetVolume command. `value` is the raw byte sent to the device.
/// Gain in dB = -0.5 × value. So value=0 → 0dB, value=20 → -10dB, value=255 → -127.5dB.
pub fn set_volume(value: u8) -> Vec<u8> {
    vec![CMD_SET_VOLUME, value]
}

/// Build a SetMute command.
pub fn set_mute(muted: bool) -> Vec<u8> {
    vec![CMD_SET_MUTE, if muted { 0x01 } else { 0x00 }]
}

/// Build a SetConfig (preset switch) command.
pub fn set_config(preset: u8, reset: u8) -> Vec<u8> {
    vec![CMD_SET_CONFIG, preset, reset]
}

/// Build a SetSource command.
pub fn set_source(source: u8) -> Vec<u8> {
    vec![CMD_SET_SOURCE, source]
}

/// Build a DiracBypass command. 0 = Dirac enabled, 1 = bypassed.
pub fn dirac_bypass(bypass: bool) -> Vec<u8> {
    vec![CMD_DIRAC_BYPASS, if bypass { 0x01 } else { 0x00 }]
}

/// Build a Reset command.
pub fn reset() -> Vec<u8> {
    vec![CMD_RESET]
}

/// Build a ReadFlash command.
pub fn read_flash(addr_hi: u8, addr_lo: u8, count: u8) -> Vec<u8> {
    vec![CMD_READ_FLASH, addr_hi, addr_lo, count]
}

/// Build a WriteFlash command.
pub fn write_flash(addr_hi: u8, addr_lo: u8, data: &[u8]) -> Vec<u8> {
    let mut cmd = vec![CMD_WRITE_FLASH, addr_hi, addr_lo];
    cmd.extend_from_slice(data);
    cmd
}

/// Build the official app's ReadFlashFullAddr command.
pub fn read_flash_full_addr(addr: u32, count: u8) -> Vec<u8> {
    let addr = addr.to_be_bytes();
    vec![
        CMD_READ_FLASH_FULL_ADDR,
        addr[1],
        addr[2],
        addr[3],
        count.min(FLASH_FULL_ADDR_CHUNK_MAX),
    ]
}

/// Build the official app's WriteFlashFullAddr command.
pub fn write_flash_full_addr(addr: u32, data: &[u8], offset: usize) -> Vec<u8> {
    let addr_bytes = addr.to_be_bytes();
    let size = FLASH_FULL_ADDR_CHUNK_MAX as usize;
    let end = data.len().min(offset.saturating_add(size));
    let mut cmd = vec![
        CMD_WRITE_FLASH_FULL_ADDR,
        addr_bytes[1],
        addr_bytes[2],
        addr_bytes[3],
    ];
    if offset < end {
        cmd.extend_from_slice(&data[offset..end]);
    }
    cmd
}

/// Build the official app's WriteFlashBlock command.
///
/// Device Console uses this during preset-save persistence. The first chunk of
/// a block uses the plain block id; continuation chunks set bit 7 on that id.
/// Payloads are limited to 32 bytes per chunk.
pub fn write_flash_block(block_id: u8, data: &[u8], begin_block: bool) -> Vec<u8> {
    let mut cmd = vec![
        CMD_WRITE_FLASH_BLOCK,
        if begin_block {
            block_id
        } else {
            block_id | 0x80
        },
    ];
    cmd.extend_from_slice(&data[..data.len().min(0x20)]);
    cmd
}

/// Build the official app's FinishFlashBlock command.
pub fn finish_flash_block(block_id: u8) -> Vec<u8> {
    vec![CMD_FINISH_FLASH_BLOCK, block_id]
}

/// Build a DSP version query command.
pub fn read_dsp_version() -> Vec<u8> {
    vec![CMD_READ_DSP_VERSION]
}

/// Build a WriteDSP command.
///
/// `save`: true = 0x80 (persist to flash), false = 0xA0 (RAM only).
pub fn write_dsp(addr: u16, value: f32, save: bool, dialect: &Dialect) -> Vec<u8> {
    let mut cmd = vec![CMD_WRITE_DSP];
    cmd.push(if save { 0x80 } else { 0xA0 });
    if dialect.value_format == crate::dialect::ValueFormat::Float32 {
        cmd.extend_from_slice(&[(addr >> 8) as u8, addr as u8]);
    } else {
        cmd.extend_from_slice(&dialect::encode_addr(addr, true, dialect));
    }
    cmd.extend_from_slice(&dialect::encode_value(value, dialect));
    cmd
}

/// Build a WriteDSP command with a raw 32-bit scalar payload.
///
/// This is the plain scalar `WriteDSP` shape used for modern slots that do
/// not need the official app's linked-int wrapper.
pub fn write_dsp_u32(addr: u16, value: u32, save: bool) -> Vec<u8> {
    let bytes = value.to_be_bytes();
    vec![
        CMD_WRITE_DSP,
        if save { 0x80 } else { 0xA0 },
        (addr >> 8) as u8,
        addr as u8,
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
    ]
}

/// Build a delay-slot `WriteDSP` command using the device's native integer
/// encoding.
///
/// Modern SHARC devices use a 16-bit little-endian sample count padded to
/// 32 bits. Legacy ADAU devices use a raw 32-bit big-endian sample count.
pub fn write_dsp_delay(addr: u16, samples: u32, save: bool, dialect: &Dialect) -> Vec<u8> {
    let mut cmd = vec![CMD_WRITE_DSP, if save { 0x80 } else { 0xA0 }];
    match dialect.value_format {
        crate::dialect::ValueFormat::Float32 => {
            let samples = samples.min(u16::MAX as u32) as u16;
            cmd.extend_from_slice(&[(addr >> 8) as u8, addr as u8]);
            cmd.extend_from_slice(&samples.to_le_bytes());
            cmd.extend_from_slice(&[0x00, 0x00]);
        }
        crate::dialect::ValueFormat::FixedPoint => {
            cmd.extend_from_slice(&dialect::encode_addr(addr, true, dialect));
            cmd.extend_from_slice(&samples.to_be_bytes());
        }
    }
    cmd
}

/// Build a WriteDSP command with a raw 32-bit scalar payload encoded
/// little-endian and an explicit control byte.
///
/// DSP100 mixer selector/status slots match the official app's `LoadDspParam`
/// shape: opcode `0x13`, control `0x80`, address big-endian, value
/// little-endian.
pub fn write_dsp_u32_le_with_ctrl(addr: u16, value: u32, ctrl: u8) -> Vec<u8> {
    let bytes = value.to_le_bytes();
    vec![
        CMD_WRITE_DSP,
        ctrl,
        (addr >> 8) as u8,
        addr as u8,
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
    ]
}

/// Build a modern integer-status `WriteDSP` command.
///
/// This matches the working DSP100 / 2x4 HD path used by `minidsp-rs` for
/// mute, routing enable, FIR bypass, and compressor bypass style selector
/// slots: `0x13`, control/address prefix, then little-endian `u16` padded to
/// 32 bits.
pub fn write_dsp_int(addr: u16, value: u16) -> Vec<u8> {
    write_dsp_u32_le_with_ctrl(addr, value as u32, 0x80)
}

/// Build the official app's linked-int `0x30` payload shape.
///
/// Modern mute and mixer-status slots on SHARC/XMOS devices use the official
/// app's `GetSetLinkedIntValCmd`, which multiplexes opcode `0x30` with a
/// non-biquad payload:
///
/// - control byte = `0x80 + linked_addr_count`
/// - target address = big-endian u16
/// - linked addresses = big-endian u16 list, or a single zero placeholder
/// - scalar value = little-endian u32
pub fn write_dsp_linked_u32(addr: u16, value: u32, linked_addrs: &[u16]) -> Vec<u8> {
    let linked_count = linked_addrs.len().min(0x7f);
    let mut cmd = Vec::with_capacity(
        1 + 1
            + 2
            + if linked_count == 0 {
                2
            } else {
                linked_count * 2
            }
            + 4,
    );
    cmd.push(CMD_WRITE_BIQUAD);
    cmd.push(0x80 + linked_count as u8);
    cmd.extend_from_slice(&addr.to_be_bytes());
    if linked_count == 0 {
        cmd.extend_from_slice(&0u16.to_be_bytes());
    } else {
        for &linked in linked_addrs.iter().take(linked_count) {
            cmd.extend_from_slice(&linked.to_be_bytes());
        }
    }
    cmd.extend_from_slice(&value.to_le_bytes());
    cmd
}

/// Build a ReadFloats command. Reads `count` float values starting at `addr`.
/// Maximum count is 14 per request.
pub fn read_floats(addr: u16, count: u8, dialect: &Dialect) -> Vec<u8> {
    let _ = dialect;
    vec![
        CMD_READ_FLOATS,
        (addr >> 8) as u8,
        addr as u8,
        count.min(14),
    ]
}

/// Build a WriteBiquad command. Writes 5 biquad coefficients at the given address.
pub fn write_biquad(addr: u16, coeffs: &[f32; 5], dialect: &Dialect) -> Vec<u8> {
    let mut cmd = vec![CMD_WRITE_BIQUAD, 0x80];
    if dialect.value_format == crate::dialect::ValueFormat::Float32 {
        cmd.extend_from_slice(&[(addr >> 8) as u8, addr as u8]);
        cmd.extend_from_slice(&[0x00, 0x00]);
    } else {
        cmd.extend_from_slice(&dialect::encode_addr(addr, true, dialect));
        cmd.extend_from_slice(&[0x00, 0x00]); // padding
    }
    for &c in coeffs {
        cmd.extend_from_slice(&dialect::encode_value(c, dialect));
    }
    cmd
}

/// Build a BypassFilter command.
pub fn bypass_filter(addr: u16, bypass: bool, dialect: &Dialect) -> Vec<u8> {
    let mut cmd = vec![CMD_BYPASS_FILTER];
    cmd.push(if bypass { 0x80 } else { 0x00 });
    if dialect.value_format == crate::dialect::ValueFormat::Float32 {
        cmd.extend_from_slice(&[(addr >> 8) as u8, addr as u8, 0x00, 0x00]);
    } else {
        cmd.extend_from_slice(&dialect::encode_addr(addr, true, dialect));
    }
    cmd
}

/// Build a SetDisplayBrightness command. level: 1-8.
pub fn set_display_brightness(level: u8) -> Vec<u8> {
    vec![CMD_SET_DISPLAY_BRIGHTNESS, level]
}

/// Build a SetDisplayIdleTime command. timeout in seconds.
pub fn set_display_idle_time(timeout_secs: u8) -> Vec<u8> {
    vec![CMD_SET_DISPLAY_IDLE_TIME, timeout_secs]
}

/// Build a SetDRE command. enabled: true = on, false = off.
pub fn set_dre(enabled: bool) -> Vec<u8> {
    vec![CMD_SET_DRE, 0x00, if enabled { 0x01 } else { 0x00 }]
}

/// Build a CopyPreset command. sync: true = synchronous (0x00), false = async.
pub fn copy_preset(sync: bool) -> Vec<u8> {
    if sync {
        vec![CMD_COPY_PRESET, 0x00]
    } else {
        vec![CMD_COPY_PRESET]
    }
}

/// Build a FirLoadStart command. Returns max coefficient count.
pub fn fir_load_start(index: u8) -> Vec<u8> {
    vec![CMD_FIR_LOAD_START, index]
}

/// Build a FirLoadData command. Up to 14 float32 coefficients per packet.
pub fn fir_load_data(index: u8, coeffs: &[f32]) -> Vec<u8> {
    let mut cmd = vec![CMD_FIR_LOAD_DATA, index];
    for &c in &coeffs[..coeffs.len().min(14)] {
        cmd.extend_from_slice(&c.to_le_bytes());
    }
    cmd
}

/// Build a FirLoadEnd command.
///
/// The device uses the same one-byte opcode shape as ReloadDspParam.
pub fn fir_load_end() -> Vec<u8> {
    vec![CMD_RELOAD_DSP_PARAM]
}

/// Build a ReloadDspParam command.
pub fn reload_dsp_param() -> Vec<u8> {
    vec![CMD_RELOAD_DSP_PARAM]
}

/// Build a DiracDelayGain read command.
pub fn dirac_delay_gain(channel_bitmask: u8) -> Vec<u8> {
    vec![CMD_DIRAC_DELAY_GAIN, 0xA1, channel_bitmask]
}

/// Build a LogControl command.
pub fn log_control(enable: bool) -> Vec<u8> {
    vec![CMD_LOG_CONTROL, if enable { 0x01 } else { 0x00 }]
}

/// Build a `GetNumFirTaps (0x39)` request for one channel.
///
/// Device Console uses this as a LIVE readback for the currently available
/// FIR tap count on a given output (or input, depending on family FIR
/// topology). The response format is `[0x39][chan][count_u16_be]`; use
/// [`parse_fir_tap_count_response`] to decode it.
pub fn get_num_fir_taps(channel: u8) -> Vec<u8> {
    vec![CMD_GET_NUM_FIR_TAPS, channel]
}

/// Parse the response to `get_num_fir_taps`. Returns `Some((chan, count))`
/// when the frame matches `[0x39][chan][count_u16_be]` and `None` otherwise.
pub fn parse_fir_tap_count_response(payload: &[u8]) -> Option<(u8, u16)> {
    if payload.len() >= 4 && payload[0] == CMD_GET_NUM_FIR_TAPS {
        let chan = payload[1];
        let count = u16::from_be_bytes([payload[2], payload[3]]);
        Some((chan, count))
    } else {
        None
    }
}

/// Build a write-to-D-flash command (`0x49`).
///
/// **This is not** an HDMI source-switch command. See [`CMD_WRITE_DFLASH_ID`]
/// for the reversal-of-fortune note. Wire shape is `[0x49][page_u8][32 data bytes]`
/// per Device Console `dspMessage.js:WriteDFlashIdDspCmd`. Callers must
/// supply exactly 32 bytes of data or the device will pad with garbage.
pub fn write_dflash_id(page: u8, data: [u8; 32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(34);
    out.push(CMD_WRITE_DFLASH_ID);
    out.push(page);
    out.extend_from_slice(&data);
    out
}

/// Build a read-from-D-flash request (`0x46`). Reads 32 bytes of page `page`.
/// Response format: `[0x46][page][32 data bytes]`.
pub fn read_dflash_id(page: u8) -> Vec<u8> {
    vec![CMD_READ_DFLASH_ID, page]
}

// -- Response parsing --

/// Parse a response payload (already decoded from frame).
pub fn parse_response(data: &[u8], dialect: &Dialect) -> Response {
    if data.is_empty() {
        return Response::Error(vec![]);
    }

    match data[0] {
        RESP_OVERFLOW => Response::Overflow,
        RESP_RESET_COMPLETE => Response::ResetComplete,
        RESP_PRESET_COMPLETE => Response::PresetComplete,
        RESP_ERROR => Response::Error(data[1..].to_vec()),

        CMD_READ_HARDWARE_ID => {
            if data.len() >= 4 {
                Response::HardwareId(HardwareId {
                    fw_major: data[1],
                    fw_minor: data[2],
                    hw_id: data[3],
                })
            } else {
                Response::Unknown(data.to_vec())
            }
        }

        CMD_READ_FLOATS => parse_floats_response(data, dialect),

        CMD_READ_FLASH => Response::FlashData(data[1..].to_vec()),

        CMD_READ_FLASH_FULL_ADDR => {
            if data.len() >= 4 {
                Response::FlashData(data[4..].to_vec())
            } else {
                Response::Unknown(data.to_vec())
            }
        }

        CMD_FIR_LOAD_START => {
            if data.len() >= 3 {
                let max_coeffs = u16::from_be_bytes([data[1], data[2]]);
                Response::FirLoadInfo { max_coeffs }
            } else {
                Response::Unknown(data.to_vec())
            }
        }

        // Ack-like responses for various commands
        CMD_WRITE_FLASH
        | CMD_WRITE_FLASH_BLOCK
        | CMD_FINISH_FLASH_BLOCK
        | CMD_WRITE_FLASH_FULL_ADDR
        | CMD_WRITE_DSP
        | CMD_WRITE_BIQUAD
        | CMD_SET_VOLUME
        | CMD_SET_MUTE
        | CMD_SET_SOURCE
        | CMD_SET_CONFIG
        | CMD_BYPASS_FILTER
        | CMD_DIRAC_BYPASS
        | CMD_SET_DISPLAY_BRIGHTNESS
        | CMD_SET_DISPLAY_IDLE_TIME
        | CMD_SET_DRE
        | CMD_COPY_PRESET
        | CMD_FIR_LOAD_DATA
        | CMD_RELOAD_DSP_PARAM
        | CMD_LOG_CONTROL
        | CMD_WRITE_DFLASH_ID
        | RESP_OK => Response::Ack,

        _ => Response::Unknown(data.to_vec()),
    }
}

/// Parse a response payload with access to the original request payload.
///
/// Real MiniDSP hardware may echo the flash address bytes in `ReadFlash`
/// responses (`[0x05, addr_hi, addr_lo, data...]`), while the simulator and
/// older assumptions use `[0x05, data...]`. This helper accepts both forms.
pub fn parse_response_for_request(request: &[u8], data: &[u8], dialect: &Dialect) -> Response {
    if request.first() == Some(&CMD_SET_CONFIG)
        && data.first() == Some(&RESP_EVENT)
        && request.len() >= 3
        && data.len() >= 3
        && data[1] == request[1]
        && data[2] == request[2]
    {
        return Response::PresetComplete;
    }

    if request.first() == Some(&CMD_READ_FLASH_FULL_ADDR)
        && data.first() == Some(&CMD_READ_FLASH_FULL_ADDR)
    {
        if request.len() >= 5 && data.len() >= 5 {
            let count = request[4] as usize;
            let body = &data[1..];

            if body.len() >= count + 3 && body[..3] == request[1..4] {
                return Response::FlashData(body[3..3 + count].to_vec());
            }
        }

        return Response::Unknown(data.to_vec());
    }

    if request.first() == Some(&CMD_READ_FLASH) && data.first() == Some(&CMD_READ_FLASH) {
        if request.len() >= 4 && data.len() >= 2 {
            let addr_hi = request[1];
            let addr_lo = request[2];
            let count = request[3] as usize;
            let body = &data[1..];

            if body.len() >= count + 2 && body[0] == addr_hi && body[1] == addr_lo {
                return Response::FlashData(body[2..2 + count].to_vec());
            }

            if body.len() == count {
                return Response::FlashData(body[..count].to_vec());
            }

            return Response::Unknown(data.to_vec());
        }
    }

    if request.first() == Some(&CMD_READ_FLOATS) && data.first() == Some(&CMD_READ_FLOATS) {
        if request.len() >= 4
            && data.len() >= 5
            && data[1] == request[1]
            && data[2] == request[2]
            && data[3] == 0x00
            && data[4] == request[3]
        {
            let count = request[3] as usize;
            let remaining = &data[5..];

            if remaining.len() > count.saturating_mul(4) + 8
                && remaining.len() >= 6
                && remaining[..6] == [0, 0, 0, 0, 0, 0]
            {
                return Response::Unknown(data.to_vec());
            }

            if let Some(values) = decode_float_block_with_offset(remaining, count, dialect, request)
            {
                return Response::Floats(values);
            }

            return Response::Unknown(data.to_vec());
        }

        if data.starts_with(request) && request.len() < data.len() {
            let count = request.last().copied().unwrap_or(0) as usize;
            let remaining = &data[request.len()..];
            let needed = count.saturating_mul(4);

            if remaining.len() > needed + 8
                && remaining.len() >= 6
                && remaining[..6] == [0, 0, 0, 0, 0, 0]
            {
                return Response::Unknown(data.to_vec());
            }

            if let Some(values) = decode_float_block_with_offset(remaining, count, dialect, request)
            {
                return Response::Floats(values);
            }

            return Response::Unknown(data.to_vec());
        }

        if request.len() >= 4
            && data.len() >= 4
            && data[1] == request[1]
            && data[2] == request[2]
            && data[3] == request[3]
        {
            let count = request[3] as usize;
            let needed = count.saturating_mul(4);
            if data.len() >= 4 + needed {
                return Response::Floats(decode_float_chunks(&data[4..4 + needed], count, dialect));
            }

            return Response::Unknown(data.to_vec());
        }

        if request.len() >= 4 && data.len() >= 3 && data[1] == request[1] && data[2] == request[2] {
            let count = request[3] as usize;
            let remaining = &data[3..];

            if let Some(values) = decode_float_block_with_offset(remaining, count, dialect, request)
            {
                return Response::Floats(values);
            }

            return Response::Unknown(data.to_vec());
        }

        return Response::Unknown(data.to_vec());
    }

    parse_response(data, dialect)
}

/// Parse a ReadFloats response into float values.
///
/// Upstream `minidsp-rs` treats these as `[0x14, addr_hi, addr_lo, float_data...]`.
/// Some local simulator paths have also emitted `[0x14, float_data...]`, so keep a
/// fallback for that older assumption.
fn parse_floats_response(data: &[u8], dialect: &Dialect) -> Response {
    if data.len() <= 1 {
        return Response::Floats(vec![]);
    }

    if data.len() >= 3 {
        let float_data = &data[3..];
        if float_data.len() % 4 == 0 {
            let floats = decode_float_chunks(float_data, float_data.len() / 4, dialect);
            return Response::Floats(floats);
        }
    }

    let float_data = &data[1..];
    let floats = decode_float_chunks(float_data, float_data.len() / 4, dialect);
    Response::Floats(floats)
}

fn decode_float_chunks(data: &[u8], count: usize, dialect: &Dialect) -> Vec<f32> {
    let mut floats = Vec::new();
    for chunk in data.chunks(4).take(count) {
        if chunk.len() == 4 {
            let bytes: [u8; 4] = [chunk[0], chunk[1], chunk[2], chunk[3]];
            floats.push(dialect::decode_value(&bytes, dialect));
        }
    }
    floats
}

fn decode_word_chunks(data: &[u8], count: usize) -> Vec<u32> {
    let mut words = Vec::new();
    for chunk in data.chunks(4).take(count) {
        if chunk.len() == 4 {
            words.push(u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
        }
    }
    words
}

fn decode_word_chunks_be(data: &[u8], count: usize) -> Vec<u32> {
    let mut words = Vec::new();
    for chunk in data.chunks(4).take(count) {
        if chunk.len() == 4 {
            words.push(u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
        }
    }
    words
}

fn parse_param_words_response_impl(
    request: &[u8],
    data: &[u8],
    decode_words: fn(&[u8], usize) -> Vec<u32>,
) -> Option<Vec<u32>> {
    if request.first().copied() != Some(CMD_READ_FLOATS) {
        return None;
    }

    let count = request.last().copied().unwrap_or(0) as usize;
    if count == 0 {
        return Some(vec![]);
    }

    let needed = count * 4;

    if data.len() >= 5
        && data.first().copied() == Some(CMD_READ_FLOATS)
        && request.len() >= 4
        && data.get(1) == request.get(1)
        && data.get(2) == request.get(2)
        && data.get(3).copied() == Some(0x00)
        && data.get(4) == request.get(3)
        && data.len() >= 5 + needed
    {
        return Some(decode_words(&data[5..5 + needed], count));
    }

    if data.len() >= 4
        && data.first().copied() == Some(CMD_READ_FLOATS)
        && request.len() >= 4
        && data.get(1) == request.get(1)
        && data.get(2) == request.get(2)
        && data.get(3) == request.get(3)
        && data.len() >= 4 + needed
    {
        return Some(decode_words(&data[4..4 + needed], count));
    }

    if data.len() >= 3
        && data.first().copied() == Some(CMD_READ_FLOATS)
        && request.len() >= 3
        && data.get(1) == request.get(1)
        && data.get(2) == request.get(2)
        && data.len() >= 3 + needed
    {
        return Some(decode_words(&data[3..3 + needed], count));
    }

    if data.starts_with(request) && data.len() >= request.len() + needed {
        return Some(decode_words(
            &data[request.len()..request.len() + needed],
            count,
        ));
    }

    None
}

/// Parse a raw `ReadFloats` reply into typed 32-bit words instead of float32.
pub fn parse_param_words_response_for_request(request: &[u8], data: &[u8]) -> Option<Vec<u32>> {
    parse_param_words_response_impl(request, data, decode_word_chunks)
}

/// Parse a raw `ReadFloats` reply into big-endian typed 32-bit words.
pub fn parse_param_words_be_response_for_request(request: &[u8], data: &[u8]) -> Option<Vec<u32>> {
    parse_param_words_response_impl(request, data, decode_word_chunks_be)
}

fn decode_float_block_with_offset(
    data: &[u8],
    count: usize,
    dialect: &Dialect,
    request: &[u8],
) -> Option<Vec<f32>> {
    if count == 0 {
        return Some(vec![]);
    }
    let needed = count * 4;
    if data.len() < needed {
        return None;
    }

    let mut best: Option<(usize, usize, Vec<f32>)> = None;

    for offset in 0..=data.len().saturating_sub(needed) {
        let values = decode_float_chunks(&data[offset..offset + needed], count, dialect);
        if values.len() != count {
            continue;
        }

        if let Some(score) = score_float_candidate(&values, offset, data, request) {
            let replace = match &best {
                Some((best_score, best_offset, _)) => {
                    score > *best_score || (score == *best_score && offset < *best_offset)
                }
                None => true,
            };
            if replace {
                best = Some((score, offset, values));
            }
        }
    }

    best.map(|(_, _, values)| values)
}

fn score_float_candidate(
    values: &[f32],
    offset: usize,
    raw_data: &[u8],
    request: &[u8],
) -> Option<usize> {
    let mut score = 0usize;
    let mut zero_count = 0usize;

    for value in values {
        if !value.is_finite() {
            return None;
        }

        let abs = value.abs();
        if abs == 0.0 {
            zero_count += 1;
            score += 5;
        } else if abs < 1e-8 {
            return None;
        } else if !value.is_normal() {
            return None;
        } else if abs <= 10.0 {
            score += 4;
        } else if abs <= 1000.0 {
            score += 3;
        } else if abs <= 50_000.0 {
            score += 1;
        } else {
            return None;
        }
    }

    if values.len() == 1 && zero_count == 1 {
        if raw_data.len() >= 2
            && raw_data[0] == 0x16
            && raw_data[1] == 0x01
            && raw_data.len() > 6
            && offset <= 4
            && request.first() == Some(&CMD_READ_FLOATS)
        {
            return None;
        }
    }

    // Prefer immediately aligned float blocks when multiple candidate decodes
    // look plausible. This keeps direct count-echo replies from being
    // outscored by stray trailer bytes while still allowing later offsets when
    // the leading bytes clearly are not float payload.
    if offset == 0 {
        score += 3;
    } else if offset % 4 == 0 {
        score += 1;
    }

    Some(score)
}

/// Check if a command opcode expects a specific response type.
pub fn matches_response(cmd_opcode: u8, resp_opcode: u8) -> bool {
    if resp_opcode == RESP_ERROR || resp_opcode == RESP_OVERFLOW {
        return true; // errors match everything
    }
    match cmd_opcode {
        CMD_READ_HARDWARE_ID => resp_opcode == CMD_READ_HARDWARE_ID,
        CMD_READ_FLOATS => resp_opcode == CMD_READ_FLOATS,
        CMD_READ_FLASH => resp_opcode == CMD_READ_FLASH,
        CMD_READ_FLASH_FULL_ADDR => resp_opcode == CMD_READ_FLASH_FULL_ADDR,
        CMD_SET_CONFIG => {
            resp_opcode == CMD_SET_CONFIG
                || resp_opcode == RESP_PRESET_COMPLETE
                || resp_opcode == RESP_EVENT
        }
        CMD_RESET => resp_opcode == RESP_RESET_COMPLETE,
        CMD_FIR_LOAD_START => resp_opcode == CMD_FIR_LOAD_START,
        _ => {
            // Most write commands just return an ack with their own opcode
            resp_opcode == cmd_opcode || resp_opcode == RESP_OK
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::Dialect;

    #[test]
    fn read_hardware_id_cmd() {
        let cmd = read_hardware_id();
        assert_eq!(cmd, vec![0x31]);
    }

    #[test]
    fn set_volume_cmd() {
        let cmd = set_volume(20); // -10dB
        assert_eq!(cmd, vec![0x42, 20]);
    }

    #[test]
    fn set_mute_cmd() {
        assert_eq!(set_mute(true), vec![0x17, 0x01]);
        assert_eq!(set_mute(false), vec![0x17, 0x00]);
    }

    #[test]
    fn set_config_cmd() {
        let cmd = set_config(2, PRESET_CHANGE_NORMAL);
        assert_eq!(cmd, vec![0x25, 0x02, 0x02]);
    }

    #[test]
    fn write_dsp_new_dialect() {
        let cmd = write_dsp(0x0042, 1.0, true, &Dialect::NEW);
        assert_eq!(cmd[0], CMD_WRITE_DSP);
        assert_eq!(cmd[1], 0x80); // save flag
        assert_eq!(cmd[2], 0x00); // addr hi
        assert_eq!(cmd[3], 0x42); // addr lo
        let value_bytes = &cmd[4..8];
        assert_eq!(value_bytes, &1.0f32.to_le_bytes());
    }

    #[test]
    fn write_dsp_u32_new_dialect() {
        let cmd = write_dsp_u32(0x0042, 2, true);
        assert_eq!(
            cmd,
            vec![CMD_WRITE_DSP, 0x80, 0x00, 0x42, 0x00, 0x00, 0x00, 0x02]
        );
    }

    #[test]
    fn write_dsp_delay_new_dialect() {
        let cmd = write_dsp_delay(0x0042, 2, false, &Dialect::NEW);
        assert_eq!(
            cmd,
            vec![CMD_WRITE_DSP, 0xA0, 0x00, 0x42, 0x02, 0x00, 0x00, 0x00]
        );
    }

    #[test]
    fn write_dsp_delay_old_dialect() {
        let cmd = write_dsp_delay(0x0042, 2, true, &Dialect::OLD);
        assert_eq!(
            cmd,
            vec![CMD_WRITE_DSP, 0x80, 0x80, 0x42, 0x00, 0x00, 0x00, 0x02]
        );
    }

    #[test]
    fn write_dsp_int_new_dialect() {
        let cmd = write_dsp_int(0x0006, 2);
        assert_eq!(
            cmd,
            vec![CMD_WRITE_DSP, 0x80, 0x00, 0x06, 0x02, 0x00, 0x00, 0x00]
        );
    }

    #[test]
    fn write_flash_block_begin_chunk() {
        let cmd = write_flash_block(0x02, &[0x05, 0x00, 0xcf], true);
        assert_eq!(cmd, vec![CMD_WRITE_FLASH_BLOCK, 0x02, 0x05, 0x00, 0xcf]);
    }

    #[test]
    fn read_flash_full_addr_cmd() {
        assert_eq!(
            read_flash_full_addr(0x00ffc8, 0x40),
            vec![CMD_READ_FLASH_FULL_ADDR, 0x00, 0xff, 0xc8, 0x3a]
        );
    }

    #[test]
    fn write_flash_full_addr_cmd() {
        let cmd = write_flash_full_addr(0x0c0000, &[0x11, 0x22, 0x33, 0x44], 1);
        assert_eq!(
            cmd,
            vec![
                CMD_WRITE_FLASH_FULL_ADDR,
                0x0c,
                0x00,
                0x00,
                0x22,
                0x33,
                0x44
            ]
        );
    }

    #[test]
    fn write_flash_block_continuation_chunk_is_capped() {
        let data = [0x11; 0x21];
        let cmd = write_flash_block(0x02, &data, false);
        assert_eq!(cmd[0], CMD_WRITE_FLASH_BLOCK);
        assert_eq!(cmd[1], 0x82);
        assert_eq!(cmd.len(), 34);
        assert!(cmd[2..].iter().all(|&byte| byte == 0x11));
    }

    #[test]
    fn finish_flash_block_cmd() {
        assert_eq!(finish_flash_block(0x02), vec![CMD_FINISH_FLASH_BLOCK, 0x02]);
    }

    #[test]
    fn flash_block_writes_are_treated_as_write_commands() {
        assert!(is_write_command(CMD_WRITE_FLASH_BLOCK));
        assert!(is_write_command(CMD_FINISH_FLASH_BLOCK));
    }

    #[test]
    fn flash_block_echoes_parse_as_ack() {
        assert_eq!(
            parse_response(&[CMD_WRITE_FLASH_BLOCK, 0x01, 0x04, 0x88], &Dialect::NEW),
            Response::Ack
        );
        assert_eq!(
            parse_response(&[CMD_FINISH_FLASH_BLOCK, 0x01], &Dialect::NEW),
            Response::Ack
        );
    }

    #[test]
    fn read_dsp_version_cmd() {
        assert_eq!(read_dsp_version(), vec![CMD_READ_DSP_VERSION]);
    }

    #[test]
    fn write_dsp_linked_u32_without_links() {
        let cmd = write_dsp_linked_u32(0x0042, 2, &[]);
        assert_eq!(
            cmd,
            vec![
                CMD_WRITE_BIQUAD,
                0x80,
                0x00,
                0x42,
                0x00,
                0x00,
                0x02,
                0x00,
                0x00,
                0x00
            ]
        );
    }

    #[test]
    fn write_dsp_u32_le_with_ctrl_new_dialect() {
        let cmd = write_dsp_u32_le_with_ctrl(0x0042, 2, 0x80);
        assert_eq!(
            cmd,
            vec![CMD_WRITE_DSP, 0x80, 0x00, 0x42, 0x02, 0x00, 0x00, 0x00]
        );
    }

    #[test]
    fn write_dsp_linked_u32_with_one_link() {
        let cmd = write_dsp_linked_u32(0x0042, 2, &[0x1234]);
        assert_eq!(
            cmd,
            vec![
                CMD_WRITE_BIQUAD,
                0x81,
                0x00,
                0x42,
                0x12,
                0x34,
                0x02,
                0x00,
                0x00,
                0x00,
            ]
        );
    }

    #[test]
    fn write_biquad_cmd() {
        let coeffs = [1.0, 0.0, 0.0, 0.0, 0.0]; // bypass biquad
        let cmd = write_biquad(0x0100, &coeffs, &Dialect::NEW);
        assert_eq!(cmd[0], CMD_WRITE_BIQUAD);
        assert_eq!(cmd[1], 0x80);
        // After addr (2 bytes) + optional/padding (2 bytes) = 5 coeffs × 4 bytes = 20 bytes
        assert_eq!(cmd.len(), 1 + 1 + 2 + 2 + 20);
    }

    #[test]
    fn read_floats_cmd() {
        let cmd = read_floats(0x0030, 4, &Dialect::NEW);
        assert_eq!(cmd[0], CMD_READ_FLOATS);
        // ReadFloats uses a 2-byte address on the wire.
        assert_eq!(cmd[1], 0x00);
        assert_eq!(cmd[2], 0x30);
        assert_eq!(cmd[3], 4); // count
    }

    #[test]
    fn fir_load_data_caps_at_14_coefficients() {
        let coeffs: Vec<f32> = (0..16).map(|i| i as f32).collect();
        let cmd = fir_load_data(2, &coeffs);
        assert_eq!(cmd[0], CMD_FIR_LOAD_DATA);
        assert_eq!(cmd[1], 2);
        assert_eq!(cmd.len(), 2 + 14 * 4);
    }

    #[test]
    fn parse_hw_id_response() {
        let data = [0x31, 1, 85, 10]; // fw 1.85, hw_id 10
        let resp = parse_response(&data, &Dialect::NEW);
        assert_eq!(
            resp,
            Response::HardwareId(HardwareId {
                fw_major: 1,
                fw_minor: 85,
                hw_id: 10,
            })
        );
    }

    #[test]
    fn parse_floats_response_new() {
        // Response: [0x14, addr_hi, addr_lo, float_data...]
        let mut data = vec![CMD_READ_FLOATS];
        data.extend_from_slice(&[0x00, 0x30]);
        data.extend_from_slice(&1.0f32.to_le_bytes());
        data.extend_from_slice(&(-3.5f32).to_le_bytes());
        let resp = parse_response(&data, &Dialect::NEW);
        match resp {
            Response::Floats(values) => {
                assert_eq!(values.len(), 2);
                assert!((values[0] - 1.0).abs() < 0.0001);
                assert!((values[1] - (-3.5)).abs() < 0.0001);
            }
            _ => panic!("expected Floats response"),
        }
    }

    #[test]
    fn parse_floats_response_simulator() {
        // Simulator returns all zeros for unwritten DSP params
        let mut data = vec![CMD_READ_FLOATS];
        data.extend_from_slice(&[0x00, 0x30]);
        data.extend_from_slice(&0.0f32.to_le_bytes());
        data.extend_from_slice(&0.0f32.to_le_bytes());
        data.extend_from_slice(&0.0f32.to_le_bytes());
        data.extend_from_slice(&0.0f32.to_le_bytes());
        let resp = parse_response(&data, &Dialect::NEW);
        match resp {
            Response::Floats(values) => {
                assert_eq!(values.len(), 4);
                for v in values {
                    assert!((v - 0.0).abs() < 0.0001);
                }
            }
            _ => panic!("expected Floats response"),
        }
    }

    #[test]
    fn parse_preset_complete() {
        let resp = parse_response(&[RESP_PRESET_COMPLETE], &Dialect::NEW);
        assert_eq!(resp, Response::PresetComplete);
    }

    #[test]
    fn parse_set_config_event_for_request_as_preset_complete() {
        let req = set_config(0, PRESET_CHANGE_NORMAL);
        let data = [RESP_EVENT, 0x00, PRESET_CHANGE_NORMAL, 0x03];
        let resp = parse_response_for_request(&req, &data, &Dialect::NEW);
        assert_eq!(resp, Response::PresetComplete);
    }

    #[test]
    fn set_config_matches_event_response_opcode() {
        assert!(matches_response(CMD_SET_CONFIG, RESP_EVENT));
    }

    // -- Simulator-verified response tests --

    #[test]
    fn parse_hw_id_simulator() {
        // Simulator: [0x31, 0x01, 0x55, 0x0A] = fw 1.85, hw_id 10 (2x4 HD)
        let data = [0x31, 0x01, 0x55, 0x0A];
        let resp = parse_response(&data, &Dialect::NEW);
        match resp {
            Response::HardwareId(id) => {
                assert_eq!(id.fw_major, 1);
                assert_eq!(id.fw_minor, 0x55); // 85 decimal
                assert_eq!(id.hw_id, 10);
            }
            _ => panic!("expected HardwareId"),
        }
    }

    #[test]
    fn parse_flash_read_simulator() {
        // ReadFlash response: [0x05, data_bytes...]
        // Reading 4 bytes from 0xFFD8 (master status): preset=0, source=2, vol=0, mute=0
        let data = [0x05, 0x00, 0x02, 0x00, 0x00];
        let resp = parse_response(&data, &Dialect::NEW);
        match resp {
            Response::FlashData(bytes) => {
                assert_eq!(bytes, vec![0x00, 0x02, 0x00, 0x00]);
            }
            _ => panic!("expected FlashData"),
        }
    }

    #[test]
    fn parse_flash_read_for_request_simulator_shape() {
        let req = read_flash(0xFF, 0xD8, 4);
        let data = [0x05, 0x00, 0x02, 0x14, 0x00];
        let resp = parse_response_for_request(&req, &data, &Dialect::NEW);
        match resp {
            Response::FlashData(bytes) => {
                assert_eq!(bytes, vec![0x00, 0x02, 0x14, 0x00]);
            }
            _ => panic!("expected FlashData"),
        }
    }

    #[test]
    fn parse_flash_read_for_request_with_echoed_address() {
        let req = read_flash(0xFF, 0xD8, 4);
        let data = [0x05, 0xFF, 0xD8, 0x00, 0x02, 0x14, 0x00];
        let resp = parse_response_for_request(&req, &data, &Dialect::NEW);
        match resp {
            Response::FlashData(bytes) => {
                assert_eq!(bytes, vec![0x00, 0x02, 0x14, 0x00]);
            }
            _ => panic!("expected FlashData"),
        }
    }

    #[test]
    fn parse_flash_read_for_request_rejects_wrong_flash_address() {
        let req = read_flash(0xFF, 0xD8, 4);
        let data = [0x05, 0xFF, 0xA9, 0x00];
        let resp = parse_response_for_request(&req, &data, &Dialect::NEW);
        assert!(matches!(resp, Response::Unknown(_)));
    }

    #[test]
    fn parse_flash_full_addr_read_for_request_with_echoed_address() {
        let req = read_flash_full_addr(0x00ffc8, 4);
        let data = [0x3d, 0x00, 0xff, 0xc8, 0x33, 0x81, 0xf0, 0x02];
        let resp = parse_response_for_request(&req, &data, &Dialect::NEW);
        match resp {
            Response::FlashData(bytes) => {
                assert_eq!(bytes, vec![0x33, 0x81, 0xf0, 0x02]);
            }
            _ => panic!("expected FlashData"),
        }
    }

    #[test]
    fn parse_flash_full_addr_read_rejects_wrong_address() {
        let req = read_flash_full_addr(0x00ffc8, 4);
        let data = [0x3d, 0x00, 0xff, 0xd8, 0x00, 0x81, 0xf0, 0x02];
        let resp = parse_response_for_request(&req, &data, &Dialect::NEW);
        assert!(matches!(resp, Response::Unknown(_)));
    }

    #[test]
    fn parse_read_floats_for_request_with_echoed_prefix() {
        let req = read_floats(0x0FC8, 1, &Dialect::NEW);
        let mut data = req.clone();
        data.extend_from_slice(&1.0f32.to_le_bytes());
        let resp = parse_response_for_request(&req, &data, &Dialect::NEW);
        match resp {
            Response::Floats(values) => {
                assert_eq!(values.len(), 1);
                assert!((values[0] - 1.0).abs() < 0.0001);
            }
            _ => panic!("expected Floats response"),
        }
    }

    #[test]
    fn parse_read_floats_for_request_with_short_address_header() {
        let req = read_floats(0x0044, 2, &Dialect::NEW);
        let mut data = vec![0x14, 0x00, 0x44];
        data.extend_from_slice(&(-106.44f32).to_le_bytes());
        data.extend_from_slice(&(-106.66f32).to_le_bytes());
        let resp = parse_response_for_request(&req, &data, &Dialect::NEW);
        match resp {
            Response::Floats(values) => {
                assert_eq!(values.len(), 2);
                assert!((values[0] + 106.44).abs() < 0.05);
                assert!((values[1] + 106.66).abs() < 0.05);
            }
            _ => panic!("expected Floats response"),
        }
    }

    #[test]
    fn parse_read_floats_for_request_with_two_byte_header_before_float() {
        let req = read_floats(0x0FC8, 1, &Dialect::NEW);
        let mut data = req.clone();
        data.extend_from_slice(&[0x16, 0x01]);
        data.extend_from_slice(&0.0f32.to_le_bytes());
        let resp = parse_response_for_request(&req, &data, &Dialect::NEW);
        match resp {
            Response::Floats(values) => {
                assert_eq!(values.len(), 1);
                assert_eq!(values[0], 0.0);
            }
            _ => panic!("expected Floats response"),
        }
    }

    #[test]
    fn parse_read_floats_for_request_with_longer_header_before_float_block() {
        let req = read_floats(0x281d, 2, &Dialect::NEW);
        let mut data = req.clone();
        data.extend_from_slice(&[0x00, 0x00, 0x21, 0xf6, 0x08, 0x5b]);
        data.extend_from_slice(&1.0f32.to_le_bytes());
        data.extend_from_slice(&(-2.0f32).to_le_bytes());
        let resp = parse_response_for_request(&req, &data, &Dialect::NEW);
        match resp {
            Response::Floats(values) => {
                assert_eq!(values.len(), 2);
                assert!((values[0] - 1.0).abs() < 0.0001);
                assert!((values[1] + 2.0).abs() < 0.0001);
            }
            _ => panic!("expected Floats response"),
        }
    }

    #[test]
    fn parse_read_floats_rejects_live_config_junk_for_single_value() {
        let req = read_floats(0x0fc2, 1, &Dialect::NEW);
        let data = [
            0x14, 0x0f, 0xc2, 0x00, 0x01, 0x16, 0x01, 0x00, 0x00, 0x00, 0x00, 0x21, 0xf6, 0x08,
            0x5b, 0x01, 0xdc, 0x5b, 0x01,
        ];
        let resp = parse_response_for_request(&req, &data, &Dialect::NEW);
        assert!(matches!(resp, Response::Unknown(_)));
    }

    #[test]
    fn parse_read_floats_rejects_live_config_junk_for_peq_read() {
        let req = read_floats(0x0fcc, 5, &Dialect::NEW);
        let data = [
            0x14, 0x0f, 0xcc, 0x00, 0x05, 0x16, 0x01, 0x00, 0x00, 0x00, 0x00, 0x21, 0xf6, 0x08,
            0x5b, 0x01, 0xdc, 0x5b, 0x01, 0xdc, 0x5b, 0x01, 0xdc, 0x5b, 0x01, 0xdc, 0x5b, 0x01,
            0xdc, 0x5b, 0x01,
        ];
        let resp = parse_response_for_request(&req, &data, &Dialect::NEW);
        assert!(matches!(resp, Response::Unknown(_)));
    }

    #[test]
    fn parse_read_floats_rejects_echoed_zero_header_junk() {
        let req = read_floats(0x0fc2, 1, &Dialect::NEW);
        let data = [
            0x14, 0x0f, 0xc2, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x21, 0xf6, 0x08,
            0x5b, 0x01, 0xdc, 0x5b, 0x01, 0xdc, 0x5b, 0x01, 0xdb, 0x30, 0x96, 0xdb, 0x10, 0x78,
            0xdb, 0x10, 0x5a, 0x5b, 0x3c, 0x5b, 0x9e, 0xf6, 0x3f, 0xf6, 0x21, 0xf6, 0x08, 0x5b,
            0x01, 0xcd, 0xd8,
        ];
        let resp = parse_response_for_request(&req, &data, &Dialect::NEW);
        assert!(matches!(resp, Response::Unknown(_)));
    }

    #[test]
    fn parse_read_floats_rejects_wrong_address_payload() {
        let req = read_floats(0x0044, 2, &Dialect::NEW);
        let data = [
            0x14, 0x00, 0x4A, 0x89, 0x81, 0x00, 0xC3, 0x89, 0x81, 0x00, 0xC3,
        ];
        let resp = parse_response_for_request(&req, &data, &Dialect::NEW);
        assert!(matches!(resp, Response::Unknown(_)));
    }

    #[test]
    fn parse_param_words_response_for_integer_slot() {
        let req = read_floats(0x0002, 1, &Dialect::NEW);
        let data = [0x14, 0x00, 0x02, 0x02, 0x00, 0x00, 0x00];

        let words = parse_param_words_response_for_request(&req, &data).unwrap();
        assert_eq!(words, vec![2]);
    }

    #[test]
    fn parse_param_words_response_for_unframed_count_echo_integer_slot() {
        let req = read_floats(0x0002, 1, &Dialect::NEW);
        let data = [0x14, 0x00, 0x02, 0x01, 0x02, 0x00, 0x00, 0x00, 0x24, 0x33];

        let words = parse_param_words_response_for_request(&req, &data).unwrap();
        assert_eq!(words, vec![2]);
    }

    #[test]
    fn parse_param_words_response_for_typed_header_integer_slot() {
        let req = read_floats(0x0002, 1, &Dialect::NEW);
        let data = [0x14, 0x00, 0x02, 0x00, 0x01, 0x02, 0x00, 0x00, 0x00];

        let words = parse_param_words_response_for_request(&req, &data).unwrap();
        assert_eq!(words, vec![2]);
    }

    #[test]
    fn parse_param_words_be_response_for_integer_slot() {
        let req = read_floats(0x0002, 1, &Dialect::OLD);
        let data = [0x14, 0x00, 0x02, 0x00, 0x00, 0x00, 0x02];

        let words = parse_param_words_be_response_for_request(&req, &data).unwrap();
        assert_eq!(words, vec![2]);
    }

    #[test]
    fn parse_readfloats_response_for_unframed_count_echo_scalar() {
        let req = read_floats(0x0028, 1, &Dialect::NEW);
        let data = [
            0x14, 0x00, 0x28, 0x01, 0x00, 0x00, 0xf0, 0xc1, 0x24, 0x33, 0x81, 0xf0,
        ];

        let resp = parse_response_for_request(&req, &data, &Dialect::NEW);
        match resp {
            Response::Floats(values) => assert_eq!(values, vec![-30.0]),
            other => panic!("expected Floats, got {other:?}"),
        }
    }

    #[test]
    fn parse_param_words_rejects_wrong_address_payload() {
        let req = read_floats(0x0016, 2, &Dialect::NEW);
        let data = [
            0x14, 0x00, 0x4a, 0x08, 0x99, 0x03, 0x4a, 0xd3, 0xdc, 0x42, 0xfd,
        ];

        let words = parse_param_words_response_for_request(&req, &data);
        assert!(words.is_none());
    }

    #[test]
    fn parse_ack_responses() {
        // All write commands return [0x01] ACK
        for &cmd in &[
            0x01,
            CMD_WRITE_FLASH,
            CMD_SET_VOLUME,
            CMD_SET_MUTE,
            CMD_SET_SOURCE,
            CMD_WRITE_DSP,
            CMD_WRITE_BIQUAD,
            CMD_BYPASS_FILTER,
        ] {
            let resp = parse_response(&[cmd], &Dialect::NEW);
            assert_eq!(resp, Response::Ack, "cmd 0x{:02x} should parse as Ack", cmd);
        }
    }

    #[test]
    fn parse_fir_load_start_response_uses_big_endian_runtime_size() {
        let resp = parse_response(&[CMD_FIR_LOAD_START, 0x08, 0x00], &Dialect::NEW);
        assert_eq!(resp, Response::FirLoadInfo { max_coeffs: 2048 });
    }

    #[test]
    fn parse_error_response() {
        let resp = parse_response(&[0x00], &Dialect::NEW);
        match resp {
            Response::Error(data) => assert!(data.is_empty()),
            _ => panic!("expected Error"),
        }
    }

    /// Cross-check the SHARC compressor encoding contract against the
    /// authoritative Device Console XML defaults. The XML stores each field
    /// as a hex string in big-endian byte order; on the wire we transmit
    /// float32 little-endian. The two MUST round-trip to the same `f32` value
    /// — if this test fails, the on-wire encoding has drifted from the
    /// official format and live writes against real hardware will be wrong.
    ///
    /// XML defaults from `app/Audio Firmware/100/DSP_default_parameters.xml`:
    ///   threshold C1F00000 = -30.0
    ///   gain      00000000 =  0.0
    ///   ratio     40800000 =  4.0
    ///   knee      41A00000 = 20.0
    ///   atime     42200000 = 40.0 ms
    ///   rtime     42C80000 = 100.0 ms
    #[test]
    fn compressor_xml_defaults_round_trip_through_wire_encoding() {
        let cases: &[(&str, u32, f32)] = &[
            ("threshold", 0xC1F00000, -30.0),
            ("gain", 0x00000000, 0.0),
            ("ratio", 0x40800000, 4.0),
            ("knee", 0x41A00000, 20.0),
            ("atime", 0x42200000, 40.0),
            ("rtime", 0x42C80000, 100.0),
        ];

        for (name, xml_word_be, expected_value) in cases {
            // 1. The XML hex string is the big-endian byte representation of
            //    the IEEE 754 float32 value. The DSP memory word value is the
            //    same u32, irrespective of byte order.
            let from_xml = f32::from_bits(*xml_word_be);
            assert_eq!(
                from_xml, *expected_value,
                "{name}: XML hex 0x{xml_word_be:08X} should decode to {expected_value}"
            );

            // 2. write_dsp(... value, ...) must produce a frame whose payload
            //    contains the float32 little-endian bytes. Round-tripping the
            //    LE bytes back into a u32 LE should yield the same word the
            //    XML stored.
            let frame = write_dsp(
                /*addr*/ 42,
                *expected_value,
                /*save*/ true,
                &Dialect::NEW,
            );
            assert!(frame.len() >= 8, "{name}: write_dsp frame too short");
            // Frame layout: [CMD_WRITE_DSP, ctrl, addr_hi, addr_lo, val_le_b0..3]
            let val_bytes: [u8; 4] = frame[4..8].try_into().unwrap();
            let wire_word = u32::from_le_bytes(val_bytes);
            assert_eq!(
                wire_word, *xml_word_be,
                "{name}: wire LE word 0x{wire_word:08X} should match XML BE hex 0x{xml_word_be:08X}"
            );

            // 3. The reverse: ReadFloats decoded as words via decode_word_chunks
            //    (which is what client.read_param_word uses for compressor reads)
            //    must produce the same u32, and reinterpreting it as f32 must
            //    give the original value back.
            let decoded_words = decode_word_chunks(&val_bytes, 1);
            assert_eq!(
                decoded_words,
                vec![*xml_word_be],
                "{name}: decode_word_chunks should round-trip"
            );
            assert_eq!(
                f32::from_bits(decoded_words[0]),
                *expected_value,
                "{name}: f32::from_bits should reproduce the original value"
            );
        }
    }
}
