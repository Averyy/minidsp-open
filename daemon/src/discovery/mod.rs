//! UDP discovery broadcast (port 3999) — Wi-DG beacon compatibility.
//!
//! Emits a 72-byte beacon every 5 seconds on every non-loopback IPv4
//! interface, rewriting the IP bytes per-interface just like the real Wi-DG
//! firmware does. When no MiniDSP is attached, the daemon still broadcasts
//! an appliance-mode beacon so Device Console can find the bridge and open a
//! TCP session later once a DSP is plugged in.

use crate::state::DaemonState;
use minidsp_protocol::device::probe;
use minidsp_protocol::discovery::{self, DiscoveryPacket};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::time::{Duration, interval};

pub use minidsp_protocol::discovery::DiscoveryPacket as ProtocolDiscoveryPacket;

/// Run the UDP discovery broadcaster.
///
/// Polls [`DaemonState`] on every tick so hot-plugging a USB device flips the
/// beacon from appliance mode to full-device mode (and vice versa) without
/// requiring a daemon restart. `override_ip` forces all per-interface sends
/// to advertise that IP regardless of the interface's own address — useful
/// when the daemon sits behind a NAT and needs to report its public IP.
pub async fn broadcast_loop(
    state: Arc<DaemonState>,
    override_ip: Option<Ipv4Addr>,
) -> std::io::Result<()> {
    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    socket.set_broadcast(true)?;

    let primary_mac = resolve_primary_mac();
    tracing::info!(
        "UDP discovery broadcaster started (mac: {}, interval: 5s)",
        format_mac(&primary_mac)
    );

    let mut tick = interval(Duration::from_secs(5));
    loop {
        tick.tick().await;

        let beacon = build_beacon(&state, primary_mac).await;

        let interfaces = if_addrs::get_if_addrs().unwrap_or_default();
        let mut sent_any = false;
        for iface in &interfaces {
            if iface.is_loopback() {
                continue;
            }
            let if_addrs::IfAddr::V4(v4) = &iface.addr else {
                continue;
            };
            let broadcast = v4.broadcast.unwrap_or(Ipv4Addr::BROADCAST);
            let advertise_ip = override_ip.unwrap_or(v4.ip);

            let mut packet = beacon.clone();
            DiscoveryPacket::patch_ip(&mut packet, advertise_ip.octets());

            let target = SocketAddrV4::new(broadcast, discovery::DISCOVERY_PORT);
            match socket.send_to(&packet, target).await {
                Ok(_) => sent_any = true,
                Err(e) => tracing::debug!(
                    "UDP discovery: send to {target} via {} failed: {e}",
                    iface.name
                ),
            }
        }

        if !sent_any {
            let mut packet = beacon.clone();
            if let Some(ip) = override_ip {
                DiscoveryPacket::patch_ip(&mut packet, ip.octets());
            }
            let target = SocketAddrV4::new(Ipv4Addr::BROADCAST, discovery::DISCOVERY_PORT);
            let _ = socket.send_to(&packet, target).await;
        }
    }
}

async fn build_beacon(state: &DaemonState, mac: [u8; 6]) -> Vec<u8> {
    if let Some(client) = state.get_device(0).await {
        let hw = client.hw_id();
        // bytes 0x13/0x14 are "hw_id sub-bytes" in the Wi-DG beacon. The
        // existing minidsp-open HwId struct carries them as fw_major/fw_minor
        // because that's how the legacy minidsp-rs probe interpreted the
        // response to opcode 0x14 — the underlying bytes are identical, so
        // we just re-label them on the way out.
        let product = probe::product_name(hw.hw_id, client.dsp_version());
        let pkt = DiscoveryPacket::from_device(
            mac,
            [0, 0, 0, 0],
            hw.hw_id,
            hw.fw_major,
            hw.fw_minor,
            client.dsp_version(),
            discovery::serial_to_u16(client.serial()),
            product.to_string(),
        );
        pkt.encode()
    } else {
        DiscoveryPacket::encode_appliance(mac, [0, 0, 0, 0])
    }
}

fn resolve_primary_mac() -> [u8; 6] {
    match mac_address::get_mac_address() {
        Ok(Some(addr)) => addr.bytes(),
        _ => [0x00, 0x1B, 0x59, 0x00, 0x00, 0x00],
    }
}

fn format_mac(mac: &[u8; 6]) -> String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}
