//! Browser-origin policy shared by the blocking and async HTTP servers.

use std::net::{IpAddr, Ipv6Addr};

/// Native clients omit Origin. Browser requests pass when their origin is a
/// local UI, an IP-literal origin addresses the same IP literal, or the origin
/// is explicitly configured. No DNS lookup is performed.
///
/// A same-authority request under a public DNS name is refused on purpose: DNS
/// rebinding points an attacker's name at the device, which makes Origin and
/// Host agree. Rebinding needs a DNS name, so names that only resolve locally
/// (single-label, `.local`, home suffixes) and IP literals stay allowed.
pub fn origin_allowed(origin: Option<&str>, host: Option<&str>, configured: Option<&str>) -> bool {
    let Some(origin) = origin else { return true };
    if configured == Some("*") {
        return true;
    }
    let Some((scheme, authority)) = origin.split_once("://") else {
        return false;
    };
    let default_port = match scheme {
        "http" => 80,
        "https" => 443,
        _ => return false,
    };
    let Some((origin_host, port)) = parse_authority(authority, default_port) else {
        return false;
    };
    if configured == Some(origin) || local_name(origin_host) {
        return true;
    }
    match origin_host.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) if ip.is_loopback() || ip.is_private() || ip.is_link_local() => true,
        Ok(IpAddr::V6(ip)) if local_ipv6(ip) => true,
        // Any IP literal may talk to itself: no DNS, so no rebinding.
        Ok(origin_ip) => host
            .and_then(|h| parse_authority(h, default_port))
            .is_some_and(|(request_host, request_port)| {
                request_port == port && request_host.parse::<IpAddr>() == Ok(origin_ip)
            }),
        Err(_) => false,
    }
}

/// Names a public DNS rebinding attack can't use: `localhost`, single-label
/// hosts, mDNS `.local`, and the non-delegated home suffixes.
fn local_name(host: &str) -> bool {
    if host.parse::<IpAddr>().is_ok() {
        return false;
    }
    let name = host.trim_end_matches('.').to_ascii_lowercase();
    !name.contains('.')
        || [
            ".local",
            ".lan",
            ".home",
            ".home.arpa",
            ".internal",
            ".localdomain",
        ]
        .iter()
        .any(|suffix| name.ends_with(suffix))
}

fn local_ipv6(ip: Ipv6Addr) -> bool {
    ip.is_loopback()
        || ip.segments()[0] & 0xfe00 == 0xfc00
        || ip.segments()[0] & 0xffc0 == 0xfe80
        || ip
            .to_ipv4_mapped()
            .is_some_and(|ip| ip.is_loopback() || ip.is_private() || ip.is_link_local())
}

fn parse_authority(authority: &str, default_port: u16) -> Option<(&str, u16)> {
    let (host, port) = if let Some(ipv6) = authority.strip_prefix('[') {
        let (host, suffix) = ipv6.split_once(']')?;
        host.parse::<Ipv6Addr>().ok()?;
        (
            host,
            if suffix.is_empty() {
                None
            } else {
                Some(suffix.strip_prefix(':')?)
            },
        )
    } else {
        let (host, port) = authority
            .split_once(':')
            .map_or((authority, None), |(h, p)| (h, Some(p)));
        if !host.trim_end_matches('.').split('.').all(|label| {
            !label.is_empty()
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        }) {
            return None;
        }
        (host, port)
    };
    let port = match port {
        Some(p) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => p.parse().ok()?,
        Some(_) => return None,
        None => default_port,
    };
    Some((host, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_and_same_authority() {
        assert!(origin_allowed(None, Some("nas:5380"), None));
        for (origin, host) in [
            ("http://nas:5380", "nas:5380"),
            ("http://NAS:5380", "nas:5380"),
            ("http://nas", "nas:80"),
            ("https://nas", "nas:443"),
            ("http://[2001:db8::1]:5380", "[2001:0db8::1]:5380"),
        ] {
            assert!(origin_allowed(Some(origin), Some(host), None), "{origin}");
        }
        // A public IP literal may address itself; a different IP may not.
        assert!(!origin_allowed(
            Some("http://[2001:db8::1]:5380"),
            Some("[2001:db8::2]:5380"),
            None
        ));
        assert!(!origin_allowed(
            Some("http://203.0.113.5:5381"),
            Some("203.0.113.5:5380"),
            None
        ));
    }

    #[test]
    fn dns_rebinding_is_refused() {
        // Rebinding makes Origin and Host agree on the attacker's public name.
        for name in ["rebind.example", "minidsp.attacker.example", "evil.com"] {
            let authority = format!("{name}:5380");
            assert!(
                !origin_allowed(Some(&format!("http://{authority}")), Some(&authority), None),
                "{name}"
            );
        }
        // An explicitly configured origin is still honoured.
        assert!(origin_allowed(
            Some("http://minidsp.example.org:5380"),
            Some("minidsp.example.org:5380"),
            Some("http://minidsp.example.org:5380")
        ));
    }

    #[test]
    fn home_network_names() {
        for host in [
            "nas",
            "minidsp",
            "minidsp.lan",
            "minidsp.home",
            "minidsp.home.arpa",
            "minidsp.internal",
            "minidsp.localdomain",
            "MiniDSP.LAN.",
        ] {
            assert!(
                origin_allowed(
                    Some(&format!("http://{host}:5380")),
                    Some("other:5380"),
                    None
                ),
                "{host}"
            );
        }
        for host in [
            "minidsp.lan.example",
            "home.example",
            "internal.example.com",
        ] {
            assert!(
                !origin_allowed(
                    Some(&format!("http://{host}:5380")),
                    Some(&format!("{host}:5380")),
                    None
                ),
                "{host}"
            );
        }
    }

    #[test]
    fn local_uis() {
        for host in [
            "localhost",
            "127.0.0.2",
            "10.0.0.1",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.0.1",
            "169.254.1.1",
            "[::1]",
            "[fc00::1]",
            "[fdff::1]",
            "[fe80::1]",
            "[febf::1]",
            "[::ffff:192.168.1.1]",
            "board.local",
            "BOARD.LOCAL.",
        ] {
            assert!(
                origin_allowed(Some(&format!("http://{host}:8080")), Some("nas:5380"), None),
                "{host}"
            );
        }
    }

    #[test]
    fn hostile_opaque_and_malformed_origins() {
        for origin in [
            "null",
            "",
            "https://evil.example",
            "http://rebind.example:5380",
            "http://localhost.evil.example",
            "http://board.local.evil.example",
            "http://192.168.1.1.evil.example",
            "http://172.32.0.1",
            "http://8.8.8.8",
            "http://[2001:db8::1]",
            "http://[fec0::1]",
            "http://[::]",
            "http://localhost@evil.example",
            "http://localhost/path",
            "http://localhost?x",
            "http://localhost#x",
            "http://localhost:99999",
            "http://localhost:",
            "http://localhost:80:90",
            "http://localhost https://evil.example",
            "file://localhost",
            "http://[::1",
            "http://localhost\r\nX: y",
        ] {
            assert!(
                !origin_allowed(Some(origin), Some("nas:5380"), None),
                "{origin}"
            );
        }
    }

    #[test]
    fn explicit_configuration() {
        assert!(origin_allowed(
            Some("https://ui.example"),
            Some("nas:5380"),
            Some("https://ui.example")
        ));
        assert!(!origin_allowed(
            Some("https://evil.example"),
            None,
            Some("https://ui.example")
        ));
        assert!(!origin_allowed(Some("null"), None, Some("null")));
        assert!(origin_allowed(
            Some("https://evil.example"),
            None,
            Some("*")
        ));
        assert!(origin_allowed(Some("null"), None, Some("*")));
    }
}
