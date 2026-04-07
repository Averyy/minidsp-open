//! CLI command implementations.

use crate::{
    Cli, ChannelCommand, PeqCommand, CrossoverCommand, CompressorCommand, FirCommand,
    apply_force_kind, connect, connect_all_local, discover_daemon_tcp,
    discover_network_devices,
};
#[cfg(unix)]
use crate::discover_daemon_unix;
use minidsp::client::MiniDspClient;
use minidsp::transport::hid;
use minidsp::transport::tcp::TcpTransport;
use minidsp::transport::ws::WsTransport;
use minidsp_daemon::http::json::DeviceJson;
use minidsp_protocol::device::probe;
use minidsp_protocol::status::DeviceInfo;
use minidsp_protocol::source::Source;
use crate::output;

/// TOML config file structure (minidsp-rs compatible).
#[derive(Debug, serde::Deserialize, Default)]
#[serde(default)]
struct DaemonConfig {
    http_server: Option<HttpServerConfig>,
    tcp_server: Option<TcpServerConfig>,
    advertise: Option<AdvertiseConfig>,
}

#[derive(Debug, serde::Deserialize, Default)]
#[serde(default)]
struct HttpServerConfig {
    bind_address: Option<String>,
}

#[derive(Debug, serde::Deserialize, Default)]
#[serde(default)]
struct TcpServerConfig {
    bind_address: Option<String>,
}

#[derive(Debug, serde::Deserialize, Default)]
#[serde(default)]
struct AdvertiseConfig {
    hostname: Option<String>,
    ip: Option<String>,
}

fn print_probe_device(
    index: usize,
    product_name: &str,
    serial: impl std::fmt::Display,
    url: &str,
    hw_id: impl std::fmt::Display,
    dsp_version: impl std::fmt::Display,
) {
    println!(
        "{index}: Found {product_name} with serial {serial} at {url} [hw_id: {hw_id}, dsp_version: {dsp_version}]"
    );
}

trait ProbeDeviceView {
    fn product_name(&self) -> &str;
    fn serial(&self) -> u32;
    fn url(&self) -> &str;
    fn hw_id(&self) -> u8;
    fn dsp_version(&self) -> u8;
}

impl ProbeDeviceView for DeviceInfo {
    fn product_name(&self) -> &str {
        &self.product_name
    }

    fn serial(&self) -> u32 {
        self.version.serial
    }

    fn url(&self) -> &str {
        &self.url
    }

    fn hw_id(&self) -> u8 {
        self.version.hw_id
    }

    fn dsp_version(&self) -> u8 {
        self.version.dsp_version
    }
}

impl ProbeDeviceView for DeviceJson {
    fn product_name(&self) -> &str {
        &self.product_name
    }

    fn serial(&self) -> u32 {
        self.version.serial
    }

    fn url(&self) -> &str {
        &self.url
    }

    fn hw_id(&self) -> u8 {
        self.version.hw_id
    }

    fn dsp_version(&self) -> u8 {
        self.version.dsp_version
    }
}

fn print_discovered_devices<T: ProbeDeviceView>(devices: &[T]) -> bool {
    for (i, dev) in devices.iter().enumerate() {
        print_probe_device(
            i,
            dev.product_name(),
            dev.serial(),
            dev.url(),
            dev.hw_id(),
            dev.dsp_version(),
        );
    }
    !devices.is_empty()
}

/// Print device status.
pub async fn status(client: &MiniDspClient, format: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let summary = client.get_status_summary().await?;

    match format {
        Some("jsonline") => output::print_status_json(&summary, false)?,
        Some("json") => output::print_status_json(&summary, true)?,
        _ => output::print_status_text(&summary, client.profile()),
    }
    Ok(())
}

/// Enumerate connected devices.
pub async fn probe(cli: &Cli) -> Result<(), Box<dyn std::error::Error>> {
    let mut printed = false;
    let env_sock = std::env::var("MINIDSP_SOCK").ok();
    let env_tcp = std::env::var("MINIDSP_TCP").ok();
    let env_url = std::env::var("MINIDSP_URL").ok();
    let env_daemon_url = std::env::var("MINIDSPD_URL").ok();
    let env_usb = std::env::var("MINIDSP_USB").ok();

    if let Some(path) = cli.daemon_sock.as_deref().or(env_sock.as_deref()) {
        #[cfg(unix)]
        {
            let devices = discover_daemon_unix(path).await?;
            printed = print_discovered_devices(&devices);
        }
    } else if let Some(addr) = cli.tcp.as_deref().or(env_tcp.as_deref()) {
        let client = MiniDspClient::connect(Box::new(TcpTransport::connect(addr).await?)).await?;
        let info = client.device_info();
        print_probe_device(0, &info.product_name, info.version.serial, &info.url, info.version.hw_id, info.version.dsp_version);
        printed = true;
    } else if let Some(url) = cli.url.as_deref().or(env_url.as_deref()) {
        if url.starts_with("tcp://") {
            let addr = &url["tcp://".len()..];
            let client = MiniDspClient::connect(Box::new(TcpTransport::connect(addr).await?)).await?;
            let info = client.device_info();
            print_probe_device(0, &info.product_name, info.version.serial, &info.url, info.version.hw_id, info.version.dsp_version);
            printed = true;
        } else if (url.starts_with("http://") || url.starts_with("ws://")) && !url.ends_with("/ws") {
            let devices = discover_daemon_tcp(url).await?;
            printed = print_discovered_devices(&devices);
        } else if url.starts_with("http://") || url.starts_with("ws://") {
            let client = MiniDspClient::connect(Box::new(WsTransport::connect(url, 0).await?)).await?;
            let info = client.device_info();
            print_probe_device(0, &info.product_name, info.version.serial, &info.url, info.version.hw_id, info.version.dsp_version);
            printed = true;
        } else if url.starts_with("usb://") || url.starts_with("usb:") {
            let path = if let Some(rest) = url.strip_prefix("usb://") {
                urlencoding::decode(rest.split('?').next().unwrap_or(rest))?.into_owned()
            } else {
                let rest = &url["usb:".len()..];
                urlencoding::decode(rest.split('?').next().unwrap_or(rest))?.into_owned()
            };
            let transport = hid::HidTransport::open(&path)?;
            let client = MiniDspClient::connect(Box::new(transport)).await?;
            let info = client.device_info();
            print_probe_device(0, &info.product_name, info.version.serial, &info.url, info.version.hw_id, info.version.dsp_version);
            printed = true;
        }
    } else if let Some(url) = cli.daemon_url.as_deref().or(env_daemon_url.as_deref()) {
        let devices = discover_daemon_tcp(url).await?;
        printed = print_discovered_devices(&devices);
    } else if let Some(path) = cli.usb.as_deref().or(env_usb.as_deref()) {
        let transport = hid::HidTransport::open(path)?;
        let client = MiniDspClient::connect(Box::new(transport)).await?;
        let info = client.device_info();
        print_probe_device(0, &info.product_name, info.version.serial, &info.url, info.version.hw_id, info.version.dsp_version);
        printed = true;
    } else {
        let devices = hid::enumerate()?;
        for (i, dev) in devices.iter().enumerate() {
            match hid::HidTransport::open(&dev.path) {
                Ok(transport) => {
                    match MiniDspClient::connect(Box::new(transport)).await {
                        Ok(client) => {
                            let name = probe::product_name(client.hw_id().hw_id, client.dsp_version());
                            let url = format!(
                                "usb:{}?vid={:04x}&pid={:04x}",
                                urlencoding::encode(&dev.path),
                                dev.vendor_id,
                                dev.product_id,
                            );
                            print_probe_device(
                                i,
                                &name,
                                client.serial(),
                                &url,
                                client.hw_id().hw_id,
                                client.dsp_version(),
                            );
                        }
                        Err(e) => {
                            println!("{i}: Device at {} (open failed: {e})", dev.path);
                        }
                    }
                }
                Err(e) => {
                    println!("{i}: Device at {} (open failed: {e})", dev.path);
                }
            }
        }
        printed = !devices.is_empty();
    }

    if !printed {
        println!("No MiniDSP devices found");
    }

    if matches!(&cli.command, Some(crate::Command::Probe { net: true })) {
        println!("Probing for network devices...");
        let devices = discover_network_devices(std::time::Duration::from_secs(8))?;
        if devices.is_empty() {
            println!("No network devices detected");
        } else {
            for device in devices {
                println!(
                    "Found: {} ({}) [hw_id: {}, dsp_version: {}, serial: {}]",
                    device.hostname, device.ip, device.hw_id, device.dsp_version, device.serial
                );
            }
        }
    }

    Ok(())
}

/// Start the daemon server.
pub async fn server(bind: &str, config_path: Option<&str>, cli_advertise: Option<&str>, cli_ip: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    use minidsp_daemon::state::DaemonState;
    use minidsp_daemon::http::routes;
    use std::sync::Arc;

    // Load optional TOML config
    let config = if let Some(path) = config_path {
        let contents = std::fs::read_to_string(path)?;
        toml::from_str::<DaemonConfig>(&contents)?
    } else {
        DaemonConfig::default()
    };

    // Override bind address from config if not provided on CLI
    let http_bind = config.http_server.as_ref()
        .and_then(|h| h.bind_address.as_deref())
        .unwrap_or(bind);
    let tcp_bind = config.tcp_server.as_ref()
        .and_then(|t| t.bind_address.as_deref())
        .unwrap_or("0.0.0.0:5333");
    // CLI --advertise overrides config file hostname
    let hostname = cli_advertise
        .or_else(|| config.advertise.as_ref().and_then(|a| a.hostname.as_deref()))
        .unwrap_or("minidsp-open");

    let state = Arc::new(DaemonState::new());

    // Auto-discover and connect USB devices
    let devices = hid::enumerate().unwrap_or_default();
    for (i, dev) in devices.iter().enumerate() {
        match hid::HidTransport::open(&dev.path) {
            Ok(transport) => {
                match MiniDspClient::connect(Box::new(transport)).await {
                    Ok(client) => {
                        let name = probe::product_name(client.hw_id().hw_id, client.dsp_version());
                        tracing::info!("Device {i}: {name} (serial {})", client.serial());
                        state.add_device(i, Arc::new(client)).await;
                    }
                    Err(e) => tracing::warn!("Device {i}: connect failed: {e}"),
                }
            }
            Err(e) => tracing::warn!("Device {i}: open failed: {e}"),
        }
    }

    if state.device_count().await == 0 {
        tracing::warn!("No MiniDSP devices found — starting server anyway");
    }

    let app = routes::router(state.clone());

    // Start TCP bridge
    let tcp_state = state.clone();
    let tcp_addr = tcp_bind.to_string();
    tokio::spawn(async move {
        if let Err(e) = minidsp_daemon::tcp::serve(&tcp_addr, tcp_state).await {
            tracing::error!("TCP bridge error: {e}");
        }
    });

    // Start UDP discovery (if we have a device)
    if let Some(client) = state.get_device(0).await {
        let hw_id = client.hw_id().hw_id;
        let fw_major = client.hw_id().fw_major;
        let fw_minor = client.hw_id().fw_minor;
        let serial = client.serial();
        let dsp = client.dsp_version();
        let disc_hostname = hostname.to_string();
        // CLI --ip overrides config file ip
        let disc_ip = cli_ip
            .and_then(|s| s.parse().ok())
            .or_else(|| config.advertise.as_ref()
                .and_then(|a| a.ip.as_deref())
                .and_then(|s| s.parse().ok()))
            .unwrap_or_else(|| {
                // Auto-detect local IP by connecting to a remote address
                std::net::UdpSocket::bind("0.0.0.0:0")
                    .and_then(|s| { s.connect("8.8.8.8:53")?; s.local_addr() })
                    .map(|a| match a.ip() {
                        std::net::IpAddr::V4(v4) => v4,
                        _ => std::net::Ipv4Addr::UNSPECIFIED,
                    })
                    .unwrap_or(std::net::Ipv4Addr::UNSPECIFIED)
            });
        tokio::spawn(async move {
            if let Err(e) = minidsp_daemon::discovery::broadcast_loop(
                disc_ip,
                hw_id, fw_major, fw_minor, dsp, serial,
                disc_hostname,
            ).await {
                tracing::error!("UDP discovery error: {e}");
            }
        });
    }

    // Start Unix socket listener at /tmp/minidsp.sock
    #[cfg(unix)]
    {
        let unix_app = app.clone();
        tokio::spawn(async move {
            let sock_path = "/tmp/minidsp.sock";
            // Remove stale socket file
            let _ = std::fs::remove_file(sock_path);
            match tokio::net::UnixListener::bind(sock_path) {
                Ok(unix_listener) => {
                    // Set socket permissions to 0o766
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(
                        sock_path,
                        std::fs::Permissions::from_mode(0o766),
                    );
                    tracing::info!("Unix socket listening on {sock_path}");
                    if let Err(e) = axum::serve(
                        unix_listener,
                        unix_app.into_make_service(),
                    ).await {
                        tracing::error!("Unix socket server error: {e}");
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to bind Unix socket at {sock_path}: {e}");
                }
            }
        });
    }

    // Start mDNS advertisement
    let _mdns_handle = if let Some(client) = state.get_device(0).await {
        let port: u16 = http_bind.rsplit(':').next()
            .and_then(|p| p.parse().ok())
            .unwrap_or(5380);
        let product = probe::product_name(client.hw_id().hw_id, client.dsp_version());
        match minidsp_daemon::mdns::advertise(
            port, client.serial(), product,
            client.hw_id().hw_id, client.dsp_version(),
        ) {
            Ok(mdns) => {
                tracing::info!("mDNS: advertising as minidsp-{}.local", client.serial());
                Some(mdns)
            }
            Err(e) => {
                tracing::warn!("mDNS advertisement failed: {e}");
                None
            }
        }
    } else {
        None
    };

    println!("minidsp-open daemon listening on {http_bind}");
    println!("  HTTP API: http://{http_bind}/devices");
    println!("  TCP bridge: {tcp_bind}");
    println!("  mDNS: _minidsp._tcp.local.");
    #[cfg(unix)]
    println!("  Unix socket: /tmp/minidsp.sock");

    let listener = tokio::net::TcpListener::bind(http_bind).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

/// Execute commands from a file (minidsp -f <file>).
pub async fn execute_file(path: &str, cli: &Cli) -> Result<(), Box<dyn std::error::Error>> {
    let contents = std::fs::read_to_string(path)?;
    if cli._all_local_devices {
        let mut clients = connect_all_local(cli).await?;
        for client in &mut clients {
            apply_force_kind(client, cli).await?;
        }
        for (line_num, line) in contents.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            for client in &clients {
                if let Err(e) = execute_line(client, line).await {
                    eprintln!("Error on line {}: {e}", line_num + 1);
                    return Err(e);
                }
            }
        }
        return Ok(());
    }

    let client = connect(cli).await?;

    for (line_num, line) in contents.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Err(e) = execute_line(&client, line).await {
            eprintln!("Error on line {}: {e}", line_num + 1);
            return Err(e);
        }
    }
    Ok(())
}

/// Execute a single command line (from -f file or interactive).
async fn execute_line(client: &MiniDspClient, line: &str) -> Result<(), Box<dyn std::error::Error>> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.is_empty() {
        return Ok(());
    }

    match parts[0] {
        "gain" => {
            // gain -- {dB}
            let db = parse_gain_value(&parts[1..])?;
            client.set_volume_db(db).await?;
        }
        "mute" => {
            let state = parse_on_off_str(parts.get(1).unwrap_or(&"on"))?;
            client.set_mute(state).await?;
        }
        "source" => {
            let name = parts.get(1).ok_or("missing source name")?;
            let src = Source::from_str_lowercase(name)
                .ok_or_else(|| format!("unknown source '{name}'"))?;
            let idx = client.profile().sources.index_for_source(src)
                .ok_or_else(|| format!("source '{name}' not available"))?;
            client.set_source(idx).await?;
        }
        "config" => {
            let preset: u8 = parts.get(1).ok_or("missing preset")?.parse()?;
            client.set_config(preset).await?;
        }
        "input" | "output" => {
            let is_input = parts[0] == "input";
            let ch: u8 = parts.get(1).ok_or("missing channel")?.parse()?;
            let subcmd = parts.get(2).ok_or("missing subcommand")?;
            match *subcmd {
                "peq" => {
                    let band_str = parts.get(3).ok_or("missing PEQ index")?;
                    let action = parts.get(4).ok_or("missing PEQ action")?;

                    // Get list of bands: "all" → all bands, numeric → single band
                    let bands: Vec<u16> = if *band_str == "all" {
                        let table = client.profile().dsp_table
                            .ok_or("device does not support PEQ")?;
                        let peq_slice = if is_input {
                            table.inputs.get(ch as usize)
                                .map(|c| c.peq).ok_or_else(|| format!("channel {ch} out of range"))?
                        } else {
                            table.outputs.get(ch as usize)
                                .map(|c| c.peq).ok_or_else(|| format!("channel {ch} out of range"))?
                        };
                        peq_slice.iter().map(|b| b.addr).collect()
                    } else {
                        let band: u8 = band_str.parse()?;
                        vec![get_peq_addr(client, is_input, ch, band)?]
                    };

                    for addr in bands {
                        match *action {
                            "set" => {
                                // set -- b0 b1 b2 a1 a2
                                let coeff_start = if parts.get(5) == Some(&"--") { 6 } else { 5 };
                                if parts.len() < coeff_start + 5 {
                                    return Err("need 5 biquad coefficients".into());
                                }
                                let b0: f32 = parts[coeff_start].parse()?;
                                let b1: f32 = parts[coeff_start + 1].parse()?;
                                let b2: f32 = parts[coeff_start + 2].parse()?;
                                let a1: f32 = parts[coeff_start + 3].parse()?;
                                let a2: f32 = parts[coeff_start + 4].parse()?;
                                client.write_biquad(addr, &[b0, b1, b2, a1, a2]).await?;
                            }
                            "bypass" => {
                                let state = parse_on_off_str(parts.get(5).unwrap_or(&"on"))?;
                                client.bypass_filter(addr, state).await?;
                            }
                            "clear" => {
                                client.write_biquad(addr, &[1.0, 0.0, 0.0, 0.0, 0.0]).await?;
                                client.bypass_filter(addr, true).await?;
                            }
                            _ => return Err(format!("unknown PEQ action '{action}'").into()),
                        }
                    }
                }
                "gain" => {
                    let db = parse_gain_value(&parts[3..])?;
                    let addr = get_channel_gain_addr(client, is_input, ch)?;
                    client.set_channel_gain(addr, db).await?;
                }
                "mute" => {
                    let state = parse_on_off_str(parts.get(3).unwrap_or(&"on"))?;
                    let addr = get_channel_mute_addr(client, is_input, ch)?;
                    client.set_channel_mute(addr, state).await?;
                }
                "delay" => {
                    if is_input {
                        return Err("input delay not supported via this command".into());
                    }
                    let ms: f32 = parts.get(3).ok_or("missing delay value")?.parse()?;
                    let samples = (ms * client.profile().sample_rate as f32 / 1000.0).round().clamp(0.0, 48000.0);
                    let table = client.profile().dsp_table
                        .ok_or("device does not support channel delay")?;
                    let ch_def = table.outputs.get(ch as usize)
                        .ok_or_else(|| format!("output channel {ch} out of range"))?;
                    client.set_channel_delay_samples(ch_def.delay, samples as u32).await?;
                }
                "invert" => {
                    if is_input {
                        return Err("input polarity inversion not supported".into());
                    }
                    let state = parse_on_off_str(parts.get(3).unwrap_or(&"on"))?;
                    let table = client.profile().dsp_table
                        .ok_or("device does not support polarity inversion")?;
                    let ch_def = table.outputs.get(ch as usize)
                        .ok_or_else(|| format!("output channel {ch} out of range"))?;
                    client.set_invert(ch_def.polarity, state).await?;
                }
                _ => return Err(format!("unknown channel command '{subcmd}'").into()),
            }
        }
        _ => return Err(format!("unknown command '{}'", parts[0]).into()),
    }
    Ok(())
}

/// Parse gain value, handling the `--` separator for negative values.
fn parse_gain_value(parts: &[&str]) -> Result<f32, Box<dyn std::error::Error>> {
    if parts.is_empty() {
        return Err("missing gain value".into());
    }
    if parts[0] == "--" {
        if parts.len() < 2 {
            return Err("missing gain value after --".into());
        }
        Ok(parts[1].parse()?)
    } else {
        Ok(parts[0].parse()?)
    }
}

fn parse_on_off_str(s: &str) -> Result<bool, Box<dyn std::error::Error>> {
    match s.to_lowercase().as_str() {
        "on" | "true" | "1" => Ok(true),
        "off" | "false" | "0" => Ok(false),
        _ => Err(format!("expected on/off, got '{s}'").into()),
    }
}

fn get_peq_addr(client: &MiniDspClient, is_input: bool, ch: u8, band: u8) -> Result<u16, Box<dyn std::error::Error>> {
    let table = client.profile().dsp_table
        .ok_or("device does not support PEQ")?;
    let peq_slice = if is_input {
        let ch_def = table.inputs.get(ch as usize)
            .ok_or_else(|| format!("input channel {ch} out of range"))?;
        ch_def.peq
    } else {
        let ch_def = table.outputs.get(ch as usize)
            .ok_or_else(|| format!("output channel {ch} out of range"))?;
        ch_def.peq
    };
    let peq = peq_slice.get(band as usize)
        .ok_or_else(|| format!("PEQ band {band} out of range"))?;
    Ok(peq.addr)
}

fn get_channel_gain_addr(client: &MiniDspClient, is_input: bool, ch: u8) -> Result<u16, Box<dyn std::error::Error>> {
    let table = client.profile().dsp_table
        .ok_or("device does not support channel gain")?;
    if is_input {
        let ch_def = table.inputs.get(ch as usize)
            .ok_or_else(|| format!("input channel {ch} out of range"))?;
        ch_def.gain.ok_or_else(|| format!("input channel {ch} does not have gain control").into())
    } else {
        let ch_def = table.outputs.get(ch as usize)
            .ok_or_else(|| format!("output channel {ch} out of range"))?;
        Ok(ch_def.gain)
    }
}

fn get_channel_mute_addr(client: &MiniDspClient, is_input: bool, ch: u8) -> Result<u16, Box<dyn std::error::Error>> {
    let table = client.profile().dsp_table
        .ok_or("device does not support channel mute")?;
    if is_input {
        let ch_def = table.inputs.get(ch as usize)
            .ok_or_else(|| format!("input channel {ch} out of range"))?;
        ch_def.mute.ok_or_else(|| format!("input channel {ch} does not have mute control").into())
    } else {
        let ch_def = table.outputs.get(ch as usize)
            .ok_or_else(|| format!("output channel {ch} out of range"))?;
        Ok(ch_def.mute)
    }
}

/// Handle input/output channel subcommands from clap.
pub async fn channel_command(
    client: &MiniDspClient,
    is_input: bool,
    channel: u8,
    cmd: ChannelCommand,
) -> Result<(), Box<dyn std::error::Error>> {
    match cmd {
        ChannelCommand::Gain { db } => {
            let addr = get_channel_gain_addr(client, is_input, channel)?;
            client.set_channel_gain(addr, db).await?;
        }
        ChannelCommand::Mute { state } => {
            let addr = get_channel_mute_addr(client, is_input, channel)?;
            client.set_channel_mute(addr, state).await?;
        }
        ChannelCommand::Delay { ms } => {
            let table = client.profile().dsp_table
                .ok_or("device does not support channel delay")?;
            let ch_def = if is_input {
                return Err("input delay not supported".into());
            } else {
                table.outputs.get(channel as usize)
                    .ok_or_else(|| format!("output channel {channel} out of range"))?
            };
            let sample_rate = client.profile().sample_rate as f32;
            let samples = (ms * sample_rate / 1000.0).round().clamp(0.0, 48000.0);
            client.set_channel_delay_samples(ch_def.delay, samples as u32).await?;
        }
        ChannelCommand::Invert { state } => {
            let table = client.profile().dsp_table
                .ok_or("device does not support polarity inversion")?;
            let ch_def = if is_input {
                return Err("input polarity inversion not supported".into());
            } else {
                table.outputs.get(channel as usize)
                    .ok_or_else(|| format!("output channel {channel} out of range"))?
            };
            client.set_invert(ch_def.polarity, state).await?;
        }
        ChannelCommand::Peq { ref index, ref cmd } => {
            if index == "all" {
                // Apply to all PEQ bands on this channel
                let table = client.profile().dsp_table
                    .ok_or("device does not support PEQ")?;
                let bands = if is_input {
                    table.inputs.get(channel as usize)
                        .map(|ch| ch.peq)
                        .ok_or_else(|| format!("input channel {channel} out of range"))?
                } else {
                    table.outputs.get(channel as usize)
                        .map(|ch| ch.peq)
                        .ok_or_else(|| format!("output channel {channel} out of range"))?
                };
                for band in bands {
                    match cmd {
                        PeqCommand::Set { b0, b1, b2, a1, a2 } => {
                            client.write_biquad(band.addr, &[*b0, *b1, *b2, *a1, *a2]).await?;
                        }
                        PeqCommand::Bypass { state } => {
                            client.bypass_filter(band.addr, *state).await?;
                        }
                        PeqCommand::Clear => {
                            client.write_biquad(band.addr, &[1.0, 0.0, 0.0, 0.0, 0.0]).await?;
                            client.bypass_filter(band.addr, true).await?;
                        }
                    }
                }
            } else {
                let band_idx: u8 = index.parse()
                    .map_err(|_| format!("invalid PEQ index '{index}' (expected number or 'all')"))?;
                let addr = get_peq_addr(client, is_input, channel, band_idx)?;
                match cmd {
                    PeqCommand::Set { b0, b1, b2, a1, a2 } => {
                        client.write_biquad(addr, &[*b0, *b1, *b2, *a1, *a2]).await?;
                    }
                    PeqCommand::Bypass { state } => {
                        client.bypass_filter(addr, *state).await?;
                    }
                    PeqCommand::Clear => {
                        client.write_biquad(addr, &[1.0, 0.0, 0.0, 0.0, 0.0]).await?;
                        client.bypass_filter(addr, true).await?;
                    }
                }
            }
        }
        ChannelCommand::Crossover { ref cmd } => {
            if is_input {
                return Err("crossover not available on input channels".into());
            }
            let table = client.profile().dsp_table
                .ok_or("device does not support crossover")?;
            let ch_def = table.outputs.get(channel as usize)
                .ok_or_else(|| format!("output channel {channel} out of range"))?;
            let xover = ch_def.crossover
                .ok_or("output channel does not have crossover")?;
            match cmd {
                CrossoverCommand::Set { group, coefficients } => {
                    // Group 0 = LPF, Group 1 = HPF. Each group has 4 biquads (20 coefficients).
                    let base_addr = match *group {
                        0 => xover.lpf_addr,
                        1 => xover.hpf_addr,
                        n => return Err(format!("crossover group must be 0 (LPF) or 1 (HPF), got {n}").into()),
                    };
                    if coefficients.len() != 20 {
                        return Err(format!("crossover needs exactly 20 coefficients (4 biquads x 5), got {}", coefficients.len()).into());
                    }
                    for (i, chunk) in coefficients.chunks(5).enumerate() {
                        let addr = base_addr + (i * 5) as u16;
                        client.write_biquad(addr, &[chunk[0], chunk[1], chunk[2], chunk[3], chunk[4]]).await?;
                    }
                }
                CrossoverCommand::Bypass { state } => {
                    // Bypass all 4 biquads in both LPF and HPF groups
                    for base in [xover.lpf_addr, xover.hpf_addr] {
                        for i in 0..4 {
                            let addr = base + (i * 5) as u16;
                            client.bypass_filter(addr, *state).await?;
                        }
                    }
                }
            }
        }
        ChannelCommand::Compressor { ref cmd } => {
            if is_input {
                return Err("compressor not available on input channels".into());
            }
            let table = client.profile().dsp_table
                .ok_or("device does not support compressor")?;
            let ch_def = table.outputs.get(channel as usize)
                .ok_or_else(|| format!("output channel {channel} out of range"))?;
            let comp = ch_def.compressor
                .ok_or("output channel does not have compressor")?;
            match cmd {
                CompressorCommand::Bypass { state } => {
                    let val = if *state { 1.0 } else { 0.0 };
                    client.write_dsp(comp.bypass, val, true).await?;
                }
                CompressorCommand::Set { threshold, ratio, attack, release } => {
                    if *attack <= 0.0 || *release <= 0.0 {
                        return Err("attack and release must be > 0 ms".into());
                    }
                    if *ratio < 1.0 {
                        return Err("ratio must be >= 1.0".into());
                    }
                    // DSP registers store raw values: threshold in dBFS, attack/release in ms
                    client.write_dsp(comp.threshold, *threshold, true).await?;
                    client.write_dsp(comp.ratio, *ratio, true).await?;
                    client.write_dsp(comp.attack, *attack, true).await?;
                    client.write_dsp(comp.release, *release, true).await?;
                }
            }
        }
        ChannelCommand::Fir { ref cmd } => {
            if !is_input {
                return Err("FIR only available on input channels".into());
            }
            let table = client.profile().dsp_table
                .ok_or("device does not support FIR")?;
            let input_ch = table.inputs.get(channel as usize)
                .ok_or_else(|| format!("input channel {channel} out of range"))?;
            let fir = input_ch.fir
                .ok_or("input channel does not have FIR filter")?;
            match cmd {
                FirCommand::Bypass { state } => {
                    let val = if *state { 1.0 } else { 0.0 };
                    client.write_dsp(fir.bypass, val, true).await?;
                }
                FirCommand::Load { file } => {
                    let contents = std::fs::read_to_string(file)?;
                    let coeffs: Vec<f32> = contents.lines()
                        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
                        .map(|l| l.trim().parse::<f32>())
                        .collect::<Result<Vec<_>, _>>()?;
                    if coeffs.len() > fir.max_taps as usize {
                        return Err(format!("FIR has {} taps but device maximum is {}", coeffs.len(), fir.max_taps).into());
                    }
                    // Use FIR load protocol: start, then send chunks
                    use minidsp_protocol::commands;
                    client.raw_command(commands::fir_load_start(channel), minidsp::queue::Priority::High).await?;
                    for chunk in coeffs.chunks(14) {
                        client.raw_command(commands::fir_load_data(channel, chunk), minidsp::queue::Priority::High).await?;
                    }
                    // Write tap count
                    client.write_dsp(fir.taps_count, coeffs.len() as f32, true).await?;
                }
            }
        }
        ChannelCommand::Routing { output, gain_db } => {
            if !is_input {
                return Err("routing only available on input channels".into());
            }
            let table = client.profile().dsp_table
                .ok_or("device does not support routing")?;
            let routing = table.routing;
            if channel >= routing.rows || output >= routing.cols {
                return Err(format!("routing index ({channel}, {output}) out of range (max {}x{})", routing.rows, routing.cols).into());
            }
            // Gain address = gain_base + (input * cols + output)
            let gain_addr = routing.gain_base + (channel as u16 * routing.cols as u16 + output as u16);
            let gain_linear = 10.0f32.powf(gain_db / 20.0);
            client.write_dsp(gain_addr, gain_linear, true).await?;
        }
    }
    Ok(())
}
