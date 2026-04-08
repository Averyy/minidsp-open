//! UDP discovery broadcast (port 3999) — Device Console compatibility.

use std::net::{Ipv4Addr, SocketAddr};
use tokio::net::UdpSocket;
use tokio::time::{Duration, interval};

// Re-export from protocol crate — single source of truth for packet format.
pub use minidsp_protocol::discovery::DiscoveryPacket;

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

    let packet = DiscoveryPacket::from_device(
        ip.octets(),
        hw_id,
        fw_major,
        fw_minor,
        dsp_id,
        serial,
        hostname,
    );
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

// Tests moved to protocol/src/discovery.rs — single source of truth.
