use clap::{Parser, Subcommand};
use minidsp::client::MiniDspClient;
use minidsp::transport::hid::HidTransport;
use minidsp::transport::tcp::TcpTransport;
use minidsp::transport::ws::WsTransport;
use minidsp_daemon::http::json::DeviceJson;
use minidsp_protocol::source::Source;
use serde::Deserialize;
use std::fs::File;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing_subscriber::Layer;
use tracing_subscriber::fmt::writer::BoxMakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, fmt};

mod cli;
mod output;

#[derive(Parser)]
#[command(name = "minidsp", version, about = "MiniDSP controller")]
struct Cli {
    /// Output format
    #[arg(short = 'o', long, value_name = "FORMAT")]
    output: Option<String>,

    /// Execute commands from a file
    #[arg(short = 'f', long, value_name = "FILE")]
    file: Option<String>,

    /// Force device kind (compatibility with minidsp-rs --force-kind)
    #[arg(long = "force-kind", value_name = "KIND")]
    force_kind: Option<String>,

    /// Select device by index when multiple are connected
    #[arg(long = "device-index", short = 'd', value_name = "INDEX")]
    device_index: Option<usize>,

    /// Connect to any transport URL (tcp://host:port, usb://path, or usb:path)
    #[arg(long, value_name = "URL")]
    url: Option<String>,

    /// Connect via TCP instead of USB
    #[arg(long, value_name = "ADDR")]
    tcp: Option<String>,

    /// Connect to daemon Unix socket
    #[arg(long = "daemon-sock", value_name = "PATH")]
    daemon_sock: Option<String>,

    /// Connect to daemon via WebSocket (ws://host:5380/devices/{id}/ws)
    #[arg(long = "daemon-url", value_name = "URL")]
    daemon_url: Option<String>,

    /// Connect to a specific USB path or vid[:pid] selector
    #[arg(long, value_name = "PATH")]
    usb: Option<String>,

    /// Verbose logging (repeat for more: -v, -vv, -vvv)
    #[arg(short = 'v', long = "verbose", action = clap::ArgAction::Count)]
    verbose: u8,

    /// Log commands and responses to a file
    #[arg(long = "log", value_name = "FILE")]
    log_path: Option<PathBuf>,

    /// Use all locally-connected devices (daemon mode)
    #[arg(long = "all-local-devices", hide = true)]
    _all_local_devices: bool,

    /// Deprecated: use --force-kind instead (accepted for backward compat)
    #[arg(long = "force-device", value_name = "SERIAL", hide = true)]
    _force_device: Option<String>,

    /// Deprecated: use --force-kind instead (accepted for backward compat)
    #[arg(long = "force-dsp", value_name = "DSP", hide = true)]
    _force_dsp: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

/// Map a --force-kind string to (hw_id, dsp_version).
fn resolve_force_kind(kind: &str) -> Option<(u8, u8)> {
    match kind.to_lowercase().as_str() {
        "2x4hd" | "2x4-hd" | "m2x4hd" => Some((10, 100)),
        "ddrc24" | "ddrc-24" | "mddrc24" => Some((10, 101)),
        "flex" | "mflex" => Some((27, 100)),
        "flex-dl" | "flexdl" => Some((27, 101)),
        "shd" => Some((14, 105)),
        "shd-studio" | "shdstudio" => Some((17, 105)),
        "shd-power" | "shdpower" => Some((21, 105)),
        "flex-eight" | "flexeight" | "flex8" => Some((30, 110)),
        "flex-eight-dl" | "flexeightdl" | "flex8dl" => Some((30, 111)),
        "flex-ht" | "flexht" => Some((31, 115)),
        "flex-htx" | "flexhtx" => Some((32, 115)),
        "4x10hd" | "4x10-hd" | "m4x10hd" => Some((1, 0)),
        "10x10hd" | "10x10-hd" | "m10x10hd" => Some((1, 51)),
        "nanodigi-2x8" | "nanodigi2x8" => Some((2, 54)),
        "2x4" => Some((2, 22)),
        "msharc-4x8" | "minisharc" | "msharc4x8" => Some((4, 0)),
        "ddrc-88bm" | "ddrc88bm" => Some((6, 95)),
        "cdsp-8x12v2" | "cdsp8x12v2" | "c8x12v2" => Some((11, 97)),
        _ => None,
    }
}

#[derive(Subcommand, Clone)]
enum Command {
    /// Print device status (default if no command given)
    Status,
    /// Enumerate connected devices
    Probe {
        /// Listen for advertisement broadcasts from WI-DG / network daemons
        #[arg(long, short)]
        net: bool,
    },
    /// Start the HTTP API daemon
    Server {
        /// Bind address
        #[arg(long, default_value = "0.0.0.0:5380")]
        bind: String,
        /// TOML config file (minidsp-rs compatible)
        #[arg(long = "config", value_name = "FILE")]
        config: Option<String>,
        /// Override advertised hostname for mDNS/UDP discovery
        #[arg(long)]
        advertise: Option<String>,
        /// Override advertised IP for UDP discovery
        #[arg(long)]
        ip: Option<String>,
    },
    /// Set master volume in dB
    Gain {
        /// Volume in dB (negative values require --)
        #[arg(allow_hyphen_values = true)]
        db: f32,
        /// Relative adjustment (add to current volume instead of absolute)
        #[arg(long, short = 'r')]
        relative: bool,
    },
    /// Set mute state (on/off/toggle)
    Mute {
        /// on, off, or toggle
        state: String,
    },
    /// Set input source
    Source { name: String },
    /// Switch preset (0-3)
    Config { preset: u8 },
    /// Set Dirac Live bypass (on/off/toggle)
    Dirac {
        /// on, off, or toggle
        state: String,
    },
    /// Input channel commands
    Input {
        /// Channel index (0-based)
        channel: u8,
        #[command(subcommand)]
        cmd: ChannelCommand,
    },
    /// Output channel commands
    Output {
        /// Channel index (0-based)
        channel: u8,
        #[command(subcommand)]
        cmd: ChannelCommand,
    },
    /// Send raw hex commands to the device (for debugging)
    Debug {
        #[command(subcommand)]
        cmd: DebugCommand,
    },
}

#[derive(Subcommand, Clone)]
enum DebugCommand {
    /// Send raw hex payload and print hex response
    Send {
        /// Hex bytes (e.g. "31" for ReadHardwareId)
        hex: String,
    },
    /// Read hardware ID
    Id,
    /// Dump float values from a DSP address
    DumpFloat {
        /// DSP address (decimal)
        addr: u16,
        /// Number of floats to read (max 14)
        #[arg(default_value = "1")]
        count: u8,
    },
    /// Dump typed 32-bit words from a DSP/config address
    DumpWord {
        /// DSP address (decimal)
        addr: u16,
        /// Number of words to read (max 14)
        #[arg(default_value = "1")]
        count: u8,
    },
    /// Dump raw bytes from the full flash-address space
    DumpFlash {
        /// Flash address (decimal or 0x-prefixed hex)
        #[arg(value_parser = parse_u32_auto)]
        addr: u32,
        /// Number of bytes to read (max 58)
        #[arg(default_value = "1")]
        count: u8,
    },
}

#[derive(Subcommand, Clone)]
enum ChannelCommand {
    /// Set channel gain in dB
    Gain {
        #[arg(allow_hyphen_values = true)]
        db: f32,
    },
    /// Set channel mute
    Mute {
        #[arg(action = clap::ArgAction::Set, value_parser = parse_on_off)]
        state: bool,
    },
    /// Set channel delay in milliseconds
    Delay { ms: f32 },
    /// Invert channel polarity
    Invert {
        #[arg(action = clap::ArgAction::Set, value_parser = parse_on_off)]
        state: bool,
    },
    /// PEQ filter commands
    Peq {
        /// PEQ band index (0-based, or "all" for all bands)
        index: String,
        #[command(subcommand)]
        cmd: PeqCommand,
    },
    /// Crossover filter (output channels only)
    Crossover {
        #[command(subcommand)]
        cmd: CrossoverCommand,
    },
    /// Compressor settings (output channels only)
    Compressor {
        #[command(subcommand)]
        cmd: CompressorCommand,
    },
    /// FIR filter
    Fir {
        #[command(subcommand)]
        cmd: FirCommand,
    },
    /// Routing matrix (input channels only)
    Routing {
        /// Output channel to route to
        output: u8,
        /// Gain in dB for this routing path
        #[arg(allow_hyphen_values = true)]
        gain_db: f32,
        /// Disable this route instead of enabling it
        #[arg(long)]
        off: bool,
    },
}

#[derive(Subcommand, Clone)]
enum CrossoverCommand {
    /// Set crossover biquad coefficients (group of 4: [b0 b1 b2 a1 a2] x4)
    Set {
        /// "0" for group 0 (e.g. low-pass), "1" for group 1 (e.g. high-pass)
        group: u8,
        /// 20 coefficients: 4 biquads x 5 coefficients each
        #[arg(allow_hyphen_values = true, num_args = 20)]
        coefficients: Vec<f32>,
    },
    /// Bypass crossover
    Bypass {
        #[arg(action = clap::ArgAction::Set, value_parser = parse_on_off)]
        state: bool,
    },
}

#[derive(Subcommand, Clone)]
enum CompressorCommand {
    /// Set compressor bypass
    Bypass {
        #[arg(action = clap::ArgAction::Set, value_parser = parse_on_off)]
        state: bool,
    },
    /// Set compressor parameters (threshold, ratio, attack, release)
    Set {
        /// Threshold in dB
        #[arg(allow_hyphen_values = true)]
        threshold: f32,
        /// Ratio (e.g. 4.0 for 4:1)
        ratio: f32,
        /// Attack time in ms
        attack: f32,
        /// Release time in ms
        release: f32,
    },
}

#[derive(Subcommand, Clone)]
enum FirCommand {
    /// Set FIR bypass
    Bypass {
        #[arg(action = clap::ArgAction::Set, value_parser = parse_on_off)]
        state: bool,
    },
    /// Load FIR coefficients from file (one coefficient per line)
    Load {
        /// Path to FIR coefficient file
        file: String,
    },
}

#[derive(Subcommand, Clone)]
enum PeqCommand {
    /// Set biquad coefficients
    Set {
        /// b0 coefficient
        #[arg(allow_hyphen_values = true)]
        b0: f32,
        /// b1 coefficient
        #[arg(allow_hyphen_values = true)]
        b1: f32,
        /// b2 coefficient
        #[arg(allow_hyphen_values = true)]
        b2: f32,
        /// a1 coefficient
        #[arg(allow_hyphen_values = true)]
        a1: f32,
        /// a2 coefficient
        #[arg(allow_hyphen_values = true)]
        a2: f32,
    },
    /// Set PEQ bypass
    Bypass {
        #[arg(action = clap::ArgAction::Set, value_parser = parse_on_off)]
        state: bool,
    },
    /// Clear PEQ band (set to identity biquad + bypass on)
    Clear,
}

fn parse_on_off(s: &str) -> Result<bool, String> {
    match s.to_lowercase().as_str() {
        "on" | "true" | "1" => Ok(true),
        "off" | "false" | "0" => Ok(false),
        _ => Err(format!("expected on/off, got '{s}'")),
    }
}

fn parse_u32_auto(s: &str) -> Result<u32, String> {
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16).map_err(|_| format!("invalid hex u32 '{s}'"))
    } else {
        s.parse::<u32>()
            .map_err(|_| format!("invalid decimal u32 '{s}'"))
    }
}

#[derive(Clone)]
struct SharedLogWriter(Arc<StdMutex<File>>);

impl io::Write for SharedLogWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .map_err(|_| io::Error::other("log file lock poisoned"))?
            .write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0
            .lock()
            .map_err(|_| io::Error::other("log file lock poisoned"))?
            .flush()
    }
}

fn make_log_writer(path: &PathBuf) -> Result<BoxMakeWriter, Box<dyn std::error::Error>> {
    let file = Arc::new(StdMutex::new(File::create(path)?));
    let writer = file.clone();
    Ok(BoxMakeWriter::new(move || SharedLogWriter(writer.clone())))
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let log_path = cli
        .log_path
        .clone()
        .or_else(|| std::env::var("MINIDSP_LOG").ok().map(PathBuf::from));

    // Match minidsp-rs logging semantics:
    // - -v/-vv/-vvv controls console verbosity
    // - --log / MINIDSP_LOG writes transport logs to a file
    let default_filter = match cli.verbose {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    let console_filter = if std::env::var_os("RUST_LOG").is_some() {
        EnvFilter::from_default_env()
    } else {
        EnvFilter::new(default_filter)
    };
    let console_layer = fmt::layer().with_filter(console_filter);

    if let Some(path) = &log_path {
        match make_log_writer(path) {
            Ok(writer) => {
                let file_layer = fmt::layer()
                    .with_ansi(false)
                    .with_writer(writer)
                    .with_filter(EnvFilter::new(
                        "minidsp=trace,minidsp_protocol=trace,minidsp_daemon=trace",
                    ));
                tracing_subscriber::registry()
                    .with(console_layer)
                    .with(file_layer)
                    .init();
            }
            Err(e) => {
                eprintln!("Error: failed to open log file {}: {e}", path.display());
                std::process::exit(1);
            }
        }
    } else {
        tracing_subscriber::registry().with(console_layer).init();
    }

    if let Err(e) = run(cli).await {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    // Handle file execution mode
    if let Some(file_path) = &cli.file {
        return cli::execute_file(file_path, &cli).await;
    }

    // Handle probe (doesn't need a connection)
    if matches!(cli.command, Some(Command::Probe { .. })) {
        return cli::probe(&cli).await;
    }

    // Handle server mode
    if let Some(Command::Server {
        ref bind,
        ref config,
        ref advertise,
        ref ip,
    }) = cli.command
    {
        return cli::server(bind, config.as_deref(), advertise.as_deref(), ip.as_deref()).await;
    }

    if cli._all_local_devices {
        let mut clients = connect_all_local(&cli).await?;
        if clients.is_empty() {
            return Err("no MiniDSP devices found".into());
        }
        for client in &mut clients {
            apply_force_kind(client, &cli).await?;
            execute_command(client, &cli).await?;
        }
        return Ok(());
    }

    // Connect to device
    let mut client = connect(&cli).await?;
    apply_force_kind(&mut client, &cli).await?;
    execute_command(&mut client, &cli).await?;
    Ok(())
}

pub(crate) async fn apply_force_kind(
    client: &mut MiniDspClient,
    cli: &Cli,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(ref kind) = cli.force_kind {
        if let Some((hw_id, dsp_version)) = resolve_force_kind(kind) {
            let profile = minidsp_protocol::device::probe::identify(hw_id, dsp_version);
            tracing::info!(
                "forcing device kind to {} (hw_id={}, dsp={})",
                profile.name,
                hw_id,
                dsp_version
            );
            client.override_profile(profile).await;
        } else {
            return Err(format!("unknown device kind '{kind}'").into());
        }
    }
    Ok(())
}

async fn execute_command(
    client: &mut MiniDspClient,
    cli: &Cli,
) -> Result<(), Box<dyn std::error::Error>> {
    match &cli.command {
        None | Some(Command::Status) => {
            cli::status(client, cli.output.as_deref()).await?;
        }
        Some(Command::Gain { db, relative }) => {
            if *relative {
                let current = client.get_master_status().await?;
                let new_vol = current.volume_db() + *db;
                client.set_volume_db(new_vol).await?;
            } else {
                client.set_volume_db(*db).await?;
            }
        }
        Some(Command::Mute { state }) => {
            let mute = match state.to_lowercase().as_str() {
                "toggle" => {
                    let current = client.get_master_status().await?;
                    !current.is_muted()
                }
                s => parse_on_off(s).map_err(|e| -> Box<dyn std::error::Error> { e.into() })?,
            };
            client.set_mute(mute).await?;
        }
        Some(Command::Source { name }) => {
            let src = Source::from_str_lowercase(name)
                .ok_or_else(|| format!("unknown source '{name}'"))?;
            let idx = client
                .profile()
                .sources
                .index_for_source(src)
                .ok_or_else(|| format!("source '{name}' not available on this device"))?;
            client.set_source(idx).await?;
        }
        Some(Command::Config { preset }) => {
            client.set_config(*preset).await?;
        }
        Some(Command::Dirac { state }) => {
            let dirac_on = match state.to_lowercase().as_str() {
                "toggle" => {
                    let status = client.get_status_summary().await?;
                    !status.master.dirac.unwrap_or(false)
                }
                s => parse_on_off(s).map_err(|e| -> Box<dyn std::error::Error> { e.into() })?,
            };
            client.set_dirac_bypass(!dirac_on).await?;
        }
        Some(Command::Input { channel, cmd }) => {
            cli::channel_command(client, true, *channel, cmd.clone()).await?;
        }
        Some(Command::Output { channel, cmd }) => {
            cli::channel_command(client, false, *channel, cmd.clone()).await?;
        }
        Some(Command::Debug { cmd }) => match cmd.clone() {
            DebugCommand::Send { hex } => {
                if hex.len() % 2 != 0 {
                    return Err("hex string must have even length".into());
                }
                let bytes: Vec<u8> = hex
                    .as_bytes()
                    .chunks(2)
                    .map(|c| {
                        let s = std::str::from_utf8(c).map_err(|_| "invalid UTF-8 in hex")?;
                        u8::from_str_radix(s, 16).map_err(|_| format!("invalid hex byte '{s}'"))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let resp = client
                    .queue()
                    .execute_raw(bytes, minidsp::queue::Priority::Normal)
                    .await?;
                let hex_str: String = resp.iter().map(|b| format!("{b:02x}")).collect();
                println!("{hex_str}");
            }
            DebugCommand::Id => {
                let hw = client.hw_id();
                println!(
                    "hw_id={}, fw={}.{}, dsp={}, serial={}",
                    hw.hw_id,
                    hw.fw_major,
                    hw.fw_minor,
                    client.dsp_version(),
                    client.serial()
                );
            }
            DebugCommand::DumpFloat { addr, count } => {
                let count = count.min(14);
                let values = client.read_floats(addr, count).await?;
                for (i, v) in values.iter().enumerate() {
                    println!("  [{:04X}+{}] = {v}", addr, i);
                }
            }
            DebugCommand::DumpWord { addr, count } => {
                let count = count.min(14);
                let values = client.read_param_words(addr, count).await?;
                for (i, v) in values.iter().enumerate() {
                    println!("  [{:04X}+{}] = {} (0x{:08X})", addr, i, v, v);
                }
            }
            DebugCommand::DumpFlash { addr, count } => {
                let count = count.min(minidsp_protocol::commands::FLASH_FULL_ADDR_CHUNK_MAX);
                let data = client.read_flash_full_addr(addr, count).await?;
                for (i, chunk) in data.chunks(16).enumerate() {
                    let line_addr = addr + (i as u32 * 16);
                    let bytes = chunk
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<Vec<_>>()
                        .join(" ");
                    println!("{line_addr:06x}: {bytes}");
                }
            }
        },
        Some(Command::Probe { .. }) | Some(Command::Server { .. }) => unreachable!(),
    }
    Ok(())
}

pub(crate) async fn connect_all_local(
    cli: &Cli,
) -> Result<Vec<MiniDspClient>, Box<dyn std::error::Error>> {
    if has_non_local_transport(cli) {
        return Err(
            "--all-local-devices only supports local USB discovery in this implementation".into(),
        );
    }

    let devices = minidsp::transport::hid::enumerate()?;
    let mut clients = Vec::new();
    for dev in devices {
        let transport = HidTransport::open(&dev.path)?;
        clients.push(MiniDspClient::connect(Box::new(transport)).await?);
    }
    Ok(clients)
}

pub(crate) async fn connect(cli: &Cli) -> Result<MiniDspClient, Box<dyn std::error::Error>> {
    // Environment variable overrides (minidsp-rs compatible)
    let env_url = std::env::var("MINIDSP_URL").ok();
    let env_daemon_url = std::env::var("MINIDSPD_URL").ok();
    let env_sock = std::env::var("MINIDSP_SOCK").ok();
    let env_tcp = std::env::var("MINIDSP_TCP").ok();
    let env_usb = std::env::var("MINIDSP_USB").ok();
    let env_index: Option<usize> = std::env::var("MINIDSP_INDEX")
        .ok()
        .and_then(|s| s.parse().ok());

    // Connection order per ref-compatibility.md:
    // 1. --daemon-sock  2. --tcp  3. --url  4. --daemon-url  5. --usb
    // 6. Default: try /tmp/minidsp.sock, then USB HID

    // Resolve device index from CLI flag or env var (used by WS and USB paths)
    let device_idx = cli.device_index.or(env_index).unwrap_or(0);

    // 1. Explicit daemon socket (or MINIDSP_DAEMON_SOCK env var) — WebSocket over Unix
    if let Some(path) = cli.daemon_sock.as_deref().or(env_sock.as_deref()) {
        #[cfg(unix)]
        {
            let devices = discover_daemon_unix(path).await?;
            if device_idx >= devices.len() {
                return Err(format!(
                    "device index {device_idx} out of range (found {} device{})",
                    devices.len(),
                    if devices.len() == 1 { "" } else { "s" }
                )
                .into());
            }
            let transport = WsTransport::connect_unix(path, device_idx).await?;
            return Ok(MiniDspClient::connect(Box::new(transport)).await?);
        }
        #[cfg(not(unix))]
        return Err("--daemon-sock (Unix socket) is not supported on this platform".into());
    }

    // 2. Explicit TCP (or MINIDSP_TCP env var)
    if let Some(addr) = cli.tcp.as_deref().or(env_tcp.as_deref()) {
        let transport = TcpTransport::connect(addr).await?;
        return Ok(MiniDspClient::connect(Box::new(transport)).await?);
    }

    // 3. --url flag or MINIDSP_URL env var (tcp://host:port, usb://path, usb:path)
    let url_source = cli.url.as_deref().or(env_url.as_deref());
    if let Some(url) = url_source {
        if url.starts_with("tcp://") {
            let addr = &url["tcp://".len()..];
            let transport = TcpTransport::connect(addr).await?;
            return Ok(MiniDspClient::connect(Box::new(transport)).await?);
        } else if url.starts_with("ws://") || url.starts_with("http://") {
            if !is_direct_ws_endpoint(url) {
                let devices = discover_daemon_tcp(url).await?;
                if device_idx >= devices.len() {
                    return Err(format!(
                        "device index {device_idx} out of range (found {} device{})",
                        devices.len(),
                        if devices.len() == 1 { "" } else { "s" }
                    )
                    .into());
                }
            }
            let transport = WsTransport::connect(url, device_idx).await?;
            return Ok(MiniDspClient::connect(Box::new(transport)).await?);
        } else if url.starts_with("usb://") {
            let path = parse_usb_url_path(&url["usb://".len()..])?;
            let transport = HidTransport::open(&path)?;
            return Ok(MiniDspClient::connect(Box::new(transport)).await?);
        } else if url.starts_with("usb:") {
            let path = parse_usb_url_path(&url["usb:".len()..])?;
            let transport = HidTransport::open(&path)?;
            return Ok(MiniDspClient::connect(Box::new(transport)).await?);
        } else {
            return Err(format!("unsupported URL scheme in '{url}' (expected tcp://, ws://, http://, usb://, or usb:)").into());
        }
    }

    // 4. Daemon URL (or MINIDSPD_URL env var) — connects via WebSocket to /devices/{id}/ws
    if let Some(url) = cli.daemon_url.as_deref().or(env_daemon_url.as_deref()) {
        let devices = discover_daemon_tcp(url).await?;
        if device_idx >= devices.len() {
            return Err(format!(
                "device index {device_idx} out of range (found {} device{})",
                devices.len(),
                if devices.len() == 1 { "" } else { "s" }
            )
            .into());
        }
        let transport = WsTransport::connect(url, device_idx).await?;
        return Ok(MiniDspClient::connect(Box::new(transport)).await?);
    }

    // 5. Explicit USB (or MINIDSP_USB env var)
    if let Some(path) = cli.usb.as_deref().or(env_usb.as_deref()) {
        let transport = open_usb_selector(path, device_idx)?;
        return Ok(MiniDspClient::connect(Box::new(transport)).await?);
    }

    // 6. Default: try Unix socket (WebSocket) first, then USB HID
    #[cfg(unix)]
    {
        let sock_path = "/tmp/minidsp.sock";
        if std::path::Path::new(sock_path).exists() {
            match WsTransport::connect_unix(sock_path, device_idx).await {
                Ok(transport) => {
                    return Ok(MiniDspClient::connect(Box::new(transport)).await?);
                }
                Err(_) => {
                    tracing::debug!("Unix socket at {sock_path} not responding, trying USB");
                }
            }
        }
    }

    // Use --device-index or MINIDSP_INDEX env var to select a specific USB device
    if let Some(idx) = cli.device_index.or(env_index) {
        let devices = minidsp::transport::hid::enumerate()?;
        let dev = devices.get(idx).ok_or_else(|| {
            format!(
                "device index {idx} out of range (found {} device{})",
                devices.len(),
                if devices.len() == 1 { "" } else { "s" },
            )
        })?;
        let transport = HidTransport::open(&dev.path)?;
        Ok(MiniDspClient::connect(Box::new(transport)).await?)
    } else {
        match HidTransport::open_first() {
            Ok(transport) => Ok(MiniDspClient::connect(Box::new(transport)).await?),
            Err(e) => Err(format!("no MiniDSP device found: {e}").into()),
        }
    }
}

fn parse_usb_url_path(url: &str) -> Result<String, Box<dyn std::error::Error>> {
    let path = url.split('?').next().unwrap_or(url);
    Ok(urlencoding::decode(path)?.into_owned())
}

fn parse_usb_vid_pid(selector: &str) -> Option<(u16, Option<u16>)> {
    let parts: Vec<_> = selector.split(':').collect();
    if parts.is_empty() || parts.len() > 2 {
        return None;
    }
    let vid = u16::from_str_radix(parts[0], 16).ok()?;
    let pid = if parts.len() == 2 {
        Some(u16::from_str_radix(parts[1], 16).ok()?)
    } else {
        None
    };
    Some((vid, pid))
}

fn open_usb_selector(
    selector: &str,
    device_idx: usize,
) -> Result<HidTransport, Box<dyn std::error::Error>> {
    if let Some((vid, pid)) = parse_usb_vid_pid(selector) {
        let matching: Vec<_> = minidsp::transport::hid::enumerate()?
            .into_iter()
            .filter(|dev| dev.vendor_id == vid && pid.is_none_or(|p| dev.product_id == p))
            .collect();
        let dev = matching.get(device_idx).ok_or_else(|| {
            format!(
                "device index {device_idx} out of range for usb selector {selector} (found {} device{})",
                matching.len(),
                if matching.len() == 1 { "" } else { "s" }
            )
        })?;
        return Ok(HidTransport::open(&dev.path)?);
    }

    Ok(HidTransport::open(selector)?)
}

fn is_direct_ws_endpoint(url: &str) -> bool {
    (url.starts_with("ws://") || url.starts_with("http://"))
        && url.contains("/devices/")
        && url.ends_with("/ws")
}

fn has_non_local_transport(cli: &Cli) -> bool {
    cli.daemon_sock.is_some()
        || cli.daemon_url.is_some()
        || cli.tcp.is_some()
        || cli
            .url
            .as_deref()
            .is_some_and(|u| !u.starts_with("usb://") && !u.starts_with("usb:"))
}

fn normalize_http_addr(base: &str) -> String {
    let stripped = base
        .trim_start_matches("http://")
        .trim_start_matches("ws://");
    let host_port = stripped.split('/').next().unwrap_or(stripped);
    if host_port.contains(':') {
        host_port.to_string()
    } else {
        format!("{host_port}:5380")
    }
}

pub(crate) async fn discover_daemon_tcp(
    base: &str,
) -> Result<Vec<DeviceJson>, Box<dyn std::error::Error>> {
    let addr = normalize_http_addr(base);
    let mut stream = tokio::net::TcpStream::connect(&addr).await?;
    let request = format!("GET /devices HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await?;
    parse_device_list_response(&buf)
}

#[cfg(unix)]
pub(crate) async fn discover_daemon_unix(
    path: &str,
) -> Result<Vec<DeviceJson>, Box<dyn std::error::Error>> {
    let mut stream = tokio::net::UnixStream::connect(path).await?;
    let request = "GET /devices HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
    stream.write_all(request.as_bytes()).await?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await?;
    parse_device_list_response(&buf)
}

fn parse_device_list_response(data: &[u8]) -> Result<Vec<DeviceJson>, Box<dyn std::error::Error>> {
    let text = std::str::from_utf8(data)?;
    let (head, body) = text.split_once("\r\n\r\n").ok_or("invalid HTTP response")?;
    if !head.starts_with("HTTP/1.1 200") && !head.starts_with("HTTP/1.0 200") {
        return Err(format!(
            "unexpected HTTP response: {}",
            head.lines().next().unwrap_or(head)
        )
        .into());
    }
    let devices: Vec<DeviceJson> = serde_json::from_str(body)?;
    Ok(devices)
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct NetworkProbeDevice {
    hostname: String,
    ip: String,
    hw_id: u8,
    dsp_version: u8,
    serial: u32,
}

pub(crate) fn discover_network_devices(
    timeout: std::time::Duration,
) -> Result<Vec<NetworkProbeDevice>, Box<dyn std::error::Error>> {
    use std::collections::BTreeMap;
    use std::net::{Ipv4Addr, UdpSocket};

    let debug = std::env::var_os("MINIDSP_DEBUG_DISCOVERY").is_some();
    let socket = UdpSocket::bind("0.0.0.0:3999")?;
    socket.join_multicast_v4(&Ipv4Addr::new(224, 0, 0, 250), &Ipv4Addr::UNSPECIFIED)?;
    socket.set_read_timeout(Some(std::time::Duration::from_millis(500)))?;

    let deadline = std::time::Instant::now() + timeout;
    let mut seen = BTreeMap::<(String, u8, u8, u32), NetworkProbeDevice>::new();
    let mut buf = [0u8; 512];

    while std::time::Instant::now() < deadline {
        match socket.recv_from(&mut buf) {
            Ok((n, addr)) => {
                if debug {
                    eprintln!("discovery recv: {addr} {n} bytes");
                }
                if let Some(pkt) = minidsp_protocol::discovery::DiscoveryPacket::decode(&buf[..n]) {
                    let ip = format!("{}.{}.{}.{}", pkt.ip[0], pkt.ip[1], pkt.ip[2], pkt.ip[3]);
                    let serial = 900000 + pkt.serial as u32;
                    if debug {
                        eprintln!(
                            "discovery decode: host={} ip={} hw_id={} dsp={} serial={}",
                            pkt.hostname, ip, pkt.hw_id, pkt.dsp_id, serial
                        );
                    }
                    let dev = NetworkProbeDevice {
                        hostname: pkt.hostname.clone(),
                        ip: ip.clone(),
                        hw_id: pkt.hw_id,
                        dsp_version: pkt.dsp_id,
                        serial,
                    };
                    seen.insert((ip, pkt.hw_id, pkt.dsp_id, serial), dev);
                } else if debug {
                    eprintln!("discovery decode: dropped invalid packet");
                }
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => return Err(e.into()),
        }
    }

    Ok(seen.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_ws_endpoint_detection() {
        assert!(is_direct_ws_endpoint("ws://localhost:5380/devices/0/ws"));
        assert!(is_direct_ws_endpoint("http://localhost:5380/devices/1/ws"));
        assert!(!is_direct_ws_endpoint("http://localhost:5380"));
    }

    #[test]
    fn http_addr_normalization() {
        assert_eq!(
            normalize_http_addr("http://localhost:5380"),
            "localhost:5380"
        );
        assert_eq!(normalize_http_addr("ws://example.com"), "example.com:5380");
        assert_eq!(normalize_http_addr("example.com:1234"), "example.com:1234");
    }

    #[test]
    fn parse_devices_http_response() {
        let resp = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n[{\"url\":\"usb:%2Fdev%2Fhidraw3?vid=2752&pid=0011\",\"version\":{\"hw_id\":10,\"fw_major\":1,\"fw_minor\":13,\"dsp_version\":100,\"serial\":912345},\"product_name\":\"2x4HD\"}]";
        let devices = parse_device_list_response(resp).unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].product_name, "2x4HD");
        assert_eq!(devices[0].version.hw_id, 10);
    }

    #[test]
    fn parse_usb_vid_pid_selectors() {
        assert_eq!(parse_usb_vid_pid("2752"), Some((0x2752, None)));
        assert_eq!(parse_usb_vid_pid("2752:0011"), Some((0x2752, Some(0x0011))));
        assert_eq!(parse_usb_vid_pid("DevSrvsID%3A1234567890"), None);
        assert_eq!(parse_usb_vid_pid("/dev/hidraw3"), None);
    }

    #[test]
    fn parse_u32_auto_supports_decimal_and_hex() {
        assert_eq!(parse_u32_auto("1234"), Ok(1234));
        assert_eq!(parse_u32_auto("0xffc8"), Ok(0xffc8));
        assert!(parse_u32_auto("0xzz").is_err());
    }
}
