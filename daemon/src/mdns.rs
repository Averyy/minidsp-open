//! mDNS service advertisement — `_http._tcp.local.` for local network discovery.
//!
//! Advertises the daemon as a Wi-DG-style HTTP service so Device Console, web
//! browsers, and other zero-config clients can reach `http://minidsp.local/`
//! without knowing the IP address. The hostname is fixed to `minidsp` so
//! `minidsp.local` is the canonical URL regardless of how many daemons live
//! on the network; mdns-sd handles conflict resolution per RFC 6762 §9.

use mdns_sd::{ServiceDaemon, ServiceInfo};
use std::collections::HashMap;
use std::net::IpAddr;

const SERVICE_TYPE: &str = "_http._tcp.local.";
const HOSTNAME: &str = "minidsp";

/// Register the daemon's HTTP API as an `_http._tcp` mDNS service.
///
/// `instance_suffix` is an optional disambiguator appended to the instance
/// name so multiple daemons on one LAN can coexist in the service browser.
/// Typical values are the connected device's serial or the process id.
///
/// Returns the [`ServiceDaemon`] handle; it must be kept alive for the
/// duration of the advertisement (drop-on-shutdown is fine).
pub fn advertise(
    port: u16,
    instance_suffix: Option<String>,
) -> Result<ServiceDaemon, Box<dyn std::error::Error>> {
    let mdns = ServiceDaemon::new()?;

    let instance_name = match instance_suffix {
        Some(suffix) if !suffix.is_empty() => format!("minidsp-open ({suffix})"),
        _ => "minidsp-open".to_string(),
    };

    let mut properties = HashMap::new();
    properties.insert("path".to_string(), "/".to_string());

    // Collect local non-loopback IPv4 addresses. mdns-sd does NOT auto-populate
    // from `""`; without an explicit address set (or `enable_addr_auto`) the
    // responder answers every A-record query with "Cannot find valid addrs".
    let ipv4s: Vec<IpAddr> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| !i.is_loopback())
        .filter_map(|i| match i.ip() {
            std::net::IpAddr::V4(v4) if !v4.is_link_local() => Some(IpAddr::V4(v4)),
            _ => None,
        })
        .collect();

    let service = ServiceInfo::new(
        SERVICE_TYPE,
        &instance_name,
        &format!("{HOSTNAME}.local."),
        &ipv4s[..],
        port,
        properties,
    )?
    .enable_addr_auto();

    mdns.register(service)?;
    tracing::info!("mDNS: advertising {instance_name} as {HOSTNAME}.local:{port} ({SERVICE_TYPE})");

    Ok(mdns)
}
