//! UDP discovery packet encoding/decoding (port 3999).
//!
//! Wire format: 36+ bytes, broadcast every 1 second.
//! Used by Device Console and minidsp-rs for auto-discovery.

/// Discovery packet magic header.
pub const MAGIC: [u8; 4] = [0x80, 0x00, 0x05, 0xA0];

/// Minimum discovery packet size.
pub const MIN_PACKET_SIZE: usize = 36;

/// Parsed/buildable discovery packet.
#[derive(Debug, Clone)]
pub struct DiscoveryPacket {
    pub mac: [u8; 6],
    pub ip: [u8; 4],
    pub hw_id: u8,
    pub fw_major: u8,
    pub fw_minor: u8,
    pub dsp_id: u8,
    pub serial: u16,
    pub hostname: alloc::string::String,
}

extern crate alloc;

impl DiscoveryPacket {
    /// Build a discovery packet from device info.
    /// Serial is encoded as `((raw_serial - 900000) & 0xFFFF)`.
    /// MAC is synthetic: `[0xFF, 0xFF, ip[0], ip[1], ip[2], ip[3]]`.
    pub fn from_device(
        ip: [u8; 4],
        hw_id: u8,
        fw_major: u8,
        fw_minor: u8,
        dsp_id: u8,
        raw_serial: u32,
        hostname: alloc::string::String,
    ) -> Self {
        Self {
            mac: [0xFF, 0xFF, ip[0], ip[1], ip[2], ip[3]],
            ip,
            hw_id,
            fw_major,
            fw_minor,
            dsp_id,
            serial: ((raw_serial.wrapping_sub(900000)) & 0xFFFF) as u16,
            hostname,
        }
    }

    /// Encode to wire bytes.
    pub fn encode(&self) -> alloc::vec::Vec<u8> {
        let hostname_bytes = &self.hostname.as_bytes()[..self.hostname.len().min(255)];
        let mut buf = alloc::vec![0u8; 36 + hostname_bytes.len()];

        buf[0..4].copy_from_slice(&MAGIC);
        buf[6..12].copy_from_slice(&self.mac);
        buf[14..18].copy_from_slice(&self.ip);
        buf[18] = self.hw_id;
        buf[19] = self.fw_major;
        buf[20] = self.fw_minor;
        buf[21] = self.dsp_id;
        buf[22] = (self.serial >> 8) as u8;
        buf[23] = (self.serial & 0xFF) as u8;
        buf[35] = hostname_bytes.len() as u8;
        buf[36..36 + hostname_bytes.len()].copy_from_slice(hostname_bytes);

        buf
    }

    /// Parse from wire bytes.
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
        let hostname_len = data[35] as usize;
        let hostname = if data.len() >= 36 + hostname_len {
            alloc::string::String::from_utf8_lossy(&data[36..36 + hostname_len])
                .trim_end_matches('\0')
                .into()
        } else {
            alloc::string::String::new()
        };

        Some(Self {
            mac,
            ip,
            hw_id: data[18],
            fw_major: data[19],
            fw_minor: data[20],
            dsp_id: data[21],
            serial: u16::from_be_bytes([data[22], data[23]]),
            hostname,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let pkt = DiscoveryPacket::from_device(
            [192, 168, 1, 100],
            10,
            1,
            85,
            100,
            912345,
            "minidsp-2x4hd".into(),
        );
        let encoded = pkt.encode();
        assert!(encoded.len() >= MIN_PACKET_SIZE);
        let decoded = DiscoveryPacket::decode(&encoded).unwrap();
        assert_eq!(decoded.ip, [192, 168, 1, 100]);
        assert_eq!(decoded.hw_id, 10);
        assert_eq!(decoded.dsp_id, 100);
        assert_eq!(decoded.hostname, "minidsp-2x4hd");
    }
}
