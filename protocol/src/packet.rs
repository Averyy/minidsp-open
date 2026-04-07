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

/// Maximum payload size in a single HID report (64 bytes data - 1 length - 1 checksum).
pub const MAX_PAYLOAD: usize = 62;

/// HID report size: 1 byte report ID + 64 bytes data.
pub const HID_REPORT_SIZE: usize = 65;

/// HID data portion size (excluding report ID).
pub const HID_DATA_SIZE: usize = 64;

/// Encode a command payload into a length-prefixed, checksummed frame.
///
/// Returns the frame bytes (length + payload + checksum). Does NOT include
/// HID report ID or padding — use [`wrap_hid`] for that.
///
/// # Panics
/// Panics if payload exceeds 254 bytes (length field is self-inclusive u8).
pub fn encode_frame(payload: &[u8]) -> Vec<u8> {
    assert!(payload.len() <= 254, "payload too large for frame encoding: {} bytes", payload.len());
    let len = (payload.len() + 1) as u8; // self-inclusive length
    let mut frame = Vec::with_capacity(payload.len() + 2);
    frame.push(len);
    frame.extend_from_slice(payload);
    let checksum = frame.iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
    frame.push(checksum);
    frame
}

/// Wrap an encoded frame into a 65-byte HID report.
///
/// Prepends report ID 0x00 and pads with 0xFF to fill 65 bytes.
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
    if len == 0 || data.len() < len {
        return None;
    }
    Some((data[1..len].to_vec(), len))
}

/// Decode a HID response packet from a 64-byte data buffer (no report ID).
pub fn decode_hid_response(data: &[u8]) -> Option<Vec<u8>> {
    let (payload, _) = decode_response_frame(data)?;
    Some(payload)
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

/// Decode a TCP server response frame. Server responses use `[len][payload]` with
/// NO checksum (unlike client requests which include a checksum byte).
/// Skips leading zero-byte padding (WI-DG compat).
///
/// Returns `(payload, bytes_consumed)` or None if insufficient data.
pub fn decode_tcp_response(data: &[u8]) -> Option<(Vec<u8>, usize)> {
    // Skip zero-byte padding
    let mut offset = 0;
    while offset < data.len() && data[offset] == 0 {
        offset += 1;
    }
    if offset >= data.len() {
        return None;
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
    fn hid_request_format() {
        let payload = &[0x42, 0x10]; // SetVolume, value=0x10
        let report = encode_hid(payload);
        assert_eq!(report[0], 0x00); // report ID
        // Remaining bytes after frame should be 0xFF padding
        let frame_len = payload.len() + 2; // len + payload + checksum
        for &b in &report[1 + frame_len..] {
            assert_eq!(b, 0xFF);
        }
    }

    #[test]
    fn hid_response_without_checksum() {
        let mut report = [0u8; HID_DATA_SIZE];
        report[..12].copy_from_slice(&[
            0x0c, 0x31, 0x01, 0x55, 0x0a, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ]);
        report[12..16].copy_from_slice(&[0x21, 0xf6, 0x08, 0x5b]); // garbage after frame

        let decoded = decode_hid_response(&report).unwrap();
        assert_eq!(decoded, vec![0x31, 0x01, 0x55, 0x0a, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn hid_ack_response_without_checksum() {
        let report = [0x01, 0x42, 0x14, 0xfe, 0x30, 0x39, 0x00, 0x00];
        let decoded = decode_hid_response(&report).unwrap();
        assert!(decoded.is_empty());
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
}
