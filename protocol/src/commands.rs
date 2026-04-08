//! Command builders and response parsing for the MiniDSP HID protocol.
//!
//! Each command is a byte sequence that gets wrapped in a frame (see [`crate::packet`]).
//! The first byte identifies the command type.

use crate::dialect::{self, Dialect};

// -- Command opcodes --

pub const CMD_RESET: u8 = 0x03;
pub const CMD_WRITE_FLASH: u8 = 0x04;
pub const CMD_READ_FLASH: u8 = 0x05;
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
pub const CMD_READ_HARDWARE_ID: u8 = 0x31;
pub const CMD_SET_SOURCE: u8 = 0x34;
pub const CMD_FIR_LOAD_START: u8 = 0x39;
pub const CMD_FIR_LOAD_DATA: u8 = 0x3A;
pub const CMD_RELOAD_DSP_PARAM: u8 = 0x3B;
pub const CMD_DIRAC_BYPASS: u8 = 0x3F;
pub const CMD_SET_VOLUME: u8 = 0x42;
pub const CMD_DIRAC_DELAY_GAIN: u8 = 0x45;
pub const CMD_SET_HDMI_SOURCE: u8 = 0x49;
pub const CMD_LOG_CONTROL: u8 = 0x54;

// -- Response types --

pub const RESP_ERROR: u8 = 0x00;
pub const RESP_OK: u8 = 0x01;
pub const RESP_EVENT: u8 = 0x02;
pub const RESP_RESET_COMPLETE: u8 = 0xAA;
pub const RESP_PRESET_COMPLETE: u8 = 0xAB;
pub const RESP_OVERFLOW: u8 = 0xFF;

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

/// Build a WriteDSP command with a raw little-endian 32-bit payload.
///
/// This is used for config slots modeled by the official app as typed 32-bit
/// values (`integer`, `samplecnt`, etc.) rather than DSP coefficient floats.
pub fn write_dsp_u32(addr: u16, value: u32, save: bool) -> Vec<u8> {
    let bytes = value.to_le_bytes();
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

/// Build a FirLoadData command. Up to 15 float32 coefficients per packet.
pub fn fir_load_data(index: u8, coeffs: &[f32]) -> Vec<u8> {
    let mut cmd = vec![CMD_FIR_LOAD_DATA, index];
    for &c in &coeffs[..coeffs.len().min(15)] {
        cmd.extend_from_slice(&c.to_le_bytes());
    }
    cmd
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

/// Build a SetHdmiSource command (nanoAVR only).
pub fn set_hdmi_source(source: u8) -> Vec<u8> {
    vec![CMD_SET_HDMI_SOURCE, source]
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

        CMD_FIR_LOAD_START => {
            if data.len() >= 3 {
                let max_coeffs = u16::from_le_bytes([data[1], data[2]]);
                Response::FirLoadInfo { max_coeffs }
            } else {
                Response::Unknown(data.to_vec())
            }
        }

        // Ack-like responses for various commands
        CMD_WRITE_DSP
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
        | CMD_SET_HDMI_SOURCE
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
    if request.first() == Some(&CMD_READ_FLASH) && data.first() == Some(&CMD_READ_FLASH) {
        if request.len() >= 4 && data.len() >= 2 {
            let addr_hi = request[1];
            let addr_lo = request[2];
            let count = request[3] as usize;
            let body = &data[1..];

            if body.len() >= count + 2 && body[0] == addr_hi && body[1] == addr_lo {
                return Response::FlashData(body[2..2 + count].to_vec());
            }

            if body.len() >= count {
                return Response::FlashData(body[..count].to_vec());
            }
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

/// Parse a raw `ReadFloats` reply into typed 32-bit words instead of float32.
pub fn parse_param_words_response_for_request(request: &[u8], data: &[u8]) -> Option<Vec<u32>> {
    if request.first().copied() != Some(CMD_READ_FLOATS) {
        return None;
    }

    let count = request.last().copied().unwrap_or(0) as usize;
    if count == 0 {
        return Some(vec![]);
    }

    let needed = count * 4;

    if data.len() >= 3
        && data.first().copied() == Some(CMD_READ_FLOATS)
        && request.len() >= 3
        && data.get(1) == request.get(1)
        && data.get(2) == request.get(2)
        && data.len() >= 3 + needed
    {
        return Some(decode_word_chunks(&data[3..3 + needed], count));
    }

    if data.starts_with(request) && data.len() >= request.len() + needed {
        return Some(decode_word_chunks(
            &data[request.len()..request.len() + needed],
            count,
        ));
    }

    if data.first().copied() == Some(CMD_READ_FLOATS) && data.len() >= 1 + needed {
        return Some(decode_word_chunks(&data[1..1 + needed], count));
    }

    None
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
        CMD_SET_CONFIG => resp_opcode == CMD_SET_CONFIG || resp_opcode == RESP_PRESET_COMPLETE,
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
        let cmd = set_config(2, 0);
        assert_eq!(cmd, vec![0x25, 0x02, 0x00]);
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
            vec![CMD_WRITE_DSP, 0x80, 0x00, 0x42, 0x02, 0x00, 0x00, 0x00]
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
    fn parse_param_words_response_for_integer_slot() {
        let req = read_floats(0x0002, 1, &Dialect::NEW);
        let data = [0x14, 0x00, 0x02, 0x02, 0x00, 0x00, 0x00];

        let words = parse_param_words_response_for_request(&req, &data).unwrap();
        assert_eq!(words, vec![2]);
    }

    #[test]
    fn parse_ack_responses() {
        // All write commands return [0x01] ACK
        for &cmd in &[
            0x01,
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
    fn parse_error_response() {
        let resp = parse_response(&[0x00], &Dialect::NEW);
        match resp {
            Response::Error(data) => assert!(data.is_empty()),
            _ => panic!("expected Error"),
        }
    }
}
