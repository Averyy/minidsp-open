//! CLI command implementations.

use crate::{Cli, ChannelCommand, PeqCommand, connect};
use minidsp::client::MiniDspClient;
use minidsp::transport::hid;
use minidsp_protocol::device::probe;
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
}

/// Print device status.
pub async fn status(client: &MiniDspClient, format: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let summary = client.get_status_summary().await?;

    if format == Some("jsonline") {
        output::print_status_json(&summary)?;
    } else {
        output::print_status_text(&summary, client.profile());
    }
    Ok(())
}

/// Enumerate connected devices.
pub async fn probe(_cli: &Cli) -> Result<(), Box<dyn std::error::Error>> {
    let devices = hid::enumerate()?;
    if devices.is_empty() {
        println!("No MiniDSP devices found");
        return Ok(());
    }

    for (i, dev) in devices.iter().enumerate() {
        // Need to open device to read hw_id and dsp_version
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
                        // Match minidsp-rs probe output format exactly
                        println!(
                            "{i}: Found {name} with serial {} at {url} [hw_id: {}, dsp_version: {}]",
                            client.serial(),
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
    Ok(())
}

/// Start the daemon server.
pub async fn server(bind: &str, config_path: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
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
    let hostname = config.advertise.as_ref()
        .and_then(|a| a.hostname.as_deref())
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
        tokio::spawn(async move {
            if let Err(e) = minidsp_daemon::discovery::broadcast_loop(
                std::net::Ipv4Addr::new(0, 0, 0, 0),
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
                    let band: u8 = parts.get(3).ok_or("missing PEQ index")?.parse()?;
                    let action = parts.get(4).ok_or("missing PEQ action")?;
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
                            let addr = get_peq_addr(client, is_input, ch, band)?;
                            client.write_biquad(addr, &[b0, b1, b2, a1, a2]).await?;
                        }
                        "bypass" => {
                            let state = parse_on_off_str(parts.get(5).unwrap_or(&"on"))?;
                            let addr = get_peq_addr(client, is_input, ch, band)?;
                            client.bypass_filter(addr, state).await?;
                        }
                        _ => return Err(format!("unknown PEQ action '{action}'").into()),
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
                    let samples = (ms * client.profile().sample_rate as f32 / 1000.0) as u16;
                    let table = client.profile().dsp_table
                        .ok_or("device does not support channel delay")?;
                    let ch_def = table.outputs.get(ch as usize)
                        .ok_or_else(|| format!("output channel {ch} out of range"))?;
                    client.write_dsp(ch_def.delay, samples as f32, true).await?;
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
            let samples = (ms * client.profile().sample_rate as f32 / 1000.0) as u16;
            client.write_dsp(ch_def.delay, samples as f32, true).await?;
        }
        ChannelCommand::Peq { index, cmd } => {
            let addr = get_peq_addr(client, is_input, channel, index)?;
            match cmd {
                PeqCommand::Set { b0, b1, b2, a1, a2 } => {
                    client.write_biquad(addr, &[b0, b1, b2, a1, a2]).await?;
                }
                PeqCommand::Bypass { state } => {
                    client.bypass_filter(addr, state).await?;
                }
            }
        }
    }
    Ok(())
}
