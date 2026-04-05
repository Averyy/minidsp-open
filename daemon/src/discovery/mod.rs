//! UDP discovery broadcast (port 3999) — Device Console compatibility.

use std::net::{Ipv4Addr, SocketAddr};
use tokio::net::UdpSocket;
use tokio::time::{Duration, interval};

/// Discovery packet magic header.
const MAGIC: [u8; 4] = [0x80, 0x00, 0x05, 0xA0];

/// Minimum discovery packet size.
const MIN_PACKET_SIZE: usize = 36;

/// Parsed discovery packet.
#[derive(Debug, Clone)]
pub struct DiscoveryPacket {
    pub mac: [u8; 6],
    pub ip: Ipv4Addr,
    pub hw_id: u8,
    pub fw_major: u8,
    pub fw_minor: u8,
    pub dsp_id: u8,
    pub serial: u16,
    pub hostname: String,
}

impl DiscoveryPacket {
    /// Encode to wire bytes.
    pub fn encode(&self) -> Vec<u8> {
        let hostname_bytes = self.hostname.as_bytes();
        let mut buf = vec![0u8; 36 + hostname_bytes.len()];

        // Magic header
        buf[0..4].copy_from_slice(&MAGIC);
        // MAC
        buf[6..12].copy_from_slice(&self.mac);
        // IP
        buf[14..18].copy_from_slice(&self.ip.octets());
        // Device info
        buf[18] = self.hw_id;
        buf[19] = self.fw_major;
        buf[20] = self.fw_minor;
        buf[21] = self.dsp_id;
        // Serial (big-endian u16)
        buf[22] = (self.serial >> 8) as u8;
        buf[23] = (self.serial & 0xFF) as u8;
        // Hostname
        buf[35] = hostname_bytes.len() as u8;
        buf[36..36 + hostname_bytes.len()].copy_from_slice(hostname_bytes);

        buf
    }

    /// Parse from wire bytes.
    pub fn decode(data: &[u8]) -> Option<Self> {
        if data.len() < MIN_PACKET_SIZE {
            return None;
        }
        // Check magic (bytes 0, 2, 3 — byte 1 varies)
        if data[0] != 0x80 || data[2] != 0x05 || data[3] != 0xA0 {
            return None;
        }

        let mut mac = [0u8; 6];
        mac.copy_from_slice(&data[6..12]);
        let ip = Ipv4Addr::new(data[14], data[15], data[16], data[17]);
        let hostname_len = data[35] as usize;
        let hostname = if data.len() >= 36 + hostname_len {
            String::from_utf8_lossy(&data[36..36 + hostname_len])
                .trim_end_matches('\0')
                .to_string()
        } else {
            String::new()
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

/// Run the UDP discovery broadcaster.
pub async fn broadcast_loop(
    ip: Ipv4Addr,
    hw_id: u8,
    fw_major: u8,
    fw_minor: u8,
    dsp_id: u8,
    serial: u32,
    hostname: String,
) -> std::io::Result<()> {
    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    socket.set_broadcast(true)?;

    // Encode serial for UDP: (serial - 900000) & 0xFFFF
    let serial_encoded = ((serial.wrapping_sub(900000)) & 0xFFFF) as u16;

    // Synthetic MAC: FF:FF:ip[0]:ip[1]:ip[2]:ip[3]
    let ip_octets = ip.octets();
    let mac = [0xFF, 0xFF, ip_octets[0], ip_octets[1], ip_octets[2], ip_octets[3]];

    let packet = DiscoveryPacket {
        mac,
        ip,
        hw_id,
        fw_major,
        fw_minor,
        dsp_id,
        serial: serial_encoded,
        hostname,
    };
    let data = packet.encode();

    let broadcast_addr: SocketAddr = "255.255.255.255:3999".parse().unwrap();
    let multicast_addr: SocketAddr = "224.0.0.250:3999".parse().unwrap();

    let mut tick = interval(Duration::from_secs(1));
    loop {
        tick.tick().await;
        // Send to both broadcast and multicast for maximum compatibility
        let _ = socket.send_to(&data, broadcast_addr).await;
        let _ = socket.send_to(&data, multicast_addr).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_roundtrip() {
        let packet = DiscoveryPacket {
            mac: [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF],
            ip: Ipv4Addr::new(192, 168, 1, 100),
            hw_id: 10,
            fw_major: 1,
            fw_minor: 85,
            dsp_id: 100,
            serial: 12345,
            hostname: "2x4HD".to_string(),
        };
        let encoded = packet.encode();
        let decoded = DiscoveryPacket::decode(&encoded).unwrap();
        assert_eq!(decoded.mac, packet.mac);
        assert_eq!(decoded.ip, packet.ip);
        assert_eq!(decoded.hw_id, 10);
        assert_eq!(decoded.dsp_id, 100);
        assert_eq!(decoded.hostname, "2x4HD");
    }

    #[test]
    fn parse_real_device_capture() {
        // From minidsp-rs test suite
        let data = [
            0x80, 0x12, 0x05, 0xA0, 0x00, 0x00, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF,
            0x00, 0x00, 0xC0, 0xA8, 0x79, 0x8D, 0xDE, 0x03, 0x00, 0x33, 0xAB, 0x64,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xAC, 0x00, 0x00, 0x00, 0x00, 0x08,
            0x50, 0x52, 0x4F, 0x44, 0x55, 0x43, 0x54, 0x00,
        ];
        let packet = DiscoveryPacket::decode(&data).unwrap();
        assert_eq!(packet.ip, Ipv4Addr::new(192, 168, 121, 141));
        assert_eq!(packet.hw_id, 0xDE);
        assert_eq!(packet.fw_major, 3);
        assert_eq!(packet.dsp_id, 0x33);
        assert_eq!(packet.hostname, "PRODUCT");
    }
}
