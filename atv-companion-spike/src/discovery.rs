use std::{collections::BTreeMap, net::IpAddr, str::FromStr, time::Duration};

use anyhow::{Context, Result};
use mdns_sd::{ServiceDaemon, ServiceEvent};

use crate::model::{DiscoveredDevice, now_millis};

const COMPANION_SERVICE: &str = "_companion-link._tcp.local.";

pub fn scan_for_devices(timeout: Duration) -> Result<Vec<DiscoveredDevice>> {
    let mdns = ServiceDaemon::new().context("failed to create mDNS daemon")?;
    let receiver = mdns
        .browse(COMPANION_SERVICE)
        .with_context(|| format!("failed to browse {COMPANION_SERVICE}"))?;

    let start = std::time::Instant::now();
    let mut devices = BTreeMap::<String, DiscoveredDevice>::new();

    while start.elapsed() < timeout {
        let remaining = timeout.saturating_sub(start.elapsed());
        match receiver.recv_timeout(remaining) {
            Ok(ServiceEvent::ServiceResolved(info)) => {
                let mut txt = BTreeMap::new();
                for property in info.get_properties().iter() {
                    txt.insert(property.key().to_owned(), property.val_str().to_owned());
                }

                let addresses = info
                    .get_addresses()
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>();

                let hostname = info.get_hostname().trim_end_matches('.').to_owned();
                let device_id = txt
                    .get("rpMRtID")
                    .cloned()
                    .or_else(|| (!hostname.is_empty()).then_some(hostname.clone()))
                    .or_else(|| txt.get("rpHN").cloned())
                    .or_else(|| txt.get("UniqueIdentifier").cloned())
                    .or_else(|| txt.get("rpHI").cloned())
                    .unwrap_or_else(|| info.get_fullname().to_owned());

                let connect_host =
                    preferred_connect_host(&addresses).unwrap_or_else(|| hostname.clone());

                let incoming = DiscoveredDevice {
                    id: device_id.clone(),
                    name: info.get_fullname().trim_end_matches(".local.").to_owned(),
                    host: hostname,
                    connect_host,
                    port: info.get_port(),
                    addresses,
                    txt,
                    last_seen_ms: now_millis(),
                };

                devices
                    .entry(device_id)
                    .and_modify(|existing| merge_device(existing, &incoming))
                    .or_insert(incoming);
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }

    drop(receiver);
    if let Ok(shutdown) = mdns.shutdown() {
        let _ = shutdown.recv_timeout(Duration::from_millis(250));
    }
    Ok(devices.into_values().collect())
}

fn merge_device(existing: &mut DiscoveredDevice, incoming: &DiscoveredDevice) {
    existing.last_seen_ms = existing.last_seen_ms.max(incoming.last_seen_ms);

    for address in &incoming.addresses {
        if !existing.addresses.contains(address) {
            existing.addresses.push(address.clone());
        }
    }
    existing.addresses = sorted_addresses(&existing.addresses);

    if let Some(preferred) = preferred_connect_host(&existing.addresses) {
        existing.connect_host = preferred;
    }

    if existing.host.is_empty() && !incoming.host.is_empty() {
        existing.host = incoming.host.clone();
    }
    if existing.name.is_empty() && !incoming.name.is_empty() {
        existing.name = incoming.name.clone();
    }

    for (key, value) in &incoming.txt {
        existing
            .txt
            .entry(key.clone())
            .or_insert_with(|| value.clone());
    }
}

fn preferred_connect_host(addresses: &[String]) -> Option<String> {
    let sorted = sorted_addresses(addresses);
    let parsed = sorted
        .iter()
        .filter_map(|address| IpAddr::from_str(address).ok().map(|ip| (address, ip)))
        .collect::<Vec<_>>();

    parsed
        .iter()
        .find_map(|(raw, ip)| match ip {
            IpAddr::V4(_) => Some((*raw).clone()),
            _ => None,
        })
        .or_else(|| {
            parsed.iter().find_map(|(raw, ip)| match ip {
                IpAddr::V6(ipv6) if !ipv6.is_unicast_link_local() => Some((*raw).clone()),
                _ => None,
            })
        })
        .or_else(|| sorted.first().cloned())
}

fn sorted_addresses(addresses: &[String]) -> Vec<String> {
    let mut values = addresses.to_vec();
    values.sort_by_key(|address| address_rank(address));
    values.dedup();
    values
}

fn address_rank(address: &str) -> u8 {
    match IpAddr::from_str(address) {
        Ok(IpAddr::V4(ipv4)) if ipv4.is_loopback() => 2,
        Ok(IpAddr::V4(_)) => 0,
        Ok(IpAddr::V6(ipv6)) if ipv6.is_unicast_link_local() => 4,
        Ok(IpAddr::V6(ipv6)) if ipv6.is_loopback() => 3,
        Ok(IpAddr::V6(_)) => 1,
        Err(_) => 5,
    }
}
