//! mDNS service advertisement — `_minidsp._tcp` for local network discovery.
//!
//! Advertises the daemon's HTTP API so that clients can find it without
//! knowing the IP address. Uses the `mdns-sd` crate.

use mdns_sd::{ServiceDaemon, ServiceInfo};
use std::collections::HashMap;

const SERVICE_TYPE: &str = "_minidsp._tcp.local.";

/// Register the daemon's HTTP API as an mDNS service.
///
/// Returns the ServiceDaemon handle (must be kept alive for the duration of the advertisement).
pub fn advertise(
    port: u16,
    serial: u32,
    product_name: &str,
    hw_id: u8,
    dsp_version: u8,
) -> Result<ServiceDaemon, Box<dyn std::error::Error>> {
    let mdns = ServiceDaemon::new()?;

    // Generate hostname from serial number: minidsp-{serial}.local
    let hostname = format!("minidsp-{serial}");
    let instance_name = format!("{product_name} ({serial})");

    let mut properties = HashMap::new();
    properties.insert("hw_id".to_string(), hw_id.to_string());
    properties.insert("dsp_version".to_string(), dsp_version.to_string());
    properties.insert("serial".to_string(), serial.to_string());
    properties.insert("product".to_string(), product_name.to_string());

    let service = ServiceInfo::new(
        SERVICE_TYPE,
        &instance_name,
        &format!("{hostname}.local."),
        "",  // Let mdns-sd determine IP
        port,
        properties,
    )?;

    mdns.register(service)?;
    tracing::info!("mDNS: advertising {instance_name} on port {port} as {hostname}.local");

    Ok(mdns)
}
