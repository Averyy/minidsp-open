//! Packet framing helpers for MiniDSP request/response traffic.
//!
//! Host-to-device request format:
//! ```text
//! Byte 0:     (payload.len() + 1) as u8   // length, self-inclusive
//! Bytes 1..N: payload (command bytes)
//! Byte N+1:   checksum = sum(all preceding bytes) & 0xFF
//! ```
//!
//! Device-to-host responses differ by transport:
//! - HID / direct USB responses are `[len][payload]` with no checksum and
//!   arbitrary padding or garbage after the frame boundary inside the 64-byte
//!   interrupt packet.
//! - TCP server responses are also `[len][payload]` with no checksum.
//!
//! HID request wrapping adds report ID `0x00` and `0xFF` padding to 65 bytes.

/// Maximum payload size in a single HID report.
///
/// Official Device Console writes fixed 64-byte HID reports total, including
/// the leading report ID byte. That leaves 63 bytes for `[len][payload][ck]`,
/// so the payload itself tops out at 61 bytes.
pub const MAX_PAYLOAD: usize = 61;

/// HID report size: 64 bytes total, including the leading report ID.
pub const HID_REPORT_SIZE: usize = 64;

/// HID data portion size (excluding report ID).
pub const HID_DATA_SIZE: usize = 63;

/// Encode a command payload into a length-prefixed, checksummed frame.
///
/// Returns the frame bytes (length + payload + checksum). Does NOT include
/// HID report ID or padding — use [`wrap_hid`] for that.
///
/// # Panics
/// Panics if payload exceeds 254 bytes (length field is self-inclusive u8).
pub fn encode_frame(payload: &[u8]) -> Vec<u8> {
    assert!(
        payload.len() <= 254,
        "payload too large for frame encoding: {} bytes",
        payload.len()
    );
    let len = (payload.len() + 1) as u8; // self-inclusive length
    let mut frame = Vec::with_capacity(payload.len() + 2);
    frame.push(len);
    frame.extend_from_slice(payload);
    let checksum = frame.iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
    frame.push(checksum);
    frame
}

/// Wrap an encoded frame into a 64-byte HID report.
///
/// Prepends report ID `0x00` and pads with `0xFF` to fill the remaining
/// 63-byte data area. This matches the official Device Console USB path.
pub fn wrap_hid(frame: &[u8]) -> [u8; HID_REPORT_SIZE] {
    let mut report = [0xFF; HID_REPORT_SIZE];
    report[0] = 0x00; // report ID
    let copy_len = frame.len().min(HID_DATA_SIZE);
    report[1..1 + copy_len].copy_from_slice(&frame[..copy_len]);
    report
}

/// Encode a payload and wrap it in a single HID report.
pub fn encode_hid(payload: &[u8]) -> [u8; HID_REPORT_SIZE] {
    wrap_hid(&encode_frame(payload))
}

/// Decode a frame from raw bytes (length + payload + checksum).
///
/// Returns the payload (without length byte or checksum) on success.
/// Returns `None` if the frame is too short or checksum doesn't match.
pub fn decode_frame(data: &[u8]) -> Option<Vec<u8>> {
    if data.is_empty() {
        return None;
    }
    let len = data[0] as usize; // self-inclusive
    let total = len + 1; // +1 for checksum
    if data.len() < total {
        return None;
    }
    // Verify checksum
    let expected = data[..total - 1]
        .iter()
        .fold(0u8, |acc, &b| acc.wrapping_add(b));
    if data[total - 1] != expected {
        return None;
    }
    Some(data[1..len].to_vec())
}

/// Decode a device response frame from raw bytes (`[len][payload]`, no checksum).
///
/// Returns `(payload, bytes_consumed)` on success. Any bytes after the frame
/// boundary are ignored by the caller; for HID responses these are often
/// garbage/non-zero padding from a fixed 64-byte interrupt packet.
pub fn decode_response_frame(data: &[u8]) -> Option<(Vec<u8>, usize)> {
    if data.is_empty() {
        return None;
    }
    let len = data[0] as usize; // self-inclusive
    if len <= 1 || data.len() < len {
        return None;
    }
    Some((data[1..len].to_vec(), len))
}

/// Decode a HID response packet from a 64-byte data buffer (no report ID).
pub fn decode_hid_response(data: &[u8]) -> Option<Vec<u8>> {
    if let Some((payload, _)) = decode_response_frame(data) {
        return Some(payload);
    }

    // Some real devices prepend a report ID on IN packets even though the
    // logical MiniDSP response still begins at the next byte.
    match data.first().copied() {
        Some(0x00) => {
            if matches!(
                data.get(1).copied(),
                Some(crate::commands::CMD_READ_HARDWARE_ID)
                    | Some(crate::commands::CMD_READ_FLOATS)
                    | Some(crate::commands::CMD_READ_FLASH)
                    | Some(crate::commands::CMD_READ_FLASH_FULL_ADDR)
                    | Some(crate::commands::CMD_WRITE_FLASH_BLOCK)
                    | Some(crate::commands::CMD_FINISH_FLASH_BLOCK)
                    | Some(crate::commands::CMD_WRITE_DSP)
                    | Some(crate::commands::CMD_WRITE_FLASH_FULL_ADDR)
                    | Some(crate::commands::CMD_WRITE_BIQUAD)
                    | Some(crate::commands::CMD_SET_VOLUME)
                    | Some(crate::commands::CMD_SET_MUTE)
                    | Some(crate::commands::CMD_SET_SOURCE)
                    | Some(crate::commands::CMD_SET_CONFIG)
                    | Some(crate::commands::CMD_BYPASS_FILTER)
                    | Some(crate::commands::CMD_DIRAC_BYPASS)
                    | Some(crate::commands::CMD_SET_DISPLAY_BRIGHTNESS)
                    | Some(crate::commands::CMD_SET_DISPLAY_IDLE_TIME)
                    | Some(crate::commands::CMD_SET_DRE)
                    | Some(crate::commands::CMD_COPY_PRESET)
                    | Some(crate::commands::CMD_FIR_LOAD_DATA)
                    | Some(crate::commands::CMD_RELOAD_DSP_PARAM)
                    | Some(crate::commands::CMD_LOG_CONTROL)
                    | Some(crate::commands::CMD_WRITE_DFLASH_ID)
                    | Some(crate::commands::RESP_EVENT)
            ) {
                return Some(data[1..].to_vec());
            }

            if let Some((payload, _)) = decode_response_frame(&data[1..]) {
                return Some(payload);
            }
        }
        Some(0x01) => {
            // Real hardware can return a report-ID-prefixed unframed echo for
            // successful writes. Restrict that special case to write/ack-like
            // commands so late read-command echoes cannot poison the next
            // request/response exchange.
            if data
                .get(1)
                .copied()
                .is_some_and(crate::commands::is_write_command)
            {
                return Some(data[1..].to_vec());
            }

            if let Some((payload, _)) = decode_response_frame(&data[1..]) {
                return Some(payload);
            }
        }
        Some(0xFF) => {
            // The 2x4 HD can emit inbound packets with a leading 0xFF prefix
            // in the preset-change window. Treat that byte like an alternate
            // report ID so the queue can keep reading until it sees the real
            // completion response instead of failing the whole command.
            if data.len() > 1 && crate::commands::is_known_opcode(data[1]) {
                return Some(data[1..].to_vec());
            }

            if let Some((payload, _)) = decode_response_frame(&data[1..]) {
                return Some(payload);
            }
        }
        _ => {}
    }

    None
}

/// Parse a TCP stream frame. TCP framing: `buf[0]` is self-inclusive length,
/// read `buf[0] + 1` bytes total (the extra byte is the checksum).
///
/// Returns `(payload, bytes_consumed)` or None if insufficient data.
pub fn decode_tcp_frame(data: &[u8]) -> Option<(Vec<u8>, usize)> {
    // Skip zero-byte padding
    let mut offset = 0;
    while offset < data.len() && data[offset] == 0 {
        offset += 1;
    }
    if offset >= data.len() {
        return None;
    }
    let len = data[offset] as usize;
    let total = len + 1; // +1 for checksum byte appended after
    if data.len() - offset < total {
        return None;
    }
    let frame = &data[offset..offset + total];
    let payload = decode_frame(frame)?;
    Some((payload, offset + total))
}

/// Encode a response frame for TCP (no checksum in server responses per protocol).
/// Server-side: `buf[0]` is self-inclusive length, followed by payload, no checksum.
pub fn encode_tcp_response(payload: &[u8]) -> Vec<u8> {
    let len = (payload.len() + 1) as u8;
    let mut frame = Vec::with_capacity(payload.len() + 1);
    frame.push(len);
    frame.extend_from_slice(payload);
    frame
}

/// Encode a TCP bridge response in the compact format the official Device
/// Console network parser expects.
///
/// The app does not accept raw echoed write opcodes like `[len][0x06 ...]` for
/// successful writes. Instead it expects:
///
/// - `0x01` for generic OK
/// - `0x00` for error
/// - `0x02 0xAA` / `0x02 0xAB` for reset / preset completion
/// - `0x02 <block_id>` for `FinishFlashBlock`
/// - `[len][cmd ...]` only for true read-style payloads
pub fn encode_device_console_tcp_response(
    request: &[u8],
    raw_resp: &[u8],
    dialect: &crate::dialect::Dialect,
) -> Vec<u8> {
    let parsed = crate::commands::parse_response_for_request(request, raw_resp, dialect);

    if request.first() == Some(&crate::commands::CMD_FINISH_FLASH_BLOCK)
        && matches!(parsed, crate::commands::Response::Ack)
    {
        let block_id = raw_resp
            .get(1)
            .copied()
            .or_else(|| request.get(1).copied())
            .unwrap_or(0);
        return vec![crate::commands::RESP_EVENT, block_id];
    }

    match parsed {
        crate::commands::Response::Ack => vec![crate::commands::RESP_OK],
        crate::commands::Response::Error(_) => vec![crate::commands::RESP_ERROR],
        crate::commands::Response::ResetComplete => {
            vec![
                crate::commands::RESP_EVENT,
                crate::commands::RESP_RESET_COMPLETE,
            ]
        }
        crate::commands::Response::PresetComplete => {
            vec![
                crate::commands::RESP_EVENT,
                crate::commands::RESP_PRESET_COMPLETE,
            ]
        }
        crate::commands::Response::Overflow => vec![crate::commands::RESP_OVERFLOW],
        _ => encode_tcp_response(raw_resp),
    }
}

/// Decode a TCP server response frame. Server responses use `[len][payload]` with
/// NO checksum (unlike client requests which include a checksum byte).
/// Skips leading zero-byte padding (WI-DG compat).
///
/// Returns `(payload, bytes_consumed)` or None if insufficient data.
pub fn decode_tcp_response(data: &[u8]) -> Option<(Vec<u8>, usize)> {
    // RESP_ERROR is zero-valued, so an exact single-byte error must be
    // recognized before we strip WI-DG-style leading zero padding.
    if data == [crate::commands::RESP_ERROR] {
        return Some((vec![crate::commands::RESP_ERROR], 1));
    }
    // Skip zero-byte padding
    let mut offset = 0;
    while offset < data.len() && data[offset] == 0 {
        offset += 1;
    }
    if offset >= data.len() {
        return None;
    }
    match data[offset] {
        crate::commands::RESP_OK | crate::commands::RESP_ERROR | crate::commands::RESP_OVERFLOW => {
            return Some((vec![data[offset]], offset + 1));
        }
        _ => {}
    }
    let (payload, consumed) = decode_response_frame(&data[offset..])?;
    Some((payload, offset + consumed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrip() {
        let payload = &[0x31]; // ReadHardwareId
        let frame = encode_frame(payload);
        assert_eq!(frame[0], 0x02); // length = 1 + 1 = 2 (self-inclusive)
        let checksum = (0x02u8).wrapping_add(0x31);
        assert_eq!(frame[2], checksum);
        let decoded = decode_frame(&frame).unwrap();
        assert_eq!(decoded, payload);
    }

    #[test]
    fn tcp_response_accepts_single_byte_ok_frame() {
        let decoded = decode_tcp_response(&[crate::commands::RESP_OK]).unwrap();
        assert_eq!(decoded, (vec![crate::commands::RESP_OK], 1));
    }

    #[test]
    fn tcp_response_accepts_single_byte_error_frame() {
        let decoded = decode_tcp_response(&[crate::commands::RESP_ERROR]).unwrap();
        assert_eq!(decoded, (vec![crate::commands::RESP_ERROR], 1));
    }

    #[test]
    fn tcp_response_accepts_padded_single_byte_ok_frame() {
        let decoded = decode_tcp_response(&[0x00, 0x00, crate::commands::RESP_OK]).unwrap();
        assert_eq!(decoded, (vec![crate::commands::RESP_OK], 3));
    }

    #[test]
    fn hid_request_format() {
        let payload = &[0x42, 0x10]; // SetVolume, value=0x10
        let report = encode_hid(payload);
        assert_eq!(report.len(), 64);
        assert_eq!(report[0], 0x00); // report ID
        // Remaining bytes after frame should be 0xFF padding
        let frame_len = payload.len() + 2; // len + payload + checksum
        for &b in &report[1 + frame_len..] {
            assert_eq!(b, 0xFF);
        }
    }

    #[test]
    fn hid_max_payload_matches_official_64_byte_reports() {
        let payload = vec![0xaa; MAX_PAYLOAD];
        let report = encode_hid(&payload);
        assert_eq!(report.len(), HID_REPORT_SIZE);
        assert_eq!(report[0], 0x00);
        assert_eq!(report[1], (payload.len() + 1) as u8);
        let expected_checksum = report[1..HID_REPORT_SIZE - 1]
            .iter()
            .fold(0u8, |acc, &b| acc.wrapping_add(b));
        assert_eq!(report[HID_REPORT_SIZE - 1], expected_checksum);
    }

    #[test]
    fn hid_response_without_checksum() {
        let mut report = [0u8; HID_DATA_SIZE];
        report[..12].copy_from_slice(&[
            0x0c, 0x31, 0x01, 0x55, 0x0a, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ]);
        report[12..16].copy_from_slice(&[0x21, 0xf6, 0x08, 0x5b]); // garbage after frame

        let decoded = decode_hid_response(&report).unwrap();
        assert_eq!(
            decoded,
            vec![
                0x31, 0x01, 0x55, 0x0a, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00
            ]
        );
    }

    #[test]
    fn hid_ack_response_without_checksum() {
        let report = [0x01, 0x42, 0x14, 0xfe, 0x30, 0x39, 0x00, 0x00];
        let decoded = decode_hid_response(&report).unwrap();
        assert_eq!(&decoded[..5], &[0x42, 0x14, 0xfe, 0x30, 0x39]);
    }

    #[test]
    fn hid_response_with_leading_zero_report_id() {
        let mut report = [0u8; HID_DATA_SIZE];
        report[..13].copy_from_slice(&[
            0x00, 0x0c, 0x31, 0x01, 0x55, 0x0a, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ]);

        let decoded = decode_hid_response(&report).unwrap();
        assert_eq!(
            decoded,
            vec![
                0x31, 0x01, 0x55, 0x0a, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00
            ]
        );
    }

    #[test]
    fn hid_unframed_read_response_with_leading_zero_report_id() {
        let mut report = [0u8; HID_DATA_SIZE];
        report[..8].copy_from_slice(&[0x00, 0x02, 0xa0, 0x58, 0x05, 0x71, 0x00, 0x00]);

        let decoded = decode_hid_response(&report).unwrap();
        assert_eq!(&decoded[..7], &[0x02, 0xa0, 0x58, 0x05, 0x71, 0x00, 0x00]);
        assert_eq!(decoded.len(), HID_DATA_SIZE - 1);
    }

    #[test]
    fn hid_unframed_write_echo_with_leading_one_report_id() {
        let mut report = [0u8; HID_DATA_SIZE];
        report[..9].copy_from_slice(&[0x01, 0x13, 0xA0, 0x00, 0x1D, 0x00, 0x00, 0x80, 0xC0]);

        let decoded = decode_hid_response(&report).unwrap();
        assert_eq!(
            &decoded[..8],
            &[0x13, 0xA0, 0x00, 0x1D, 0x00, 0x00, 0x80, 0xC0]
        );
    }

    #[test]
    fn hid_unframed_write_flash_echo_with_leading_one_report_id() {
        let report = [0x01, 0x04, 0xff, 0xc8, 0x5a, 0x4c, 0x1b, 0x2b];
        let decoded = decode_hid_response(&report).unwrap();
        assert_eq!(decoded, vec![0x04, 0xff, 0xc8, 0x5a, 0x4c, 0x1b, 0x2b]);
    }

    #[test]
    fn hid_unframed_write_flash_block_echo_with_leading_zero_report_id() {
        let report = [0x00, 0x06, 0x01, 0x04, 0x88, 0x97, 0x13, 0x0f, 0x00, 0x00];
        let decoded = decode_hid_response(&report).unwrap();
        assert_eq!(
            decoded,
            vec![0x06, 0x01, 0x04, 0x88, 0x97, 0x13, 0x0f, 0x00, 0x00]
        );
    }

    #[test]
    fn hid_unframed_write_flash_block_echo_with_leading_one_report_id() {
        let report = [0x01, 0x06, 0x01, 0x04, 0x88, 0x97, 0x13, 0x0f, 0x00, 0x00];
        let decoded = decode_hid_response(&report).unwrap();
        assert_eq!(
            decoded,
            vec![0x06, 0x01, 0x04, 0x88, 0x97, 0x13, 0x0f, 0x00, 0x00]
        );
    }

    #[test]
    fn hid_unframed_finish_flash_block_echo_with_leading_zero_report_id() {
        let report = [0x00, 0x07, 0x02, 0x00, 0x00];
        let decoded = decode_hid_response(&report).unwrap();
        assert_eq!(decoded, vec![0x07, 0x02, 0x00, 0x00]);
    }

    #[test]
    fn hid_unframed_finish_flash_block_echo_with_leading_one_report_id() {
        let report = [0x01, 0x07, 0x02, 0x00, 0x00];
        let decoded = decode_hid_response(&report).unwrap();
        assert_eq!(decoded, vec![0x07, 0x02, 0x00, 0x00]);
    }

    #[test]
    fn hid_unframed_event_with_leading_ff_prefix() {
        let report = [0xff, 0x02, 0x00, 0x02, 0x03, 0x00, 0x00, 0x00];
        let decoded = decode_hid_response(&report).unwrap();
        assert_eq!(decoded, vec![0x02, 0x00, 0x02, 0x03, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn hid_unframed_read_echo_with_leading_one_report_id_is_rejected() {
        let report = [0x01, 0x05, 0xff, 0xa9, 0x02];
        assert!(decode_hid_response(&report).is_none());
    }

    #[test]
    fn bad_checksum_rejected() {
        let mut frame = encode_frame(&[0x31]);
        frame[2] ^= 0xFF; // corrupt checksum
        assert!(decode_frame(&frame).is_none());
    }

    #[test]
    fn tcp_frame_with_padding() {
        let frame = encode_frame(&[0x31]);
        let mut tcp_data = vec![0x00, 0x00]; // zero padding
        tcp_data.extend_from_slice(&frame);
        let (payload, consumed) = decode_tcp_frame(&tcp_data).unwrap();
        assert_eq!(payload, &[0x31]);
        assert_eq!(consumed, 2 + frame.len()); // 2 padding + frame
    }

    #[test]
    fn tcp_response_roundtrip() {
        let payload = &[0x31, 0x0A, 0x64];
        let frame = encode_tcp_response(payload);
        assert_eq!(frame[0], 4); // self-inclusive length = 3 + 1
        assert_eq!(frame.len(), 4); // no checksum byte
        let (decoded, consumed) = decode_tcp_response(&frame).unwrap();
        assert_eq!(decoded, payload);
        assert_eq!(consumed, 4);
    }

    #[test]
    fn tcp_response_with_padding() {
        let frame = encode_tcp_response(&[0x31]);
        let mut data = vec![0x00, 0x00]; // zero padding
        data.extend_from_slice(&frame);
        let (payload, consumed) = decode_tcp_response(&data).unwrap();
        assert_eq!(payload, &[0x31]);
        assert_eq!(consumed, 2 + frame.len()); // 2 padding + frame
    }

    #[test]
    fn encode_frame_complex() {
        // WriteDSP: 0x13, 0x80, addr_hi, addr_lo, value bytes
        let payload = &[0x13, 0x80, 0x00, 0x42, 0x3F, 0x80, 0x00, 0x00];
        let frame = encode_frame(payload);
        assert_eq!(frame[0], 9); // self-inclusive length = 8 + 1
        let decoded = decode_frame(&frame).unwrap();
        assert_eq!(decoded, payload);
    }

    #[test]
    fn device_console_write_flash_block_echo_becomes_ok() {
        let request = [0x06u8, 0x01, 0x04, 0x88, 0x97, 0x13, 0x0f, 0x00, 0x00];
        let raw_resp = request;
        assert_eq!(
            encode_device_console_tcp_response(&request, &raw_resp, &crate::dialect::Dialect::NEW),
            vec![crate::commands::RESP_OK]
        );
    }

    #[test]
    fn device_console_finish_flash_block_echo_becomes_block_event() {
        let request = [crate::commands::CMD_FINISH_FLASH_BLOCK, 0x02];
        let raw_resp = request;
        assert_eq!(
            encode_device_console_tcp_response(&request, &raw_resp, &crate::dialect::Dialect::NEW),
            vec![crate::commands::RESP_EVENT, 0x02]
        );
    }

    #[test]
    fn device_console_finish_flash_block_event_stays_block_event() {
        let request = [crate::commands::CMD_FINISH_FLASH_BLOCK, 0x02];
        let raw_resp = [crate::commands::RESP_EVENT, 0x02];
        assert_eq!(
            encode_device_console_tcp_response(&request, &raw_resp, &crate::dialect::Dialect::NEW),
            vec![crate::commands::RESP_EVENT, 0x02]
        );
    }

    #[test]
    fn device_console_finish_flash_block_bare_event_uses_request_block_id() {
        let request = [crate::commands::CMD_FINISH_FLASH_BLOCK, 0x02];
        let raw_resp = [crate::commands::RESP_EVENT];
        assert_eq!(
            encode_device_console_tcp_response(&request, &raw_resp, &crate::dialect::Dialect::NEW),
            vec![crate::commands::RESP_EVENT, 0x02]
        );
    }

    #[test]
    fn device_console_finish_flash_block_ok_becomes_block_event() {
        let request = [crate::commands::CMD_FINISH_FLASH_BLOCK, 0x01];
        let raw_resp = [crate::commands::RESP_OK];
        assert_eq!(
            encode_device_console_tcp_response(&request, &raw_resp, &crate::dialect::Dialect::NEW),
            vec![crate::commands::RESP_EVENT, 0x01]
        );
    }

    #[test]
    fn device_console_reload_same_preset_stays_preset_complete_event() {
        let request =
            crate::commands::set_config(0, crate::commands::PRESET_CHANGE_RELOAD_SAME_SLOT);
        let raw_resp = [crate::commands::RESP_PRESET_COMPLETE];
        assert_eq!(
            encode_device_console_tcp_response(&request, &raw_resp, &crate::dialect::Dialect::NEW),
            vec![
                crate::commands::RESP_EVENT,
                crate::commands::RESP_PRESET_COMPLETE
            ]
        );
    }

    #[test]
    fn device_console_set_config_without_reset_becomes_ok() {
        let request = crate::commands::set_config(0, crate::commands::PRESET_CHANGE_WITHOUT_RESET);
        let raw_resp = [crate::commands::RESP_OK];
        assert_eq!(
            encode_device_console_tcp_response(&request, &raw_resp, &crate::dialect::Dialect::NEW),
            vec![crate::commands::RESP_OK]
        );
    }

    #[test]
    fn device_console_read_flash_keeps_command_frame_shape() {
        let request = crate::commands::read_flash(0xff, 0xa3, 1);
        let raw_resp = [crate::commands::CMD_READ_FLASH, 0xff, 0xa3, 0x03];
        assert_eq!(
            encode_device_console_tcp_response(&request, &raw_resp, &crate::dialect::Dialect::NEW),
            vec![5, crate::commands::CMD_READ_FLASH, 0xff, 0xa3, 0x03]
        );
    }
}
