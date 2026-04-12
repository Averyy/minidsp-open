//! UDP discovery packet encoding/decoding (port 3999).
//!
//! Wire format: 72 bytes, broadcast every 5 seconds to the per-interface
//! broadcast address. Matches the MiniDSP Wi-DG beacon format exactly so that
//! Device Console discovers desktop-daemon and ESP32 targets as native Wi-DG
//! devices. See `app/widg-re/beacon.md` for the byte-by-byte specification
//! derived from the Wi-DG firmware binary.

extern crate alloc;

/// Magic header bytes at offset 0x00-0x03.
///
/// Byte 0x01 (0x12 / 18 decimal) is interpreted as a protocol version by the
/// Wi-DG firmware and is required by Device Console for beacon recognition.
pub const MAGIC: [u8; 4] = [0x80, 0x12, 0x05, 0xA0];

/// Fixed tag bytes at offset 0x0C-0x0D. Semantics are unknown (possibly a
/// Wi-DG device-type discriminator) but Device Console rejects beacons
/// without them.
pub const TAG: [u8; 2] = [0x14, 0xD5];

/// Exact beacon size in bytes (0x48).
pub const PACKET_SIZE: usize = 72;

/// Minimum accepted packet size when decoding (kept at 36 for backwards
/// compatibility with captures produced by the legacy 36-byte encoder).
pub const MIN_PACKET_SIZE: usize = 36;

/// Maximum device-name length encoded at bytes 0x24..0x48.
pub const MAX_NAME_LEN: usize = PACKET_SIZE - 0x24;

/// UDP port used for discovery broadcasts.
pub const DISCOVERY_PORT: u16 = 3999;

/// Parsed/buildable discovery packet.
#[derive(Debug, Clone)]
pub struct DiscoveryPacket {
    pub mac: [u8; 6],
    pub ip: [u8; 4],
    pub hw_id: u8,
    pub hw_id_sub_a: u8,
    pub hw_id_sub_b: u8,
    pub dsp_no: u8,
    pub serial: u16,
    pub hostname: alloc::string::String,
}

impl DiscoveryPacket {
    /// Build a discovery packet from fully-resolved device identity.
    pub fn from_device(
        mac: [u8; 6],
        ip: [u8; 4],
        hw_id: u8,
        hw_id_sub_a: u8,
        hw_id_sub_b: u8,
        dsp_no: u8,
        serial: u16,
        hostname: alloc::string::String,
    ) -> Self {
        Self {
            mac,
            ip,
            hw_id,
            hw_id_sub_a,
            hw_id_sub_b,
            dsp_no,
            serial,
            hostname,
        }
    }

    /// Encode a 72-byte beacon matching the Wi-DG wire format exactly.
    pub fn encode(&self) -> alloc::vec::Vec<u8> {
        let mut buf = alloc::vec![0xFFu8; PACKET_SIZE];

        buf[0..4].copy_from_slice(&MAGIC);
        // bytes 0x04-0x05 stay 0xFF
        buf[6..12].copy_from_slice(&self.mac);
        buf[12..14].copy_from_slice(&TAG);
        buf[14..18].copy_from_slice(&self.ip);
        buf[0x12] = self.hw_id;
        buf[0x13] = self.hw_id_sub_a;
        buf[0x14] = self.hw_id_sub_b;
        buf[0x15] = self.dsp_no;
        buf[0x16] = (self.serial >> 8) as u8;
        buf[0x17] = (self.serial & 0xFF) as u8;
        // bytes 0x18-0x22 stay 0xFF

        let name_bytes = self.hostname.as_bytes();
        let name_len = name_bytes.len().min(MAX_NAME_LEN);
        buf[0x23] = name_len as u8;
        buf[0x24..0x24 + name_len].copy_from_slice(&name_bytes[..name_len]);
        // Remaining bytes up to byte 0x47 stay 0xFF.

        buf
    }

    /// Encode a 72-byte appliance-mode beacon emitted when no DSP is attached.
    ///
    /// The magic header, MAC, tag bytes, and IP are populated so clients can
    /// still see the device on the LAN, but bytes 0x12..0x18 and 0x23.. stay
    /// as 0xFF — matching what the real Wi-DG emits when its USB device is
    /// unplugged.
    pub fn encode_appliance(mac: [u8; 6], ip: [u8; 4]) -> alloc::vec::Vec<u8> {
        let mut buf = alloc::vec![0xFFu8; PACKET_SIZE];
        buf[0..4].copy_from_slice(&MAGIC);
        buf[6..12].copy_from_slice(&mac);
        buf[12..14].copy_from_slice(&TAG);
        buf[14..18].copy_from_slice(&ip);
        buf
    }

    /// Overwrite the IPv4 address bytes (0x0E-0x11) in an already-encoded
    /// beacon. Used by the broadcast loop so each per-interface `sendto` can
    /// stamp its own IP without re-running the full encoder.
    pub fn patch_ip(buf: &mut [u8], ip: [u8; 4]) {
        if buf.len() >= 18 {
            buf[14..18].copy_from_slice(&ip);
        }
    }

    /// Parse from wire bytes.
    ///
    /// Accepts packets of any length >= 36 to keep legacy captures decodable,
    /// but byte 0x01 (protocol version) is intentionally not validated so
    /// that both the historical 0x00 magic and the Wi-DG 0x12 magic parse.
    pub fn decode(data: &[u8]) -> Option<Self> {
        if data.len() < MIN_PACKET_SIZE {
            return None;
        }
        if data[0] != 0x80 || data[2] != 0x05 || data[3] != 0xA0 {
            return None;
        }

        let mut mac = [0u8; 6];
        mac.copy_from_slice(&data[6..12]);
        let ip = [data[14], data[15], data[16], data[17]];

        let name_len = data[0x23] as usize;
        let hostname = if name_len <= MAX_NAME_LEN && data.len() >= 0x24 + name_len {
            alloc::string::String::from_utf8_lossy(&data[0x24..0x24 + name_len])
                .trim_end_matches('\0')
                .into()
        } else {
            alloc::string::String::new()
        };

        Some(Self {
            mac,
            ip,
            hw_id: data[0x12],
            hw_id_sub_a: data[0x13],
            hw_id_sub_b: data[0x14],
            dsp_no: data[0x15],
            serial: u16::from_be_bytes([data[0x16], data[0x17]]),
            hostname,
        })
    }
}

/// Convert a 32-bit MiniDSP serial number into the 16-bit form used in the
/// beacon's sn_high/sn_low bytes. The legacy minidsp-rs encoder subtracted
/// 900000 before masking to u16; this helper preserves that transform so
/// callers don't have to duplicate it.
pub fn serial_to_u16(raw_serial: u32) -> u16 {
    (raw_serial.wrapping_sub(900000) & 0xFFFF) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_length_is_72() {
        let pkt = DiscoveryPacket::from_device(
            [0x00, 0x1B, 0x59, 0xAB, 0xCD, 0xEF],
            [192, 168, 1, 100],
            0x0A,
            0x01,
            0x85,
            0x64,
            42,
            "MiniDSP-2x4-HD".into(),
        );
        assert_eq!(pkt.encode().len(), PACKET_SIZE);
    }

    #[test]
    fn encode_byte_exact_2x4hd() {
        let pkt = DiscoveryPacket::from_device(
            [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF],
            [10, 0, 0, 42],
            0x0A,
            0x01,
            0x85,
            0x64,
            0x1234,
            "MiniDSP-2x4-HD".into(),
        );
        let got = pkt.encode();

        let mut expected = [0xFFu8; 72];
        expected[0] = 0x80;
        expected[1] = 0x12;
        expected[2] = 0x05;
        expected[3] = 0xA0;
        expected[6..12].copy_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
        expected[12] = 0x14;
        expected[13] = 0xD5;
        expected[14..18].copy_from_slice(&[10, 0, 0, 42]);
        expected[0x12] = 0x0A;
        expected[0x13] = 0x01;
        expected[0x14] = 0x85;
        expected[0x15] = 0x64;
        expected[0x16] = 0x12;
        expected[0x17] = 0x34;
        expected[0x23] = 14;
        expected[0x24..0x24 + 14].copy_from_slice(b"MiniDSP-2x4-HD");

        assert_eq!(got.as_slice(), &expected[..]);
    }

    #[test]
    fn encode_appliance_leaves_identity_ff() {
        let mac = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66];
        let ip = [192, 168, 4, 1];
        let got = DiscoveryPacket::encode_appliance(mac, ip);

        assert_eq!(got.len(), PACKET_SIZE);
        assert_eq!(&got[0..4], &MAGIC);
        assert_eq!(&got[4..6], &[0xFF, 0xFF]);
        assert_eq!(&got[6..12], &mac);
        assert_eq!(&got[12..14], &TAG);
        assert_eq!(&got[14..18], &ip);

        for (i, b) in got[0x12..=0x17].iter().enumerate() {
            assert_eq!(
                *b,
                0xFF,
                "identity byte 0x{:02X} should stay 0xFF",
                0x12 + i
            );
        }
        for (i, b) in got[0x18..PACKET_SIZE].iter().enumerate() {
            assert_eq!(
                *b,
                0xFF,
                "trailing byte 0x{:02X} should stay 0xFF",
                0x18 + i
            );
        }
    }

    #[test]
    fn patch_ip_rewrites_only_ip_bytes() {
        let pkt = DiscoveryPacket::from_device(
            [0x00; 6],
            [0, 0, 0, 0],
            0x0A,
            0,
            0,
            0x64,
            0,
            "minidsp-open".into(),
        );
        let mut buf = pkt.encode();
        DiscoveryPacket::patch_ip(&mut buf, [192, 168, 10, 5]);
        assert_eq!(&buf[14..18], &[192, 168, 10, 5]);
        assert_eq!(buf[0x12], 0x0A);
        assert_eq!(buf[0x15], 0x64);
    }

    #[test]
    fn roundtrip_new_format() {
        let pkt = DiscoveryPacket::from_device(
            [0x00, 0x1B, 0x59, 0x12, 0x34, 0x56],
            [192, 168, 1, 100],
            0x0A,
            0x01,
            0x85,
            0x64,
            0xDEAD,
            "MiniDSP-2x4-HD".into(),
        );
        let encoded = pkt.encode();
        assert_eq!(encoded.len(), PACKET_SIZE);
        let decoded = DiscoveryPacket::decode(&encoded).unwrap();
        assert_eq!(decoded.mac, [0x00, 0x1B, 0x59, 0x12, 0x34, 0x56]);
        assert_eq!(decoded.ip, [192, 168, 1, 100]);
        assert_eq!(decoded.hw_id, 0x0A);
        assert_eq!(decoded.hw_id_sub_a, 0x01);
        assert_eq!(decoded.hw_id_sub_b, 0x85);
        assert_eq!(decoded.dsp_no, 0x64);
        assert_eq!(decoded.serial, 0xDEAD);
        assert_eq!(decoded.hostname, "MiniDSP-2x4-HD");
    }

    #[test]
    fn decode_rejects_short_packet() {
        assert!(DiscoveryPacket::decode(&[0x80, 0x12, 0x05, 0xA0]).is_none());
    }

    #[test]
    fn decode_rejects_wrong_magic() {
        let mut bad = [0xFFu8; PACKET_SIZE];
        bad[0] = 0x80;
        bad[2] = 0x06;
        bad[3] = 0xA0;
        assert!(DiscoveryPacket::decode(&bad).is_none());
    }

    #[test]
    fn serial_helper_matches_legacy_transform() {
        assert_eq!(serial_to_u16(912345), 12345);
        assert_eq!(serial_to_u16(900000), 0);
    }

    #[test]
    fn long_hostname_is_truncated() {
        let long = "X".repeat(200);
        let pkt = DiscoveryPacket::from_device([0; 6], [0, 0, 0, 0], 0, 0, 0, 0, 0, long);
        let encoded = pkt.encode();
        assert_eq!(encoded.len(), PACKET_SIZE);
        assert_eq!(encoded[0x23] as usize, MAX_NAME_LEN);
    }
}
